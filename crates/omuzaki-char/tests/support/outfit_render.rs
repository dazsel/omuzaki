//! The evidence pictures of the outfit tests: a built stack rendered from four sides through the mock toon renderer into
//! one binary PPM file, the views in a two-by-two grid. Included by the tests that make pictures (`#[path = "support/outfit_render.rs"] mod render;`), next to
//! the conformance suite as `conformance`.
//!
//! Every garment's pictures come through here, so they share one camera, one light rig and one image size: a reviewer can
//! lay the shirt's sheet beside the vest's and compare like with like.

#![allow(dead_code)] // each test binary uses the part it needs
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers: a panic is a clear failure
#![allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]

use std::collections::HashMap;
use std::path::Path;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
};
use omuzaki_render_mock::MockRenderer;

use super::conformance::Stack;

/// The four sides every picture shows, as the camera's turn about the vertical in degrees, in reading order: front and
/// three-quarter on the top row, side and back below.
pub const VIEWS: [f32; 4] = [0.0, 35.0, 90.0, 180.0];

/// The side of one square view, pixels; the picture is twice this each way.
const VIEW: u32 = 448;

/// Where the camera looks and how far back it stands.
#[derive(Clone, Copy, Debug)]
pub struct Framing {
    /// The point the camera looks at, metres.
    pub centre: Vec3,
    /// The camera's distance from `centre`, metres.
    pub distance: f32,
}

impl Framing {
    /// Frames the meshes of the named parts of `stack` (every part when `parts` is empty) so that they fill most of the
    /// picture from every side in [`VIEWS`].
    ///
    /// # Panics
    /// When the named parts have no vertices.
    pub fn parts(stack: &Stack, parts: &[&str]) -> Self {
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for (id, output) in &stack.outputs {
            if !parts.is_empty() && !parts.contains(&id.as_str()) {
                continue;
            }
            for p in output.meshes.iter().flat_map(|m| &m.geometry.positions) {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        assert!(lo.x <= hi.x, "no vertices in {parts:?}");
        // The default lens sees 30° vertically: half the picture's height at distance d is d·tan 15° ≈ 0.27·d. Fitting
        // the largest extent with a margin of a tenth, plus half the depth in front of the centre, keeps the parts whole
        // from any side.
        let size = (hi - lo).max_element();
        Self { centre: (lo + hi) / 2.0, distance: 2.05 * size + 0.5 * size }
    }

    /// The whole figure: the body and everything on it.
    pub fn figure(stack: &Stack) -> Self {
        Self::parts(stack, &[])
    }
}

/// Renders every mesh of `stack` with `framing` from each side in [`VIEWS`] into `<name>.ppm` in `directory`, the four
/// views in a two-by-two grid so one file shows the garment all the way round.
pub fn render(stack: &Stack, framing: Framing, name: &str, directory: &Path) {
    std::fs::create_dir_all(directory).unwrap();
    let side = 2 * VIEW;
    let mut rgb = vec![0_u8; (side * side * 3) as usize];
    for (n, azimuth_deg) in (0_u32..).zip(VIEWS) {
        let image = view(stack, framing, azimuth_deg);
        let (left, top) = ((n % 2) * VIEW, (n / 2) * VIEW);
        for y in 0..VIEW {
            for x in 0..VIEW {
                let [r, g, b, _] = image.pixel(x, y).unwrap();
                let at = (((top + y) * side + left + x) * 3) as usize;
                rgb[at..at + 3].copy_from_slice(&[r, g, b]);
            }
        }
    }
    let mut ppm = format!("P6\n{side} {side}\n255\n").into_bytes();
    ppm.extend(rgb);
    std::fs::write(directory.join(format!("{name}.ppm")), ppm).unwrap();
}

/// One view of `stack`: the camera turned `azimuth_deg` about the vertical from the front.
fn view(stack: &Stack, framing: Framing, azimuth_deg: f32) -> omuzaki_render_api::Image {
    let mut renderer = MockRenderer::new();
    let eye = framing.centre
        + Mat4::from_rotation_y(azimuth_deg.to_radians())
            .transform_vector3(Vec3::new(0.0, 0.08 * framing.distance, framing.distance));
    let mut scene = Scene::new(Camera::look_at(eye, framing.centre, Vec3::Y, Camera::default().projection));
    scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
        Light::Directional {
            to_light: Vec3::new(1.6, 3.0, 2.4),
            colour: Colour::hex(0x00ff_f6ec),
            intensity: 1.7,
            casts_shadow: false,
        },
    ];
    scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
    for (_, output) in &stack.outputs {
        let textures: HashMap<_, _> = output
            .textures
            .iter()
            .map(|(name, data)| (name.clone(), renderer.create_texture(&data.desc, Some(&data.rgba8)).unwrap()))
            .collect();
        for built in &output.meshes {
            let mesh = renderer.create_mesh(&built.geometry).unwrap();
            let (_, spec) = output.materials.iter().find(|(m, _)| *m == built.material).unwrap();
            // The face's overlays need the face rasteriser's pages, which these tests don't build.
            let mut spec = spec.clone();
            spec.overlays.clear();
            let material = spec.to_material(&|name| textures.get(name).copied(), &|_| None).unwrap();
            let material = renderer.create_material(&material).unwrap();
            scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        }
    }
    let quality = Quality { samples: 4, ..Quality::offline() };
    renderer.render_image(UVec2::splat(VIEW), AlphaEncoding::Straight, &scene, &quality).unwrap()
}
