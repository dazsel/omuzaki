//! End-to-end checks of the loader and the shader, against colours worked out here in Rust.
//!
//! The GPU tests skip (with a message) when the machine has no adapter, so CI without a GPU stays green; on any
//! adapter, software ones included, they check the same numbers.

use glam::Vec3;
use toon_viewport::camera::OrbitCamera;
use toon_viewport::gpu::open_headless;
use toon_viewport::lighting::Lighting;
use toon_viewport::model::{Hooks, Kind, Model};
use toon_viewport::renderer::{FrameParams, RenderOptions, Renderer};

/// A glTF with one big triangle facing +Z through the origin plane, one material and the toon extension, buffers
/// embedded. `material` is the JSON of the material, `extras` the JSON of the node extras.
fn triangle_gltf(material: &str, extras: &str) -> String {
    // positions (-5,-5,0) (5,-5,0) (0,10,0); normals +Z; 3 x 12 bytes each, little endian f32.
    let floats: [f32; 18] =
        [-5.0, -5.0, 0.0, 5.0, -5.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
    let b64 = {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    };
    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0, "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"mesh": 0, "name": "tri", "extras": {extras}}}],
  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0, "NORMAL": 1}}, "material": 0}}]}}],
  "materials": [{material}],
  "buffers": [{{"byteLength": 72, "uri": "data:application/octet-stream;base64,{b64}"}}],
  "bufferViews": [{{"buffer": 0, "byteOffset": 0, "byteLength": 36}}, {{"buffer": 0, "byteOffset": 36, "byteLength": 36}}],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [-5,-5,0], "max": [5,10,0]}},
    {{"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}}
  ]
}}"#
    )
}

const TOON_MATERIAL: &str = r#"{
  "name": "test",
  "pbrMetallicRoughness": {"baseColorFactor": [0.5, 0.25, 0.1, 1.0]},
  "extensions": {"OMUZAKI_toon_spike": {
    "kind": "toon", "vertexColors": false, "shade": [0.5, 0.5, 0.5], "shift": -0.1, "soft": 0.1,
    "rim": [0, 0, 0], "rimPow": 3, "rimStr": 0, "hooks": ["skin_out"], "uniforms": {"uSkinFloor": 0.0}, "textures": {}
  }}
}"#;

const BASIC_MATERIAL: &str = r#"{
  "name": "flat",
  "pbrMetallicRoughness": {"baseColorFactor": [0.18, 0.18, 0.18, 1.0]},
  "extensions": {"OMUZAKI_toon_spike": {"kind": "basic", "vertexColors": false}}
}"#;

const EXTRAS: &str = r#"{"omuzaki": {"castShadow": true, "receiveShadow": true, "renderOrder": 0,
  "outline": {"color": [0.1, 0.2, 0.3], "width": 0.002}}}"#;

/// three.js `ACESFilmicToneMapping` then the sRGB OETF, to 8 bits: the oracle for the shader's output stage.
fn encode(linear: Vec3, exposure: f32) -> [u8; 3] {
    let scaled = linear * (exposure / 0.6);
    let to_rrt = glam::Mat3::from_cols(
        Vec3::new(0.59719, 0.07600, 0.02840),
        Vec3::new(0.35458, 0.90834, 0.13383),
        Vec3::new(0.04823, 0.01566, 0.83777),
    );
    let from_odt = glam::Mat3::from_cols(
        Vec3::new(1.60475, -0.10208, -0.00327),
        Vec3::new(-0.53108, 1.10813, -0.07276),
        Vec3::new(-0.07367, -0.00605, 1.07602),
    );
    let rrt = to_rrt * scaled;
    let numerator = rrt * (rrt + Vec3::splat(0.024_578_6)) - Vec3::splat(0.000_090_537);
    let denominator = rrt * (0.983_729 * rrt + Vec3::splat(0.432_951)) + Vec3::splat(0.238_081);
    let mapped = (from_odt * (numerator / denominator)).clamp(Vec3::ZERO, Vec3::ONE);
    let srgb = |x: f32| if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    // Clamped to 0..=255 before the cast, so it cannot truncate or wrap.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |x: f32| (srgb(x) * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(mapped.x), byte(mapped.y), byte(mapped.z)]
}

fn centre_pixel(model: &Model, lighting: &Lighting) -> Option<[u8; 3]> {
    let gpu = match open_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipping GPU test: {e}");
            return None;
        }
    };
    let mut renderer = Renderer::new(&gpu.device, &gpu.queue, model);
    let camera = OrbitCamera::front();
    let options = RenderOptions { outlines: false, shadows: false, ground: false };
    renderer.render((64, 64), &FrameParams { camera: &camera, lighting, options });
    let (pixels, (w, _)) = renderer.read_pixels().ok()?;
    let i = ((32 * w + 32) * 4) as usize;
    Some([pixels[i], pixels[i + 1], pixels[i + 2]])
}

fn assert_close(got: [u8; 3], want: [u8; 3]) {
    for (g, w) in got.iter().zip(want) {
        assert!(g.abs_diff(w) <= 2, "got {got:?}, want {want:?}");
    }
}

#[test]
fn loader_reads_materials_and_node_extras() {
    let model = Model::from_slice(triangle_gltf(TOON_MATERIAL, EXTRAS).as_bytes()).unwrap();
    assert_eq!(model.meshes.len(), 1);
    let (mesh, material) = (&model.meshes[0], &model.materials[0]);
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    assert!(mesh.cast_shadow && mesh.receive_shadow);
    assert!((mesh.outline.unwrap().width - 0.002).abs() < 1e-9);
    assert_eq!(material.kind, Kind::Toon);
    assert_eq!(material.hooks, Hooks(Hooks::SKIN_OUT));
    assert!((material.toon.shift + 0.1).abs() < 1e-6);
    assert_eq!(material.base_color, [0.5, 0.25, 0.1, 1.0]);
}

#[test]
fn loader_rejects_a_malformed_extension() {
    let bad = TOON_MATERIAL.replace("\"shift\": -0.1", "\"shift\": \"left\"");
    assert!(Model::from_slice(triangle_gltf(&bad, EXTRAS).as_bytes()).is_err());
}

#[test]
fn flat_material_comes_out_tone_mapped_and_encoded() {
    let model = Model::from_slice(triangle_gltf(BASIC_MATERIAL, EXTRAS).as_bytes()).unwrap();
    let Some(got) = centre_pixel(&model, &Lighting::studio()) else { return };
    assert_close(got, encode(Vec3::splat(0.18), 1.0));
}

#[test]
fn toon_material_sums_hemisphere_key_ramp_and_shaded_fill() {
    let model = Model::from_slice(triangle_gltf(TOON_MATERIAL, EXTRAS).as_bytes()).unwrap();
    let lighting = Lighting::studio();
    let Some(got) = centre_pixel(&model, &lighting) else { return };

    // Normal +Z. Hemisphere: halfway between ground and sky. Key faces it (ramp fully on); fill is behind it
    // (ramp fully off, so it contributes only its shade colour).
    let albedo = Vec3::new(0.5, 0.25, 0.1);
    let hemi = lighting.ground.lerp(lighting.sky, 0.5) * lighting.hemi_intensity;
    let key = lighting.lights[0].color * lighting.lights[0].intensity;
    let fill = lighting.lights[1].color * lighting.lights[1].intensity * 0.5;
    let want = (hemi + key + fill) * albedo / std::f32::consts::PI;
    assert_close(got, encode(want, 1.0));
}
