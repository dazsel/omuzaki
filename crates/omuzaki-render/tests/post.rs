//! The post effects: bloom, depth of field, grain, grade and the quality modes, on the software adapter.
//!
//! The bloom is checked against pictures made by the prototype's own three.js composer (`tests/golden/`); the rest has
//! no prototype to copy, so each effect is checked against what it is defined to do: a grade of zero saturation is
//! grey, grain has the asked-for amplitude and the same seed gives the same bytes, the blur leaves the focal plane
//! alone and never smears the character's outline into the background. With `OMUZAKI_RENDER_OUT=<dir>` the tests keep
//! the pictures they render, for evidence.

// Integration tests may panic: a panic is a clear test failure. Their statistics (means, standard deviations, pixel counts)
// and pixel maths use small counts in float arithmetic, and a long test reads better top to bottom than split.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

mod support;

use std::path::PathBuf;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Bloom, Camera, Colour, ColourGrade, DepthOfField, FilmGrain, Filter, FrameInfo, Image,
    Instance, Light, Material, MaterialId, MeshData, Outline, Pass, Projection, Quality, QualityMode, Renderer,
    RendererExt, Sampler, Scene, TargetDesc, TextureDesc, Tonemap,
};
use omuzaki_render_mock::shapes;

/// A rectangle's `[left, top, right, bottom]` in pixels of a 512 x 512 image seen by an orthographic camera 2 m tall (256
/// pixels per metre, the origin at the centre of the image). Scenes draw `(Rect, colour)` pairs in order, each over the
/// one before.
type Rect = [u32; 4];

/// The scene the three.js reference image was made from (the prototype's generator script draws the same
/// rectangles with `MeshBasicMaterial` and the prototype's composer): a dim panel with a bright square, a thin bright
/// strip down its edge (a rim light), a bar that stays under the threshold and a three-pixel speck.
const PANEL: (Rect, u32) = ([96, 40, 416, 470], 0x005a_4d6e);
const BULB: (Rect, u32) = ([280, 120, 330, 170], 0x00ff_ffff);
const BAR: (Rect, u32) = ([120, 300, 230, 306], 0x00ff_d27a);
const RIM: (Rect, u32) = ([412, 60, 416, 440], 0x00f2_f6ff);
const SPECK: (Rect, u32) = ([200, 200, 203, 203], 0x00ff_ffff);
const BACKDROP: u32 = 0x001b_2236;
const SIZE: u32 = 512;

fn rect_scene(r: &mut dyn Renderer, rects: &[(Rect, u32)], background: u32) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 2.0, near: 0.1, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: Colour::hex(background) };
    let half = f32::from(u16::try_from(SIZE / 2).unwrap());
    for (i, &([left, top, right, bottom], colour)) in rects.iter().enumerate() {
        let px = |v: u32| f32::from(u16::try_from(v).unwrap());
        let centre =
            Vec2::new(f32::midpoint(px(left), px(right)) - half, half - f32::midpoint(px(top), px(bottom))) / half;
        let extent = Vec2::new(px(right - left), px(bottom - top)) / (2.0 * half);
        let mesh = r.create_mesh(&shapes::quad(extent)).unwrap();
        let material = r.create_material(&Material::unlit(Colour::hex(colour))).unwrap();
        let z = 0.01 * f32::from(u16::try_from(i).unwrap());
        scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(centre.extend(z))));
    }
    scene
}

fn parity_scene(r: &mut dyn Renderer) -> Scene {
    rect_scene(r, &[PANEL, BULB, BAR, RIM, SPECK], BACKDROP)
}

fn render(r: &mut dyn Renderer, scene: &Scene, quality: &Quality) -> Image {
    r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, scene, quality).unwrap()
}

/// Offline quality at four samples: the software adapter's maximum.
fn offline() -> Quality {
    Quality { samples: 4, ..Quality::offline() }
}

/// Keeps `image` as `name` in `OMUZAKI_RENDER_OUT`, if that is set. A premultiplied image is saved as stored, so its PNG
/// looks dark where it is transparent: composite it before judging it.
fn save(name: &str, image: &Image) {
    if let Some(dir) = std::env::var_os("OMUZAKI_RENDER_OUT") {
        std::fs::create_dir_all(&dir).unwrap();
        let path = PathBuf::from(dir).join(name);
        image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    }
}

const BLOOM: Bloom = Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 };

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

/// Largest per-channel difference between two images of the same size.
fn max_difference(a: &Image, b: &Image) -> u8 {
    assert_eq!(a.size, b.size);
    a.rgba8.iter().zip(&b.rgba8).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// A three.js reference picture, compiled in so the test needs no path to the source tree (the Windows build runs from
/// elsewhere).
fn load_golden(name: &str) -> Image {
    let bytes: &[u8] = match name {
        "three_bloom_prototype.png" => include_bytes!("golden/three_bloom_prototype.png"),
        "three_bloom_exposure_0_9.png" => include_bytes!("golden/three_bloom_exposure_0_9.png"),
        "three_bloom_strength_1_radius_1.png" => include_bytes!("golden/three_bloom_strength_1_radius_1.png"),
        other => panic!("no reference picture called {other}"),
    };
    let rgba = image::load_from_memory(bytes).unwrap().to_rgba8();
    Image::new(UVec2::new(rgba.width(), rgba.height()), AlphaEncoding::Straight, rgba.into_raw()).unwrap()
}

/// Mean squared horizontal step of the luma over a rectangle: how much edge there is, so how sharp it is. Squared, because
/// blurring a step spreads it without changing the sum of its steps, but divides the sum of their squares by the width.
fn edge_energy(image: &Image, [left, top, right, bottom]: [u32; 4]) -> f32 {
    let mut total = 0.0;
    for y in top..bottom {
        for x in left..right {
            total += (luma(image.pixel(x + 1, y).unwrap()) - luma(image.pixel(x, y).unwrap())).powi(2);
        }
    }
    total / ((right - left) * (bottom - top)) as f32
}

// ---- bloom ----

/// The three.js composer's pictures of the same scene: the Bloom pass reproduces them to within a few levels (the last
/// level or two is half-float rounding and the dither that keeps a glow's long fall-off from banding).
#[test]
fn bloom_matches_the_prototypes_three_js_composer() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    let cases = [
        ("three_bloom_prototype.png", 1.0, Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 }),
        // The studio rig: exposure 0.9, and the threshold is still compared before exposure.
        ("three_bloom_exposure_0_9.png", 0.9, Bloom { strength: 0.32, radius: 0.5, threshold: 0.92 }),
        // The extremes of strength and radius: strength acts twice in three.js, and radius 1 weights the widest mip most.
        ("three_bloom_strength_1_radius_1.png", 1.0, Bloom { strength: 1.0, radius: 1.0, threshold: 0.9 }),
    ];
    for (name, exposure, bloom) in cases {
        scene.post.exposure = exposure;
        scene.post.bloom = Some(bloom);
        let ours = render(&mut r, &scene, &offline());
        save(&format!("rust_{name}"), &ours);
        let reference = load_golden(name);
        let worst = max_difference(&ours, &reference);
        assert!(
            worst <= 4,
            "{name}: the bloom differs from three.js by {worst} levels (it is 2 on the software adapter)"
        );
    }
}

#[test]
fn bloom_adds_glow_around_what_is_bright_and_nowhere_else() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    let off = render(&mut r, &scene, &offline());
    scene.post.bloom = Some(BLOOM);
    let on = render(&mut r, &scene, &offline());
    save("rust_bloom_off.png", &off);
    save("rust_bloom_on.png", &on);

    let brighter = |x, y| luma(on.pixel(x, y).unwrap()) - luma(off.pixel(x, y).unwrap());
    assert!(brighter(340, 145) > 8.0, "just right of the bulb: {}", brighter(340, 145));
    assert!(brighter(305, 112) > 8.0, "just above the bulb: {}", brighter(305, 112));
    assert!(brighter(425, 250) > 6.0, "outside the rim strip, over the backdrop: {}", brighter(425, 250));
    assert!(brighter(300, 145).abs() < 20.0, "inside the bulb, already white");
    // The bar is under the threshold: it adds no glow of its own, and the corner is too far from anything to notice.
    assert!(brighter(60, 480).abs() <= 3.0, "far from everything: {}", brighter(60, 480));
}

/// Bloom works at the target's size, so a supersampled render draws the same glow: the high-pass takes a grid of taps over
/// the larger image instead of missing the thin bright lines in it.
#[test]
fn bloom_looks_the_same_at_any_render_scale() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    scene.post.bloom = Some(BLOOM);
    let reference = render(&mut r, &scene, &offline());
    for scale in [0.5, 1.5, 2.0] {
        let image = render(&mut r, &scene, &Quality { render_scale: scale, ..offline() });
        save(&format!("rust_bloom_scale_{scale}.png"), &image);
        // The picture itself differs at the edges of the rectangles (more samples, or fewer); the glow does not.
        let mut glow_difference = 0.0f32;
        for (x, y) in [(340, 145), (305, 112), (425, 250), (435, 250), (330, 200), (205, 205), (445, 120)] {
            glow_difference =
                glow_difference.max((luma(image.pixel(x, y).unwrap()) - luma(reference.pixel(x, y).unwrap())).abs());
        }
        assert!(glow_difference < 4.0, "scale {scale}: the glow differs by {glow_difference} levels");
    }
}

#[test]
fn bloom_uses_the_threshold_on_the_scene_light_before_exposure() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    scene.post.bloom = Some(Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 });
    // The panel's luminance is about 0.1: far under the threshold at exposure 1, and exposure does not lift it over.
    scene.post.exposure = 3.0;
    let off = {
        scene.post.bloom = None;
        render(&mut r, &scene, &offline())
    };
    scene.post.bloom = Some(BLOOM);
    let on = render(&mut r, &scene, &offline());
    // Only the bright things glow: the bar and the panel are far from the threshold, so away from the bulb, the rim and
    // the speck they match the picture without bloom.
    assert!(luma(on.pixel(150, 400).unwrap()) - luma(off.pixel(150, 400).unwrap()) < 3.0);
}

#[test]
fn zero_strength_and_non_finite_numbers_draw_no_bloom_and_do_not_poison_the_frame() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    let off = render(&mut r, &scene, &offline());
    for bloom in [
        Bloom { strength: 0.0, ..BLOOM },
        Bloom { strength: -1.0, ..BLOOM },
        Bloom { strength: f32::NAN, ..BLOOM },
        Bloom { strength: f32::INFINITY, ..BLOOM },
    ] {
        scene.post.bloom = Some(bloom);
        assert_eq!(render(&mut r, &scene, &offline()), off, "{bloom:?} adds nothing");
    }
    // A NaN elsewhere is pulled to zero, not spread over the picture.
    scene.post.bloom = Some(Bloom { radius: f32::NAN, threshold: f32::NAN, ..BLOOM });
    let image = render(&mut r, &scene, &offline());
    assert!(image.rgba8.as_chunks::<4>().0.iter().all(|p| p[3] == 255), "alpha is intact");
    assert!(luma(image.pixel(256, 256).unwrap()) > 40.0, "the panel still draws");
}

// ---- depth of field ----

/// A flat quad of half-size `half` centred at `at`.
fn add(r: &mut dyn Renderer, scene: &mut Scene, half: Vec2, at: Vec3, material: MaterialId) {
    let mesh = r.create_mesh(&shapes::quad(half)).unwrap();
    scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(at)));
}

/// A shot with something at three distances, camera at the origin of the distances: a character stand-in with a dark
/// outline 1.5 m away (centre left), a bright pole 3.5 m away and a checkered wall 6 m away.
fn depth_scene(r: &mut dyn Renderer) -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 3.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.1, far: 50.0 },
    ));
    scene.background = Background::Solid { colour: Colour::hex(0x0030_2a3a) };
    let cells = UVec2::new(40, 24);
    let checks: Vec<u8> = (0..cells.x * cells.y)
        .flat_map(
            |i| if (i % cells.x + i / cells.x).is_multiple_of(2) { [70, 130, 150, 255] } else { [30, 50, 70, 255] },
        )
        .collect();
    let hard = TextureDesc {
        sampler: Sampler { filter: Filter::Nearest, ..Sampler::default() },
        ..TextureDesc::colour(cells)
    };
    let texture = r.create_texture(&hard, Some(&checks)).unwrap();
    let mut wall = Material::unlit(Colour::WHITE);
    wall.base_texture = Some(texture);
    let wall = r.create_material(&wall).unwrap();
    add(r, &mut scene, Vec2::new(5.0, 3.1), Vec3::new(0.0, 0.0, -3.0), wall);
    let pole = r.create_material(&Material::unlit(Colour::hex(0x00ff_a040))).unwrap();
    add(r, &mut scene, Vec2::new(0.04, 1.2), Vec3::new(0.5, 0.0, -0.5), pole);
    let outline = r.create_material(&Material::unlit(Colour::hex(0x001a_1020))).unwrap();
    let body = r.create_material(&Material::unlit(Colour::hex(0x00c9_b8f0))).unwrap();
    add(r, &mut scene, Vec2::new(0.235, 0.355), Vec3::new(-0.15, -0.05, 1.49), outline);
    add(r, &mut scene, Vec2::new(0.22, 0.34), Vec3::new(-0.15, -0.05, 1.5), body);
    scene
}

fn focus(distance: f32) -> DepthOfField {
    DepthOfField { focus_distance: distance, focus_range: 0.3, max_blur: 0.04 }
}

/// A patch of the checkered wall to the right of the pole, and the character's left outline with the wall behind it.
const WALL: [u32; 4] = [420, 20, 500, 480];
const CHARACTER_EDGE: [u32; 4] = [4, 100, 34, 400];

fn exact_depth_scene(r: &mut dyn Renderer) -> Scene {
    let mut scene = depth_scene(r);
    scene.post.tonemap = Tonemap::None;
    scene
}

#[test]
fn a_focus_pull_moves_the_sharp_plane() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    let off = render(&mut r, &scene, &offline());
    save("rust_dof_off.png", &off);
    let (wall_off, edge_off) = (edge_energy(&off, WALL), edge_energy(&off, CHARACTER_EDGE));

    let mut sharpness = Vec::new();
    for (name, distance) in [("character", 1.5), ("pole", 3.5), ("wall", 6.0)] {
        scene.post.depth_of_field = Some(focus(distance));
        let image = render(&mut r, &scene, &offline());
        save(&format!("rust_dof_{name}.png"), &image);
        sharpness.push((edge_energy(&image, WALL) / wall_off, edge_energy(&image, CHARACTER_EDGE) / edge_off));
    }
    let [(wall_a, character_a), (wall_b, character_b), (wall_c, character_c)] = sharpness[..] else { panic!() };
    println!("kept sharpness (wall, character) focused on the character, the pole, the wall: {sharpness:?}");
    assert!(character_a > 0.9 && wall_a < 0.6, "focused on the character: it is sharp, the wall is not");
    assert!(character_b < 0.7 && wall_b < 0.8, "focused on the pole: both are soft");
    assert!(wall_c > 0.9 && character_c < 0.6, "focused on the wall: it is sharp, the character is not");
}

#[test]
fn what_is_in_focus_is_untouched_and_its_outline_does_not_smear_into_the_blur() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    let off = render(&mut r, &scene, &offline());
    scene.post.depth_of_field = Some(focus(1.5));
    let on = render(&mut r, &scene, &offline());

    // The character is in the sharp zone: its body, its outline and its edge are exactly as without the effect.
    for (x, y) in [(150, 250), (150, 70), (16, 250), (19, 250), (20, 250), (303, 250), (306, 250)] {
        let (got, want) = (on.pixel(x, y).unwrap(), off.pixel(x, y).unwrap());
        assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 3), "({x}, {y}): {got:?} against {want:?}");
    }
    // The wall behind it blurs, but the dark outline must not leak into it: just outside the outline (more than a few
    // pixels away, where the wall blurs freely) no pixel is darker than the wall's own darkest colour.
    let darkest_wall = luma([30, 50, 70, 255]);
    let mut worst = f32::MAX;
    for y in 120..380 {
        for x in (0..8).chain(312..330) {
            worst = worst.min(luma(on.pixel(x, y).unwrap()));
        }
    }
    assert!(
        worst >= darkest_wall - 3.0,
        "a dark halo beside the outline: luma {worst} (the wall is never under {darkest_wall})"
    );
}

#[test]
fn a_blurred_foreground_spreads_over_a_sharp_background() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    // A flat dark panel over the checkered wall, 5.9 m away, inside the sharp zone when the focus is on the wall.
    let flat = r.create_material(&Material::unlit(Colour::hex(0x0020_3040))).unwrap();
    add(&mut r, &mut scene, Vec2::new(5.0, 3.1), Vec3::new(0.0, 0.0, -2.9), flat);
    let off = render(&mut r, &scene, &offline());
    scene.post.depth_of_field = Some(focus(6.0));
    let on = render(&mut r, &scene, &offline());
    save("rust_dof_spill.png", &on);
    // The character, 4.5 m nearer, is blurred by 20 pixels, so it spreads over the panel beside it: its dark outline and
    // light body, mixed by distance from the edge. (Without the foreground search the soft edge would stop at the
    // geometry, and the panel would not change at all.)
    let spill = |x| luma(on.pixel(x, 250).unwrap()) - luma(off.pixel(x, 250).unwrap());
    let seen: Vec<f32> = [311, 316, 322, 330, 345].into_iter().map(spill).collect();
    println!(
        "the character's light spills onto the panel by {seen:.1?} levels at 3, 8, 14, 22 and 37 px from the edge"
    );
    assert!(seen[0] > 20.0 && seen[1] > 5.0, "it spills over the first pixels: {seen:?}");
    assert!(seen[3].abs() <= 3.0 && seen[4].abs() <= 1.0, "and stops at the blur's reach: {seen:?}");
}

#[test]
fn a_transparent_background_stays_transparent_through_depth_of_field() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    // Only the character: no wall behind it. Everything around it is empty.
    scene.instances.remove(0);
    scene.background = Background::Transparent;
    scene.post.depth_of_field = Some(focus(6.0));
    for alpha in [AlphaEncoding::Straight, AlphaEncoding::Premultiplied] {
        let image = r.render_image(UVec2::splat(SIZE), alpha, &scene, &offline()).unwrap();
        save(&format!("rust_dof_transparent_{alpha:?}.png").to_lowercase(), &image);
        // Far from the character, nothing appears.
        assert_eq!(image.pixel(480, 60).unwrap(), [0, 0, 0, 0], "{alpha:?}: empty stays empty");
        assert_eq!(image.pixel(450, 480).unwrap(), [0, 0, 0, 0]);
        // The character is solid in its middle, and blurred (some alpha beyond its rectangle) at its edge, which is out of
        // focus here.
        assert_eq!(image.pixel(150, 250).unwrap()[3], 255, "{alpha:?}: opaque inside");
        let beyond = image.pixel(309, 250).unwrap()[3];
        assert!(beyond > 0 && beyond < 255, "{alpha:?}: a soft edge, got alpha {beyond}");
    }
}

/// An opaque scene keeps each pixel's circle of confusion in the alpha of the colour the gather reads (one fetch a tap); a
/// scene with any transparency keeps it in an image of its own. A background one level short of opaque takes the second
/// route and must draw the same picture, so the packed gather is the same blur as the general one.
#[test]
fn the_packed_gather_of_an_opaque_scene_draws_the_general_gathers_picture() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    scene.post.depth_of_field = Some(focus(1.5));
    let packed = render(&mut r, &scene, &offline());
    scene.background = Background::Solid { colour: Colour::rgba(0x30, 0x2a, 0x3a, 254) };
    let general = render(&mut r, &scene, &offline());
    save("rust_dof_packed.png", &packed);
    save("rust_dof_general.png", &general);
    let worst = max_difference(&packed, &general);
    assert!(worst <= 3, "the two gathers differ by {worst} levels");
    let sharp = {
        scene.post.depth_of_field = None;
        edge_energy(&render(&mut r, &scene, &offline()), WALL)
    };
    assert!(edge_energy(&packed, WALL) < 0.1 * sharp, "and the picture is blurred, or the comparison proves nothing");
}

#[test]
fn depth_of_field_works_with_and_without_multisampled_depth() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    scene.post.depth_of_field = Some(focus(1.5));
    let single = render(&mut r, &scene, &Quality { samples: 1, ..offline() });
    let multi = render(&mut r, &scene, &offline());
    save("rust_dof_1_sample.png", &single);
    // Away from edges they are the same picture: the wall's blur.
    assert!((edge_energy(&single, WALL) - edge_energy(&multi, WALL)).abs() < 0.15 * edge_energy(&multi, WALL));
}

#[test]
fn offline_gathers_more_taps_so_the_same_blur_is_smoother() {
    let mut r = support::renderer();
    let mut scene = exact_depth_scene(&mut r);
    let sharp = edge_energy(&render(&mut r, &scene, &offline()), WALL);
    scene.post.depth_of_field = Some(focus(1.5));
    let live = render(&mut r, &scene, &Quality { mode: QualityMode::Live, ..offline() });
    let frame_exact = render(&mut r, &scene, &offline());
    save("rust_dof_live_taps.png", &live);
    // The same look at both: the wall is blurred to a few percent of its edge energy, and the frame-exact gather leaves
    // less of the live gather's fine noise in it.
    let (a, b) = (edge_energy(&live, WALL), edge_energy(&frame_exact, WALL));
    println!("wall edge energy: {sharp:.1} sharp, {a:.2} with live taps, {b:.2} with offline taps");
    assert!(a < 0.1 * sharp && b < 0.1 * sharp, "both blur the wall");
    assert!(b < a && a < 1.5 * b, "live {a}, offline {b}");
}

// ---- grade, grain, dither ----

#[test]
fn a_neutral_grade_changes_nothing_and_zero_saturation_is_grey() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    let off = render(&mut r, &scene, &offline());
    scene.post.grade = Some(ColourGrade { saturation: 1.0, contrast: 1.0 });
    assert_eq!(render(&mut r, &scene, &offline()), off, "a neutral grade is not applied at all");

    scene.post.grade = Some(ColourGrade { saturation: 0.0, contrast: 1.0 });
    let grey = render(&mut r, &scene, &offline());
    save("rust_grade_grey.png", &grey);
    assert!(
        grey.rgba8.as_chunks::<4>().0.iter().all(|p| p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1),
        "grey is grey"
    );
    // Display-space luma is kept: the panel's grey is the panel's brightness.
    let (panel_off, panel_grey) = (off.pixel(150, 400).unwrap(), grey.pixel(150, 400).unwrap());
    assert!((luma(panel_off) - luma(panel_grey)).abs() < 2.0, "{panel_off:?} -> {panel_grey:?}");

    scene.post.grade = Some(ColourGrade { saturation: 1.6, contrast: 1.0 });
    let vivid = render(&mut r, &scene, &offline());
    save("rust_grade_vivid.png", &vivid);
    let spread = |p: [u8; 4]| p[..3].iter().max().unwrap() - p[..3].iter().min().unwrap();
    assert!(spread(vivid.pixel(150, 400).unwrap()) > spread(off.pixel(150, 400).unwrap()), "more colour");
}

#[test]
fn contrast_steepens_the_curve_around_mid_grey_and_keeps_black_and_white() {
    let mut r = support::renderer();
    let mut scene = rect_scene(
        &mut r,
        &[
            ([0, 0, 128, 512], 0x0000_0000),
            ([128, 0, 256, 512], 0x0040_4040),
            ([256, 0, 384, 512], 0x00bc_bcbc),
            ([384, 0, 512, 512], 0x00ff_ffff),
        ],
        0x0080_8080,
    );
    scene.post.tonemap = Tonemap::None;
    let off = render(&mut r, &scene, &offline());
    let at = |image: &Image, quarter: u32| image.pixel(quarter * 128 + 64, 256).unwrap()[0];
    scene.post.grade = Some(ColourGrade { saturation: 1.0, contrast: 1.5 });
    let steep = render(&mut r, &scene, &offline());
    scene.post.grade = Some(ColourGrade { saturation: 1.0, contrast: 0.5 });
    let flat = render(&mut r, &scene, &offline());
    scene.post.grade = Some(ColourGrade { saturation: 1.0, contrast: 0.0 });
    let grey = render(&mut r, &scene, &offline());
    assert!(
        grey.rgba8.as_chunks::<4>().0.iter().all(|p| p[0].abs_diff(128) <= 2),
        "a contrast of zero is a flat mid grey, black and white included"
    );
    save("rust_grade_contrast_steep.png", &steep);
    save("rust_grade_contrast_flat.png", &flat);
    assert!(at(&steep, 0) <= 1 && at(&steep, 3) >= 254, "black and white stay where they are");
    assert!(at(&steep, 1) < at(&off, 1) && at(&steep, 2) > at(&off, 2), "darks go darker, lights lighter");
    assert!(at(&flat, 1) > at(&off, 1) && at(&flat, 2) < at(&off, 2), "less contrast pulls them together");
    assert!(at(&flat, 0) <= 1 && at(&flat, 3) >= 254, "and still keeps the ends");
}

/// A flat mid-grey picture with film grain of `strength` on it.
fn grain_scene(r: &mut dyn Renderer, strength: f32) -> Scene {
    let mut scene = rect_scene(r, &[([0, 0, 512, 512], 0x0080_8080)], 0x0080_8080);
    scene.post.tonemap = Tonemap::None;
    scene.post.grain = Some(FilmGrain { strength });
    scene
}

#[test]
fn grain_has_the_asked_amplitude_and_is_seeded_by_the_frame() {
    let mut r = support::renderer();
    let mut scene = grain_scene(&mut r, 0.0);
    scene.post.grain = None;
    let flat = render(&mut r, &scene, &offline());
    let base = f64::from(flat.pixel(0, 0).unwrap()[0]);
    scene.frame = FrameInfo { index: 12, ..FrameInfo::default() };
    for strength in [0.02f32, 0.08] {
        scene.post.grain = Some(FilmGrain { strength });
        let image = render(&mut r, &scene, &offline());
        let samples: Vec<f64> = image.rgba8.as_chunks::<4>().0.iter().map(|p| f64::from(p[0]) - base).collect();
        let n = samples.len() as f64;
        let mean = samples.iter().sum::<f64>() / n;
        let std = (samples.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
        // Triangular noise on -1..1 has a standard deviation of 1/sqrt(6), in levels of 1/255, plus the dither's.
        let expected = f64::from(strength) * 255.0 / 6.0f64.sqrt();
        println!("grain {strength}: mean {mean:.3}, std {std:.2} levels (expected about {expected:.2})");
        assert!(mean.abs() < 0.3, "grain has no bias: {mean}");
        assert!((std - expected).abs() < 0.15 * expected + 0.6, "std {std}, expected about {expected}");
        save(&format!("rust_grain_{strength}.png"), &image);
    }
    scene.post.grain = Some(FilmGrain { strength: 0.05 });
    scene.frame.index = 7;
    let a = render(&mut r, &scene, &offline());
    let again = render(&mut r, &scene, &offline());
    scene.frame.index = 8;
    let next = render(&mut r, &scene, &offline());
    assert_eq!(a, again, "the same frame has the same grain, byte for byte");
    let changed = a.rgba8.iter().zip(&next.rgba8).filter(|(x, y)| x != y).count();
    assert!(
        changed * 2 > a.rgba8.len(),
        "the next frame has other grain ({changed} of {} bytes differ)",
        a.rgba8.len()
    );
}

#[test]
fn grain_never_touches_alpha_and_leaves_empty_pixels_empty() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    scene.background = Background::Transparent;
    scene.post.grain = Some(FilmGrain { strength: 0.2 });
    scene.post.grade = Some(ColourGrade { saturation: 1.3, contrast: 1.2 });
    for alpha in [AlphaEncoding::Straight, AlphaEncoding::Premultiplied] {
        let image = r.render_image(UVec2::splat(SIZE), alpha, &scene, &offline()).unwrap();
        let mut empty = 0;
        for p in image.rgba8.as_chunks::<4>().0 {
            if p[3] == 0 {
                empty += 1;
                assert_eq!(p[..3], [0, 0, 0], "{alpha:?}: an empty pixel has no colour");
            }
        }
        assert!(empty > 20_000, "{alpha:?}: the backdrop stays transparent ({empty} empty pixels)");
        assert_eq!(image.pixel(150, 400).unwrap()[3], 255, "{alpha:?}: the panel stays opaque");
    }
}

// ---- transparent output ----

/// Composites a transparent image over a flat display-space colour the way OBS blends: straight is `c * a + back * (1 - a)`,
/// premultiplied is `c + back * (1 - a)`.
fn over(image: &Image, back: [u8; 3], x: u32, y: u32) -> [f32; 3] {
    let p = image.pixel(x, y).unwrap();
    let a = f32::from(p[3]) / 255.0;
    std::array::from_fn(|i| {
        let c = f32::from(p[i]);
        let back = f32::from(back[i]) * (1.0 - a);
        match image.alpha {
            AlphaEncoding::Straight => c * a + back,
            AlphaEncoding::Premultiplied => c + back,
        }
    })
}

#[test]
fn the_glow_reaches_past_a_transparent_silhouette_in_both_alpha_encodings() {
    let mut r = support::renderer();
    let mut scene = parity_scene(&mut r);
    scene.post.bloom = Some(BLOOM);
    // The reference: the same scene on an opaque grey, which is what a compositor would show it over.
    scene.background = Background::Solid { colour: Colour::hex(0x0080_8080) };
    let reference = render(&mut r, &scene, &offline());
    let grey = reference.pixel(5, 5).unwrap();
    let back = [grey[0], grey[1], grey[2]];

    scene.background = Background::Transparent;
    let straight = r.render_image(UVec2::splat(SIZE), AlphaEncoding::Straight, &scene, &offline()).unwrap();
    let premultiplied = r.render_image(UVec2::splat(SIZE), AlphaEncoding::Premultiplied, &scene, &offline()).unwrap();
    save("rust_alpha_straight.png", &straight);
    save("rust_alpha_premultiplied.png", &premultiplied);

    assert_eq!(straight.pixel(5, 5).unwrap(), [0, 0, 0, 0], "far from everything: nothing");
    let halo = straight.pixel(424, 250).unwrap()[3];
    assert!(halo > 10 && halo < 255, "the rim's halo has alpha outside the panel: {halo}");
    assert!(straight.pixel(424, 250).unwrap()[3] > straight.pixel(470, 250).unwrap()[3], "and it fades with distance");

    // Both encodings composite to the same picture, and to the opaque render, within the error of blending a glow in
    // display space (it is largest right at the rim's halo).
    let mut worst_encoding = 0.0f32;
    let mut worst_reference = 0.0f32;
    let mut total_reference = 0.0f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (a, b) = (over(&straight, back, x, y), over(&premultiplied, back, x, y));
            let want = reference.pixel(x, y).unwrap();
            for i in 0..3 {
                worst_encoding = worst_encoding.max((a[i] - b[i]).abs());
                let off = (a[i] - f32::from(want[i])).abs();
                worst_reference = worst_reference.max(off);
                total_reference += off;
            }
        }
    }
    let mean = total_reference / (SIZE * SIZE * 3) as f32;
    println!(
        "composite over grey: straight vs premultiplied worst {worst_encoding:.2}, vs opaque worst {worst_reference:.2} mean {mean:.3}"
    );
    assert!(worst_encoding <= 2.0, "straight and premultiplied agree: {worst_encoding}");
    assert!(
        worst_reference <= 20.0 && mean < 1.0,
        "the glow composites like the opaque render: worst {worst_reference}, mean {mean}"
    );
}

// ---- quality modes, determinism, reporting ----

fn everything(r: &mut dyn Renderer) -> Scene {
    let mut scene = depth_scene(r);
    scene.post.bloom = Some(BLOOM);
    scene.post.depth_of_field = Some(focus(1.5));
    scene.post.grain = Some(FilmGrain { strength: 0.04 });
    scene.post.grade = Some(ColourGrade { saturation: 1.1, contrast: 1.1 });
    scene
}

#[test]
fn offline_frames_are_byte_stable_whatever_was_drawn_before() {
    let mut r = support::renderer();
    let mut scene = everything(&mut r);
    scene.frame.index = 41;
    let first = render(&mut r, &scene, &offline());
    save("rust_everything_offline.png", &first);
    // Other frames in between, a different size and other quality: nothing leaks into the next frame.
    let mut other = scene.clone();
    other.frame.index = 99;
    other.post.bloom = None;
    render(&mut r, &other, &Quality { samples: 4, ..Quality::high() });
    let small = r.create_target(&TargetDesc { size: UVec2::splat(96), alpha: AlphaEncoding::Straight }).unwrap();
    r.render(small, &scene, &offline()).unwrap();
    let second = render(&mut r, &scene, &offline());
    assert_eq!(first, second, "an offline frame depends only on the scene, the quality and the resources");
    let mut fresh = support::renderer();
    let scene = everything(&mut fresh);
    let mut other_scene = scene;
    other_scene.frame.index = 41;
    assert_eq!(render(&mut fresh, &other_scene, &offline()), first, "and not on the renderer's history either");
}

#[test]
fn post_effects_off_is_exposure_and_tone_mapping_alone() {
    let mut r = support::renderer();
    let mut scene = everything(&mut r);
    let target = r.create_target(&TargetDesc { size: UVec2::splat(SIZE), alpha: AlphaEncoding::Straight }).unwrap();

    let plain = Quality { post_effects: false, ..offline() };
    let report = r.render(target, &scene, &plain).unwrap();
    let with_everything_requested = r.read_target(target).unwrap();
    scene.post.bloom = None;
    scene.post.depth_of_field = None;
    scene.post.grain = None;
    scene.post.grade = None;
    r.render(target, &scene, &plain).unwrap();
    assert_eq!(with_everything_requested, r.read_target(target).unwrap(), "switched off by quality, byte for byte");
    assert!(report.cpu.get(Pass::Bloom).is_none() && report.cpu.get(Pass::DepthOfField).is_none());
    assert!(report.cpu.get(Pass::Resolve).is_some());
}

#[test]
fn the_report_says_which_post_passes_ran() {
    let mut r = support::renderer();
    assert!(r.capabilities().supports(Pass::Bloom) && r.capabilities().supports(Pass::DepthOfField));
    let mut scene = parity_scene(&mut r);
    let target = r.create_target(&TargetDesc { size: UVec2::splat(64), alpha: AlphaEncoding::Straight }).unwrap();
    let ran = |r: &mut dyn Renderer, scene: &Scene| {
        let report = r.render(target, scene, &Quality { samples: 1, ..Quality::high() }).unwrap();
        (report.cpu.get(Pass::Bloom).is_some(), report.cpu.get(Pass::DepthOfField).is_some())
    };
    assert_eq!(ran(&mut r, &scene), (false, false), "nothing asked for");
    scene.post.bloom = Some(BLOOM);
    assert_eq!(ran(&mut r, &scene), (true, false));
    scene.post.depth_of_field = Some(focus(2.0));
    assert_eq!(ran(&mut r, &scene), (true, true));
    scene.post.depth_of_field = Some(DepthOfField { focus_distance: 2.0, focus_range: 0.3, max_blur: 0.0 });
    assert_eq!(ran(&mut r, &scene), (true, false), "a blur of nothing is not drawn");
}

#[test]
fn every_render_scale_and_target_size_works_with_every_effect() {
    let mut r = support::renderer();
    let scene = everything(&mut r);
    for (size, scale) in
        [(UVec2::new(101, 57), 1.0), (UVec2::new(64, 64), 0.5), (UVec2::new(50, 31), 2.0), (UVec2::splat(1), 1.0)]
    {
        let target = r.create_target(&TargetDesc { size, alpha: AlphaEncoding::Premultiplied }).unwrap();
        r.render(target, &scene, &Quality { render_scale: scale, samples: 4, ..Quality::high() }).unwrap();
        let image = r.read_target(target).unwrap();
        assert_eq!(image.size, size);
        r.release(target.into()).unwrap();
    }
}

// ---- toon shading and outlines, composed with the effects ----

/// A UV sphere of radius `radius` with welded vertices (the outline pass pushes them along their smooth normals).
fn sphere(radius: f32) -> MeshData {
    let (rings, segments) = (48u32, 96u32);
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let (sin_t, cos_t) = (ring as f32 / rings as f32 * std::f32::consts::PI).sin_cos();
        for segment in 0..=segments {
            let (sin_p, cos_p) = (segment as f32 / segments as f32 * std::f32::consts::TAU).sin_cos();
            let n = Vec3::new(sin_t * cos_p, cos_t, sin_t * sin_p);
            mesh.positions.push(n * radius);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(segment as f32 / segments as f32, ring as f32 / rings as f32));
        }
    }
    for ring in 0..rings {
        for segment in 0..segments {
            let a = ring * (segments + 1) + segment;
            let b = a + segments + 1;
            mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

/// A toon character stand-in with an outline 1.5 m from the camera, a glowing toon sphere beside it (emissive, so bloom has
/// something real to catch) and the checkered wall behind, lit by a key and a hemisphere light.
fn toon_scene(r: &mut dyn Renderer) -> Scene {
    let mut scene = depth_scene(r);
    scene.instances.clear();
    scene.lights = vec![
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
        Light::Directional {
            to_light: Vec3::new(-0.8, 1.6, 2.2),
            colour: Colour::WHITE,
            intensity: 2.0,
            casts_shadow: false,
        },
    ];
    // The wall comes from `depth_scene`: rebuild it as the first instance.
    let wall_scene = depth_scene(r);
    scene.instances.push(wall_scene.instances[0]);
    let line = Some(Outline { colour: Colour::hex(0x002a_1a30), width: 0.012 });
    let mut skin = Material::toon(Colour::hex(0x00f9_d3c5));
    skin.outline = line;
    let skin = r.create_material(&skin).unwrap();
    let mut lamp = Material::toon(Colour::hex(0x00ff_f0c0));
    if let omuzaki_render_api::Shading::Toon(toon) = &mut lamp.shading {
        toon.emissive = Colour::hex(0x00ff_e8a0);
    }
    let lamp = r.create_material(&lamp).unwrap();
    let body = r.create_mesh(&sphere(0.2)).unwrap();
    let small = r.create_mesh(&sphere(0.08)).unwrap();
    scene.instances.push(Instance::new(body, skin, Mat4::from_translation(Vec3::new(-0.1, 0.0, 1.5))));
    scene.instances.push(Instance::new(small, lamp, Mat4::from_translation(Vec3::new(0.22, 0.1, 1.5))));
    scene
}

const TOON_GOLDEN: &[u8] = include_bytes!("golden/toon_outline_dof_bloom.png");

/// Real toon shading and outlines with depth of field and bloom on: the outline stays crisp when the character is in focus,
/// bloom does not put a halo on the outline away from the lamp, and the picture matches the committed render.
#[test]
fn the_outline_of_a_toon_character_survives_depth_of_field_and_bloom() {
    let mut r = support::renderer();
    let mut scene = toon_scene(&mut r);
    scene.post.tonemap = Tonemap::Aces;
    let off = render(&mut r, &scene, &offline());
    // A sharp zone as deep as the character (an author sets it so; the outline's far side is 0.4 m behind its near side).
    scene.post.depth_of_field = Some(DepthOfField { focus_range: 0.8, ..focus(1.5) });
    scene.post.bloom = Some(BLOOM);
    let on = render(&mut r, &scene, &offline());
    save("rust_toon_off.png", &off);
    save("rust_toon_dof_bloom.png", &on);

    // The outline pixels on the side away from the lamp (the lamp's own glow, which is additive light, brightens the line
    // next to it as it should; purple-dark, unlike the wall's blue-dark cells) must be the same, to within a few levels,
    // with the effects: no halo from the line, no smear.
    let is_outline = |p: [u8; 4]| p[0] > p[1] + 3 && luma(p) < 60.0;
    let mut ring = Vec::new();
    // Interior pixels of the line only (the line continues two pixels each way): its anti-aliased edge blends with the wall.
    let line = |x: u32, y: u32| off.pixel(x, y).is_some_and(is_outline);
    for y in 2..SIZE - 2 {
        for x in 2..230 {
            if line(x, y) && line(x - 2, y) && line(x + 2, y) && line(x, y - 2) && line(x, y + 2) {
                ring.push((x, y));
            }
        }
    }
    assert!(ring.len() > 300, "the outline is drawn: {} pixels", ring.len());
    // The lamp's glow is additive light over the whole picture (beside the lamp it lifts the line by up to 55 levels, as it
    // lifts the wall: `a_lamps_glow_lifts_a_dark_outline...` checks that against three.js; away from it, 10 to 15),
    // so the line lifts a little; it must not glow itself, smear or take the wall's colour: still dark, still purple.
    let worst = ring
        .iter()
        .map(|&(x, y)| (luma(off.pixel(x, y).unwrap()), luma(on.pixel(x, y).unwrap())))
        .fold((0.0f32, 0.0f32), |(lift, top), (a, b)| (lift.max(b - a), top.max(b)));
    println!("outline lift at most {:.1} levels, brightest outline pixel {:.1}", worst.0, worst.1);
    assert!(worst.0 < 22.0 && worst.1 < 60.0, "the outline changed: {worst:?}");
    // Crisp: along the middle row the step from the outline to the body keeps its size.
    let outline_end = (0..SIZE).filter(|&x| x < 230 && is_outline(off.pixel(x, 256).unwrap())).max().unwrap();
    let step = |image: &Image| {
        (luma(image.pixel(outline_end + 3, 256).unwrap()) - luma(image.pixel(outline_end, 256).unwrap())).abs()
    };
    println!("outline edge step: {:.1} without effects, {:.1} with", step(&off), step(&on));
    assert!(step(&on) > 0.8 * step(&off), "edge step {} against {}", step(&on), step(&off));

    let golden = {
        let rgba = image::load_from_memory(TOON_GOLDEN).unwrap().to_rgba8();
        Image::new(UVec2::new(rgba.width(), rgba.height()), AlphaEncoding::Straight, rgba.into_raw()).unwrap()
    };
    let worst = max_difference(&on, &golden);
    assert!(worst <= 4, "the picture differs from the golden by {worst} levels");
}

/// The prototype's outline is an ordinary mesh in the scene that its composer bloom reads, so the glow of a lamp lifts a
/// dark outline beside it (here by 12 or 13 levels, 76 pixels from a white bulb, a little more than the wall's own 10). Ours
/// must lift it the same way: this compares a dark line next to a bulb with three.js's picture of the same scene.
#[test]
fn a_lamps_glow_lifts_a_dark_outline_as_it_does_in_the_prototype() {
    let mut r = support::renderer();
    let mut scene =
        rect_scene(&mut r, &[([318, 150, 324, 310], 0x002a_1a30), ([400, 200, 460, 260], 0x00ff_ffff)], 0x000a_2038);
    let off = render(&mut r, &scene, &offline());
    scene.post.bloom = Some(BLOOM);
    let on = render(&mut r, &scene, &offline());
    save("rust_outline_strip_on.png", &on);
    let reference = |bytes: &[u8]| {
        let rgba = image::load_from_memory(bytes).unwrap().to_rgba8();
        Image::new(UVec2::new(rgba.width(), rgba.height()), AlphaEncoding::Straight, rgba.into_raw()).unwrap()
    };
    let three_on = reference(include_bytes!("golden/three_bloom_outline_strip.png"));
    let three_off = reference(include_bytes!("golden/three_off_outline_strip.png"));
    assert!(max_difference(&off, &three_off) <= 1, "the same scene without bloom");
    assert!(max_difference(&on, &three_on) <= 4, "with bloom, within a few levels of three.js");
    let lift = |a: &Image, b: &Image| luma(b.pixel(321, 230).unwrap()) - luma(a.pixel(321, 230).unwrap());
    println!("outline lift: {:.1} levels here, {:.1} in three.js", lift(&off, &on), lift(&three_off, &three_on));
    assert!((lift(&off, &on) - lift(&three_off, &three_on)).abs() < 2.0);
}

// ---- evidence ----

/// Pictures for the pull request, at a size where the blur can be judged: the focus pull at 1280 x 720 in live and offline
/// quality, and the finished look with everything on. Does nothing unless `OMUZAKI_RENDER_OUT` names a folder.
#[test]
fn evidence_pictures_at_720p() {
    if std::env::var_os("OMUZAKI_RENDER_OUT").is_none() {
        return;
    }
    let mut r = support::renderer();
    let mut scene = depth_scene(&mut r);
    let mut look = depth_scene(&mut r);
    let size = UVec2::new(1280, 720);
    let mut shoot = |name: &str, scene: &Scene, quality: &Quality| {
        save(name, &r.render_image(size, AlphaEncoding::Straight, scene, quality).unwrap());
    };
    shoot("rust_720p_dof_off.png", &scene, &offline());
    for (name, distance) in [("character", 1.5), ("pole", 3.5), ("wall", 6.0)] {
        scene.post.depth_of_field = Some(DepthOfField { focus_distance: distance, focus_range: 0.3, max_blur: 0.03 });
        shoot(&format!("rust_720p_dof_{name}_offline.png"), &scene, &offline());
        shoot(&format!("rust_720p_dof_{name}_live.png"), &scene, &Quality { samples: 4, ..Quality::high() });
    }
    look.post.bloom = Some(BLOOM);
    look.post.depth_of_field = Some(DepthOfField { focus_distance: 1.5, focus_range: 0.3, max_blur: 0.03 });
    look.post.grain = Some(FilmGrain { strength: 0.04 });
    look.post.grade = Some(ColourGrade { saturation: 1.15, contrast: 1.15 });
    shoot("rust_720p_everything_offline.png", &look, &offline());

    // The grade and the grain on the bloom scene, whose colours and fall-offs show them.
    let mut scene = parity_scene(&mut r);
    save("rust_look_off.png", &render(&mut r, &scene, &offline()));
    for (name, saturation, contrast) in
        [("sat0", 0.0, 1.0), ("sat1.5", 1.5, 1.0), ("con1.4", 1.0, 1.4), ("con0.6", 1.0, 0.6)]
    {
        scene.post.grade = Some(ColourGrade { saturation, contrast });
        save(&format!("rust_look_grade_{name}.png"), &render(&mut r, &scene, &offline()));
    }
    scene.post.grade = None;
    scene.frame.index = 7;
    for strength in [0.05, 0.15] {
        scene.post.grain = Some(FilmGrain { strength });
        save(&format!("rust_look_grain_{strength}.png"), &render(&mut r, &scene, &offline()));
    }
}
