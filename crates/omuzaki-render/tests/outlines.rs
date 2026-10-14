//! The Outlines pass on the software adapter: where the line goes, how wide it is at each distance, and that it agrees
//! with the mock renderer, which draws the same hull on the CPU with an exact rasteriser. The conformance suite
//! (`conformance.rs`) holds the contract's own outline check; these cover what is specific to the GPU implementation.
//!
//! Every picture is rendered twice, with the outline and without, and the *line* is the set of pixels that changed:
//! that isolates the hull from the surface colour, so the checks keep working when the Opaque pass learns to light.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
// Pixel counts, sizes and byte values are small and the checks tolerate rounding: the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Colour, FrameReport, Image, Instance, JointRange, Material, MeshData, MeshId,
    Outline, Pass, Projection, Quality, Renderer, RendererExt, Scene, TargetDesc, Tonemap,
};
use omuzaki_render_mock::{MockRenderer, shapes};

const SIZE: UVec2 = UVec2::new(128, 128);
const LINE: Colour = Colour::rgb(255, 0, 0);
const SURFACE: Colour = Colour::rgb(0, 255, 0);
/// The prototype's lens: 30 degrees vertical.
const FOV_Y_DEG: f32 = 30.0;

/// A camera on +Z looking at the origin from `distance` metres.
fn camera(distance: f32) -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 0.0, distance),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: FOV_Y_DEG, near: 0.05, far: 80.0 },
    )
}

/// Exact colours (no tone curve) on black.
fn scene(distance: f32) -> Scene {
    let mut s = Scene::new(camera(distance));
    s.background = Background::Solid { colour: Colour::BLACK };
    s.post.tonemap = Tonemap::None;
    s
}

/// One sample, no effects: pixels are exactly covered or not.
fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

fn draw(r: &mut dyn Renderer, scene: &Scene, quality: &Quality) -> Image {
    r.render_image(SIZE, AlphaEncoding::Straight, scene, quality).unwrap()
}

/// A sphere with smooth normals: `segments` around, `rings` from pole to pole, counter-clockwise from outside.
fn sphere(radius: f32, segments: u32, rings: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let polar = std::f32::consts::PI * ring as f32 / rings as f32;
        for segment in 0..=segments {
            let azimuth = std::f32::consts::TAU * segment as f32 / segments as f32;
            let normal = Vec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
            mesh.positions.push(normal * radius);
            mesh.normals.push(normal);
        }
    }
    let row = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let (a, b) = (ring * row + segment, (ring + 1) * row + segment);
            mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

/// One mesh on one renderer, drawn with an unlit material that has an outline or does not.
struct Subject {
    renderer: Box<dyn Renderer>,
    mesh: MeshId,
    /// Skin every instance with this palette range, if set.
    skin: Option<JointRange>,
}

/// A picture with the outline, the same picture without, and the pixels that differ: the line.
struct Line {
    with: Image,
    without: Image,
    mask: Vec<bool>,
}

impl Subject {
    fn new(mut renderer: Box<dyn Renderer>, mesh: &MeshData) -> Self {
        let mesh = renderer.create_mesh(mesh).unwrap();
        Self { renderer, mesh, skin: None }
    }

    fn picture(&mut self, scene: &Scene, transforms: &[Mat4], outline: Option<Outline>, quality: &Quality) -> Image {
        let mut material = Material::unlit(SURFACE);
        material.outline = outline;
        let material = self.renderer.create_material(&material).unwrap();
        let mut scene = scene.clone();
        scene.instances = transforms
            .iter()
            .map(|&t| {
                let mut instance = Instance::new(self.mesh, material, t);
                instance.skin = self.skin;
                instance
            })
            .collect();
        let image = draw(self.renderer.as_mut(), &scene, quality);
        self.renderer.release(material.into()).unwrap();
        image
    }

    /// Renders the mesh at each of `transforms` with `outline` and without it.
    fn line(&mut self, scene: &Scene, transforms: &[Mat4], outline: Option<Outline>, quality: &Quality) -> Line {
        let with = self.picture(scene, transforms, outline, quality);
        let without = self.picture(scene, transforms, None, quality);
        let mask = line_mask(&with, &without);
        Line { with, without, mask }
    }
}

/// A width in metres at 2 m, in the middle of what characters use (a body line, 2.8 mm, is the widest of the sample character's).
fn outline(width: f32) -> Outline {
    Outline { colour: LINE, width }
}

/// The pixels where `with` differs from `without`: the line.
fn line_mask(with: &Image, without: &Image) -> Vec<bool> {
    with.rgba8.as_chunks::<4>().0.iter().zip(without.rgba8.as_chunks::<4>().0).map(|(a, b)| a != b).collect()
}

fn count(mask: &[bool]) -> usize {
    mask.iter().filter(|&&on| on).count()
}

/// How many pixels the two masks disagree on.
fn disagreement(a: &[bool], b: &[bool]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// The line's pixel width along the middle row, to the right of the picture's centre.
fn ring_width_right_of_centre(mask: &[bool]) -> usize {
    let row = (SIZE.y / 2) as usize * SIZE.x as usize;
    mask[row + (SIZE.x / 2) as usize..row + SIZE.x as usize].iter().filter(|&&on| on).count()
}

fn gpu() -> Box<dyn Renderer> {
    Box::new(support::renderer())
}

fn mock() -> Box<dyn Renderer> {
    Box::new(MockRenderer::new())
}

// ---- width by distance ----

/// The hull of a sphere of radius `r` seen from `distance` is a sphere of `r + w`; its silhouette is the cone of
/// half-angle `asin(radius / distance)`, so the line's width on the middle row is the difference of the two radii in
/// pixels. `w` is the contract's own `world_width` at the depth of the silhouette.
fn expected_ring_width(radius: f32, width: f32, distance: f32) -> f32 {
    let focal = SIZE.y as f32 / 2.0 / (FOV_Y_DEG.to_radians() / 2.0).tan();
    let on_screen = |reach: f32| focal * (reach / distance).asin().tan();
    let tangent_depth = distance - radius * radius / distance;
    let pushed = Outline { colour: LINE, width }.world_width(tangent_depth);
    on_screen(radius + pushed) - on_screen(radius)
}

#[test]
fn the_line_is_as_wide_as_world_width_says_at_every_distance() {
    // 1 m is inside the lower clamp (0.6x), 2 m is exact, 4 m scales to 2x, 12 m is past the upper clamp (3x). Each
    // sphere subtends the same angle, so the pictures differ only in the line.
    for (distance, width) in [(1.0_f32, 0.03), (2.0, 0.03), (4.0, 0.03), (12.0, 0.08)] {
        let radius = 0.15 * distance;
        let mut subject = Subject::new(gpu(), &sphere(radius, 64, 32));
        let line = subject.line(&scene(distance), &[Mat4::IDENTITY], Some(outline(width)), &plain());
        let measured = ring_width_right_of_centre(&line.mask) as f32;
        let wanted = expected_ring_width(radius, width, distance);
        println!("distance {distance:>4} m: line {measured:.0} px, contract says {wanted:.2} px");
        assert!((measured - wanted).abs() <= 1.5, "at {distance} m the line is {measured} px, wanted {wanted:.2}");
    }
}

#[test]
fn the_line_keeps_a_steady_width_on_screen_between_the_clamps() {
    // The prototype's choice: the world width grows with distance so the line stays about as wide in pixels.
    let width_at = |distance: f32| {
        let mut subject = Subject::new(gpu(), &sphere(0.1 * distance, 64, 32));
        let line = subject.line(&scene(distance), &[Mat4::IDENTITY], Some(outline(0.03)), &plain());
        ring_width_right_of_centre(&line.mask)
    };
    let (near, middle, far) = (width_at(1.5), width_at(3.0), width_at(6.0));
    println!("line width at 1.5 m, 3 m, 6 m: {near}, {middle}, {far} px");
    assert!(near.abs_diff(middle) <= 2 && middle.abs_diff(far) <= 2, "{near}, {middle}, {far}");
    assert!(near > 1 && middle > 1 && far > 1);
}

// ---- agreement with the mock ----

#[test]
fn the_hull_agrees_with_the_mock_on_a_sphere_at_each_distance() {
    for (distance, width) in [(1.0_f32, 0.03), (2.0, 0.03), (4.0, 0.03), (12.0, 0.08)] {
        let mesh = sphere(0.15 * distance, 48, 24);
        let (mut a, mut b) = (Subject::new(gpu(), &mesh), Subject::new(mock(), &mesh));
        let s = scene(distance);
        let (gpu_line, mock_line) = (
            a.line(&s, &[Mat4::IDENTITY], Some(outline(width)), &plain()).mask,
            b.line(&s, &[Mat4::IDENTITY], Some(outline(width)), &plain()).mask,
        );
        let off = disagreement(&gpu_line, &mock_line);
        println!("distance {distance:>4} m: {} line pixels, GPU and mock disagree on {off}", count(&mock_line));
        assert!(count(&mock_line) > 100, "the scene must have a line to compare");
        assert!(off * 40 <= count(&mock_line), "{off} of {} line pixels differ", count(&mock_line));
    }
}

#[test]
fn skinned_and_mirrored_hulls_agree_with_the_mock() {
    let mut skinned = sphere(0.25, 32, 16);
    skinned.joints = vec![[0, 1, 0, 0]; skinned.positions.len()];
    // Blend a translation with a twist by height, so the hull's normals are skinned too, not just its positions.
    skinned.weights = skinned.positions.iter().map(|p| [0.5 + p.y, 0.5 - p.y, 0.0, 0.0]).collect();
    let mut s = scene(2.0);
    s.joints = vec![Mat4::from_translation(Vec3::new(0.4, 0.0, 0.0)), Mat4::from_rotation_z(0.5)];
    let mirrored = Mat4::from_translation(Vec3::new(-0.4, 0.0, 0.0)) * Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    for (name, transform) in [("skinned", Mat4::IDENTITY), ("mirrored", mirrored)] {
        let (mut a, mut b) = (Subject::new(gpu(), &skinned), Subject::new(mock(), &skinned));
        a.skin = Some(JointRange { start: 0, len: 2 });
        b.skin = a.skin;
        let (gpu_line, mock_line) = (
            a.line(&s, &[transform], Some(outline(0.02)), &plain()).mask,
            b.line(&s, &[transform], Some(outline(0.02)), &plain()).mask,
        );
        let off = disagreement(&gpu_line, &mock_line);
        println!("{name}: {} line pixels, GPU and mock disagree on {off}", count(&mock_line));
        assert!(count(&mock_line) > 100, "{name}: the scene must have a line to compare");
        assert!(off * 25 <= count(&mock_line), "{name}: {off} of {} line pixels differ", count(&mock_line));
    }
}

// ---- the hull's shape ----

/// A cube whose copies of each corner (three, one per face) are `jitter` metres apart instead of exactly equal, the way
/// an exporter that rounds through another unit leaves them.
fn cube_with_jittered_corners(half: f32, jitter: f32) -> MeshData {
    let mut cube = shapes::cuboid(Vec3::splat(half));
    for (i, position) in cube.positions.iter_mut().enumerate() {
        let sign = |bit: usize| if (i >> bit) & 1 == 0 { jitter } else { -jitter };
        *position += Vec3::new(sign(0), sign(1), sign(2));
    }
    cube
}

#[test]
fn a_hard_edged_cube_is_outlined_without_gaps_at_its_creases_even_when_its_corners_are_a_hair_apart() {
    // Seen along a diagonal, a cube's silhouette is a hexagon of six creases and six corners. Pushing each face along
    // its own normal would tear the hull open there; averaged normals keep it closed, so every pixel just outside the
    // cube's silhouette is line. The same holds when the copies of a corner differ by 1e-7 m (about an ulp), which an
    // exact comparison of positions would not weld.
    for jitter in [0.0, 1e-7] {
        let mut subject = Subject::new(gpu(), &cube_with_jittered_corners(0.25, jitter));
        let tilt = Mat4::from_rotation_x(0.6) * Mat4::from_rotation_y(0.7);
        let Line { without, mask: line, .. } = subject.line(&scene(2.0), &[tilt], Some(outline(0.03)), &plain());
        let surface: Vec<bool> = without.rgba8.as_chunks::<4>().0.iter().map(|p| *p == [0, 255, 0, 255]).collect();
        let (w, h) = (SIZE.x as usize, SIZE.y as usize);
        let mut ring = 0;
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                let touches_surface = [i - 1, i + 1, i - w, i + w].into_iter().any(|n| surface[n]);
                if !surface[i] && touches_surface {
                    ring += 1;
                    assert!(line[i], "a gap in the outline at pixel ({x}, {y}) with {jitter} m of jitter");
                }
            }
        }
        assert!(ring > 100, "the cube's silhouette is long: {ring}");
        // And the surface itself is untouched: the hull never draws over the front faces.
        assert!(surface.iter().zip(&line).all(|(&surface, &line)| !(surface && line)), "the line covers the cube");
    }
}

#[test]
fn a_line_hides_behind_a_nearer_surface_and_shows_over_a_farther_one() {
    // Two spheres side by side along the view, the near one off to the left. Where they overlap on screen, the near
    // sphere's line is drawn over the far sphere, and the far sphere's line is hidden behind the near sphere.
    let mut subject = Subject::new(gpu(), &sphere(0.3, 48, 24));
    let s = scene(3.0);
    let near = Mat4::from_translation(Vec3::new(-0.15, 0.0, 0.8));
    let far = Mat4::from_translation(Vec3::new(0.2, 0.0, -0.5));
    let Line { with, mask: line, .. } = subject.line(&s, &[far, near], Some(outline(0.02)), &plain());
    let (w, h) = (SIZE.x as usize, SIZE.y as usize);
    // Walking the middle row from left to right: the near sphere's line is the first run of line pixels, then its
    // surface, then (where the spheres overlap) its line again before the far sphere shows.
    let row: Vec<bool> = (0..w).map(|x| line[(h / 2) * w + x]).collect();
    let runs = row.windows(2).filter(|p| p[0] != p[1]).count();
    assert!(runs >= 4, "expected the near line, a gap for its surface, then more line: {runs} transitions");
    // The near sphere is whole: no pixel of it was overdrawn by either line.
    let surface_still_green = |x: usize| with.rgba8[((h / 2) * w + x) * 4..][..4] == [0, 255, 0, 255];
    assert!((0..w).filter(|&x| surface_still_green(x)).count() > 20);
    assert!(count(&line) > 100);
}

// ---- colour, light and alpha ----

fn linear_to_srgb_byte(linear: f64) -> u8 {
    let encoded = if linear <= 0.003_130_8 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0).round() as u8
}

#[test]
fn outline_light_scales_the_line_in_linear_light() {
    let mut subject = Subject::new(gpu(), &shapes::cuboid(Vec3::splat(0.4)));
    let mut s = scene(5.0);
    s.outline_light = 0.5;
    let with = subject.picture(&s, &[Mat4::IDENTITY], Some(outline(0.2)), &plain());
    // At 5 m a pixel is 2.1 cm: the cube's face ends 19 pixels right of the centre, and a line this wide runs to 33.
    let ring = with.pixel(SIZE.x / 2 + 25, SIZE.y / 2).unwrap();
    // The line colour is pure red, so its linear red is 1 and half of it is 0.5.
    let wanted = linear_to_srgb_byte(0.5);
    assert!(ring[0].abs_diff(wanted) <= 1 && ring[1] == 0 && ring[2] == 0, "got {ring:?}, wanted red {wanted}");
}

#[test]
fn the_line_is_opaque_on_a_transparent_background() {
    let mut subject = Subject::new(gpu(), &sphere(0.3, 32, 16));
    let mut s = scene(2.0);
    s.background = Background::Transparent;
    let Line { with, mask: line, .. } = subject.line(&s, &[Mat4::IDENTITY], Some(outline(0.02)), &plain());
    assert!(count(&line) > 50);
    for (i, _) in line.iter().enumerate().filter(|(_, on)| **on) {
        assert_eq!(with.rgba8[i * 4..i * 4 + 4], [255, 0, 0, 255], "line pixel {i} must be solid red");
    }
}

#[test]
fn multisampling_and_render_scale_keep_the_line_where_it_is() {
    let mut subject = Subject::new(gpu(), &sphere(0.3, 48, 24));
    let s = scene(2.0);
    let mut line_with = |quality: &Quality| subject.line(&s, &[Mat4::IDENTITY], Some(outline(0.02)), quality).mask;
    let single = line_with(&plain());
    for quality in [Quality { samples: 4, ..plain() }, Quality { render_scale: 2.0, ..plain() }] {
        let other = line_with(&quality);
        let off = disagreement(&single, &other);
        println!(
            "samples {} scale {}: {} vs {} line pixels, {off} differ",
            quality.samples,
            quality.render_scale,
            count(&single),
            count(&other)
        );
        // Edges anti-alias or downsample, so the set of changed pixels differs along the edge, not by much.
        assert!(off * 3 <= count(&single) * 2, "{off} of {} line pixels moved", count(&single));
        assert!(count(&other) * 2 > count(&single), "the line must survive");
    }
}

// ---- when the pass runs ----

fn report_for(width: Option<Outline>) -> FrameReport {
    let mut r = support::renderer();
    let mesh = r.create_mesh(&shapes::cuboid(Vec3::splat(0.4))).unwrap();
    let mut material = Material::unlit(SURFACE);
    material.outline = width;
    let material = r.create_material(&material).unwrap();
    let target = r.create_target(&TargetDesc { size: SIZE, alpha: AlphaEncoding::Straight }).unwrap();
    let mut s = scene(3.0);
    s.instances.push(Instance::new(mesh, material, Mat4::from_rotation_y(0.7)));
    r.render(target, &s, &plain()).unwrap()
}

#[test]
fn the_pass_is_listed_and_runs_only_for_a_visible_outline() {
    let renderer: WgpuRenderer = support::renderer();
    assert!(renderer.capabilities().supports(Pass::Outlines), "the pass is implemented");

    let none = report_for(None);
    assert_eq!(none.cpu.get(Pass::Outlines), None, "no outline: nothing to do");
    assert_eq!(none.draw_calls, 2, "one cube and the resolve triangle");

    let drawn = report_for(Some(outline(0.02)));
    assert!(drawn.cpu.get(Pass::Outlines).is_some(), "an outlined instance runs the pass");
    assert_eq!((drawn.draw_calls, drawn.triangles), (3, 24), "the cube and its hull, and the resolve triangle");

    let hairline = report_for(Some(outline(0.0)));
    assert_eq!(hairline.cpu.get(Pass::Outlines), None, "a width of 0 is no outline, not a hairline");
    assert_eq!(hairline.draw_calls, 2);
}

#[test]
fn a_zero_width_outline_changes_no_pixel() {
    let mut subject = Subject::new(gpu(), &sphere(0.3, 48, 24));
    let s = scene(2.0);
    let Line { with, without, .. } = subject.line(&s, &[Mat4::IDENTITY], Some(outline(0.0)), &plain());
    assert_eq!(with.rgba8, without.rgba8);
}
