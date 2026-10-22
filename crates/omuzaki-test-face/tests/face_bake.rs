//! The face baker (#172, #173, `omuzaki_face::bake`) on a synthetic head: every `ARKit` channel and VRM preset becomes an expression, the
//! morph targets move only the face, the atlases stay inside their cap, the bake is deterministic, and a baked scene
//! exports through `omuzaki-io-mock`'s exporter with no errors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns). The casts
// are pixel and grid counts, far inside f32's exact range.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use image::{Rgba, RgbaImage};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face::bake::FaceBaker;
use omuzaki_face::bake::flipbook::MAX_ATLAS_PX;
use omuzaki_face_api::{BakerInput, face_baker};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_io_api::{
    ARKIT_52, BakeReport, BakeRequest, ExportCtx, ExportOptions, Expression, ExpressionBaker, ExpressionName,
    ImageMime, ImportCtx, ImportInput, ImportOptions, IoScene, NoProgress, PresetExpression, Realisation, Severity,
    run_export, run_import,
};
use omuzaki_io_mock::fixtures::{BODY_NODE, tiny_avatar};
use omuzaki_io_mock::{MockVrm1Exporter, MockVrm1Importer};

/// Where the synthetic head sits in bind space.
const CENTRE: Vec3 = Vec3::new(0.0, 1.5, 0.0);
/// Its half-sizes: wide and tall enough to hold the mock projection's face window (0.4 x 0.29 m).
const RADII: Vec3 = Vec3::new(0.26, 0.34, 0.24);

/// An ellipsoid head around [`CENTRE`]: `rings` x `segments` quads, outward normals, skinned to the first joint.
fn head(rings: u32, segments: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let polar = std::f32::consts::PI * ring as f32 / rings as f32;
        for segment in 0..=segments {
            let azimuth = std::f32::consts::TAU * segment as f32 / segments as f32;
            // +z is the face's front.
            let unit = Vec3::new(polar.sin() * azimuth.sin(), polar.cos(), polar.sin() * azimuth.cos());
            let local = unit * RADII;
            mesh.positions.push(CENTRE + local);
            mesh.normals.push((local / (RADII * RADII)).normalize_or_zero());
            mesh.uvs.push(Vec2::new(segment as f32 / segments as f32, ring as f32 / rings as f32));
        }
    }
    let row = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b) = (ring * row + segment, (ring + 1) * row + segment);
            mesh.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    mesh.joints = vec![[0; 4]; mesh.positions.len()];
    mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; mesh.positions.len()];
    mesh
}

/// The fixture avatar with its body mesh swapped for the synthetic head, and its own expressions and morph targets
/// cleared so the baker owns every channel.
fn scene() -> IoScene {
    let mut scene = tiny_avatar();
    scene.avatar.as_mut().unwrap().expressions.clear();
    let mesh = &mut scene.meshes[0];
    mesh.primitives[0].mesh = head(48, 64);
    mesh.primitives[0].morph_targets.clear();
    mesh.morph_target_names.clear();
    scene
}

fn baker() -> FaceBaker {
    FaceBaker::new(BakerInput {
        profile: &mock_profile(),
        projection: mock_projection(),
        head_centre: CENTRE,
        scale: 1.0,
    })
}

fn arkit() -> Vec<ExpressionName> {
    ARKIT_52.iter().map(|n| ExpressionName::parse(n).unwrap()).collect()
}

fn bake(scene: &mut IoScene) -> BakeReport {
    baker().bake(scene, &BakeRequest::new(arkit(), BODY_NODE), &NoProgress).expect("the bake succeeds")
}

fn realisation(report: &BakeReport, channel: &str) -> Realisation {
    report.channels.iter().find(|c| c.channel == channel).map(|c| c.realisation.clone()).expect(channel)
}

#[test]
fn every_arkit_channel_and_preset_becomes_an_expression_and_the_face_channels_move() {
    let mut scene = scene();
    let report = bake(&mut scene);
    assert_eq!(report.channels.len(), 52);
    let expressions = &scene.avatar.as_ref().unwrap().expressions;
    for name in arkit() {
        assert!(expressions.contains_key(&name), "{name:?} has an expression");
    }
    for preset in PresetExpression::ALL {
        assert!(expressions.contains_key(&ExpressionName::Preset(preset)), "{preset:?} has an expression");
    }
    // The painted channels flip an atlas, the shape channels move the surface.
    for channel in ["eyeBlinkLeft", "eyeBlinkRight", "eyeWideLeft", "browDownLeft", "mouthSmileLeft", "mouthFrownRight"]
    {
        assert!(
            matches!(realisation(&report, channel), Realisation::Flipbook { .. } | Realisation::Mixed),
            "{channel}: {:?}",
            realisation(&report, channel)
        );
    }
    for channel in ["jawLeft", "jawForward", "cheekPuff"] {
        assert_eq!(realisation(&report, channel), Realisation::Morph { targets: 1 }, "{channel}");
    }
    assert!(!matches!(realisation(&report, "jawOpen"), Realisation::Inert { .. }), "the jaw opens");
    // Only gaze stays inert: the painted eyes turn through the file's look-at block, not through cells.
    let inert: Vec<&str> = report
        .channels
        .iter()
        .filter(|c| matches!(c.realisation, Realisation::Inert { .. }))
        .map(|c| c.channel.as_str())
        .collect();
    assert!(inert.iter().all(|c| c.starts_with("eyeLook")) && inert.len() == 8, "inert: {inert:?}");
    // A flipbook expression is binary, so the channels' UV moves add up instead of blending.
    let blink = &expressions[&ExpressionName::parse("eyeBlinkLeft").unwrap()];
    assert!(blink.is_binary && !blink.uv_binds.is_empty());
}

#[test]
fn the_morph_targets_move_only_the_face() {
    let mut scene = scene();
    bake(&mut scene);
    let mesh = &scene.meshes[0];
    let head = &mesh.primitives[0];
    assert_eq!(head.morph_targets.len(), mesh.morph_target_names.len());
    let jaw = mesh.morph_target_names.iter().position(|n| n == "jawOpen").expect("a jawOpen target");
    let moved = head.morph_targets[jaw].positions.iter().filter(|d| **d != Vec3::ZERO).count();
    assert!(moved > 20, "the jaw moves {moved} vertices");
    for (at, target) in head.morph_targets.iter().enumerate() {
        for (p, d) in head.mesh.positions.iter().zip(&target.positions) {
            let local = *p - CENTRE;
            assert!(
                local.z > 0.0 || *d == Vec3::ZERO,
                "{} moves {local} behind the face's front",
                mesh.morph_target_names[at]
            );
        }
    }
    // Every primitive (head and patches) carries every target, so the patches move with the skin.
    for primitive in &mesh.primitives {
        assert_eq!(primitive.morph_targets.len(), mesh.morph_target_names.len());
    }
}

#[test]
fn the_atlases_are_pngs_inside_the_cap() {
    let mut scene = scene();
    bake(&mut scene);
    let atlases: Vec<_> = scene.images.iter().filter(|i| i.name.starts_with("face.")).collect();
    assert!(atlases.len() >= 6, "{} face atlases", atlases.len());
    for image in atlases {
        assert_eq!(image.mime, ImageMime::Png);
        let bytes = image.bytes.as_slice();
        let side = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        let (width, height) = (side(16), side(20));
        assert!(width <= MAX_ATLAS_PX && height <= MAX_ATLAS_PX, "{}: {width} x {height}", image.name);
    }
}

#[test]
fn the_bake_is_deterministic() {
    let (mut a, mut b) = (scene(), scene());
    assert_eq!(bake(&mut a), bake(&mut b));
    assert!(a == b, "the same face, scene and request bake the same scene, bytes included");
}

#[test]
fn the_registry_finds_the_real_baker_and_its_bake_exports() {
    omuzaki_face::link();
    let factory = face_baker().expect("no priority conflict").expect("a face baker is registered");
    assert_eq!(factory.priority(), 10, "the real baker outranks the mock");
    let profile = mock_profile();
    let baker = factory
        .create(BakerInput { profile: &profile, projection: mock_projection(), head_centre: CENTRE, scale: 1.0 })
        .expect("the baker");
    let mut scene = scene();
    let report = baker.bake(&mut scene, &BakeRequest::new(arkit(), BODY_NODE), &NoProgress).expect("bake");
    let options = ExportOptions::default();
    let out = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&options)).expect("export");
    assert_eq!(out.report.count(Severity::Error), 0, "{:#?}", out.report);
    let inert = report.channels.iter().filter(|c| matches!(c.realisation, Realisation::Inert { .. })).count();
    assert!(out.report.count(Severity::Warning) >= inert, "each inert channel is at least one warning");

    // The file reads back with the targets and the expressions. The mock writes morph binds only (it reports the
    // atlas moves and colour fades it drops); the real VRM writer's round trip of those is the export's own test.
    let import_options = ImportOptions::default();
    let back = run_import(&MockVrm1Importer, &ImportInput::new(&out.files[0].bytes), &ImportCtx::new(&import_options))
        .expect("the baked file imports")
        .scene;
    assert_eq!(back.meshes[0].morph_target_names, scene.meshes[0].morph_target_names);
    assert_eq!(back.meshes[0].primitives.len(), scene.meshes[0].primitives.len(), "the head and its patches");
    let jaw = back.meshes[0].morph_target_names.iter().position(|n| n == "jawOpen").unwrap();
    assert!(back.meshes[0].primitives[0].morph_targets[jaw].positions.iter().any(|d| d.length() > 1e-4));
    let (before, after) = (&scene.avatar.as_ref().unwrap().expressions, &back.avatar.as_ref().unwrap().expressions);
    assert_eq!(after.len(), before.len());
    let jaw_open = ExpressionName::parse("jawOpen").unwrap();
    assert_eq!(after[&jaw_open].morph_binds, before[&jaw_open].morph_binds, "the jaw's bind survives");
}

/// What a viewer of the baked file shows on the face plane at 500 px/m when `expression` is at full weight (`None`:
/// at rest): each patch's atlas, moved by the expression's UV binds and faded by its colour binds, composited over the
/// skin in the patches' order. It reads only the scene, as an importer would.
fn shown(scene: &IoScene, expression: Option<&Expression>) -> RgbaImage {
    const PX_PER_M: f32 = 500.0;
    let rect = mock_projection().rect;
    let size = |extent: f32| (extent * PX_PER_M).round() as u32;
    let mut canvas = RgbaImage::from_pixel(size(rect.width()), size(rect.height()), Rgba([255, 224, 210, 255]));
    for primitive in &scene.meshes[0].primitives[1..] {
        let material = primitive.material.unwrap();
        let texture = scene.materials[material.index()].base_color_texture.as_ref().unwrap().texture;
        let image = &scene.images[scene.textures[texture.index()].image.index()];
        let atlas = image::load_from_memory(image.bytes.as_slice()).unwrap().to_rgba8();
        let offset: Vec2 = expression
            .map(|e| e.uv_binds.iter().filter(|b| b.material == material).map(|b| b.offset).sum())
            .unwrap_or_default();
        let alpha = expression
            .and_then(|e| e.color_binds.iter().find(|b| b.material == material))
            .map_or(scene.materials[material.index()].base_color.w, |b| b.target.w);
        // The patch's UVs are linear in the face plane: fit u to x and v to y from the extreme corners.
        let data = &primitive.mesh;
        let corners: Vec<(Vec2, Vec2)> =
            data.positions.iter().zip(&data.uvs).map(|(p, uv)| ((*p - CENTRE).truncate(), *uv)).collect();
        let extreme = |key: fn(&(Vec2, Vec2)) -> f32| {
            let min = corners.iter().min_by(|a, b| key(a).total_cmp(&key(b))).unwrap();
            let max = corners.iter().max_by(|a, b| key(a).total_cmp(&key(b))).unwrap();
            (*min, *max)
        };
        let ((xa, xb), (ya, yb)) = (extreme(|c| c.0.x), extreme(|c| c.0.y));
        let (width, height) = (atlas.width() as f32, atlas.height() as f32);
        for (i, j, pixel) in canvas.enumerate_pixels_mut() {
            let q = Vec2::new(rect.x0 + (i as f32 + 0.5) / PX_PER_M, rect.y1 - (j as f32 + 0.5) / PX_PER_M);
            if q.x < xa.0.x || q.x > xb.0.x || q.y < ya.0.y || q.y > yb.0.y {
                continue;
            }
            let u = xa.1.x + (q.x - xa.0.x) * (xb.1.x - xa.1.x) / (xb.0.x - xa.0.x) + offset.x;
            let v = ya.1.y + (q.y - ya.0.y) * (yb.1.y - ya.1.y) / (yb.0.y - ya.0.y) + offset.y;
            let (x, y) = ((u * width) as u32, (v * height) as u32);
            let Some(src) = atlas.get_pixel_checked(x, y) else { continue };
            let a = f32::from(src[3]) / 255.0 * alpha;
            for c in 0..3 {
                pixel[c] = (f32::from(src[c]) * a + f32::from(pixel[c]) * (1.0 - a)).round() as u8;
            }
        }
    }
    canvas
}

fn mean_difference(a: &RgbaImage, b: &RgbaImage) -> f32 {
    let total: u64 = a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f32 / a.as_raw().len() as f32
}

#[test]
fn blink_aa_and_happy_show_different_faces_from_the_file_alone() {
    let mut scene = scene();
    bake(&mut scene);
    let expressions = &scene.avatar.as_ref().unwrap().expressions;
    let preset = |p| &expressions[&ExpressionName::Preset(p)];
    let faces = [
        ("rest", shown(&scene, None)),
        ("blink", shown(&scene, Some(preset(PresetExpression::Blink)))),
        ("aa", shown(&scene, Some(preset(PresetExpression::Aa)))),
        ("happy", shown(&scene, Some(preset(PresetExpression::Happy)))),
    ];
    for (at, (a, face_a)) in faces.iter().enumerate() {
        for (b, face_b) in &faces[at + 1..] {
            let d = mean_difference(face_a, face_b);
            assert!(d > 0.2, "{a} and {b} look the same from the file (mean difference {d})");
        }
    }
    if let Some(dir) = std::env::var_os("OMUZAKI_BAKE_EVIDENCE") {
        let (w, h) = faces[0].1.dimensions();
        let mut sheet = RgbaImage::new(w * faces.len() as u32, h);
        for (at, (_, face)) in faces.iter().enumerate() {
            image::imageops::replace(&mut sheet, face, i64::from(w) * at as i64, 0);
        }
        sheet.save(std::path::Path::new(&dir).join("bake-presets.png")).unwrap();
    }
}

/// A face's own named expression (#771) is requested as `omuzaki_<id>` and baked from its channel deltas, as a preset
/// is: `smug` (a one-sided smile, a lowered brow and the jaw pushed aside) binds the jaw's morph at its weight and flips
/// the mouth and brow atlases, and the baked scene exports and reads back with the expression.
#[test]
fn a_named_expression_is_baked_from_its_deltas() {
    use omuzaki_anim_api::{Channel, ExpressionId};
    use omuzaki_face_api::{ChannelValues, Expression as FaceExpression, ExpressionKind, IntensityCurve, Playback};

    let mut profile = mock_profile();
    let deltas = ChannelValues::default()
        .with(Channel::MOUTH_SMILE_LEFT, 1.0)
        .with(Channel::BROW_DOWN_RIGHT, 0.75)
        .with(Channel::JAW_LEFT, 0.5);
    let smug = FaceExpression {
        kind: ExpressionKind::Emotion,
        deltas,
        curve: IntensityCurve::Linear,
        playback: Playback::default(),
    };
    profile.expressions.0.insert(ExpressionId::new("smug").unwrap(), smug);
    let baker = FaceBaker::new(BakerInput {
        profile: &profile,
        projection: mock_projection(),
        head_centre: CENTRE,
        scale: 1.0,
    });
    let name = ExpressionName::parse("omuzaki_smug").unwrap();
    let mut channels = arkit();
    channels.push(name.clone());
    let mut scene = scene();
    let report = baker.bake(&mut scene, &BakeRequest::new(channels, BODY_NODE), &NoProgress).unwrap();
    assert_eq!(realisation(&report, "omuzaki_smug"), Realisation::Mixed, "a morph and atlas flips together");

    let avatar = scene.avatar.as_ref().unwrap();
    let expression = &avatar.expressions[&name];
    let jaw = avatar.expressions[&ExpressionName::parse("jawLeft").unwrap()].morph_binds[0];
    assert!(
        expression.morph_binds.iter().any(|b| b.index == jaw.index && (b.weight - 0.5).abs() < 1e-6),
        "the jaw's morph at the expression's weight: {:?}",
        expression.morph_binds
    );
    assert!(!expression.uv_binds.is_empty() && expression.is_binary, "the smile and the brow flip their atlases");

    let file = run_export(&MockVrm1Exporter, &scene, &ExportCtx::new(&ExportOptions::default())).unwrap();
    let back = run_import(
        &MockVrm1Importer,
        &ImportInput::new(&file.files[0].bytes),
        &ImportCtx::new(&ImportOptions::default()),
    )
    .unwrap()
    .scene;
    assert!(back.avatar.unwrap().expressions.contains_key(&name), "the file carries omuzaki_smug");
}
