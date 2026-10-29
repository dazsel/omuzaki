//! The CPU renderer against pixels computed by hand: coverage, seams, masks, composites, mips and thread counts.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test arithmetic on small numbers"
)]

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_puppet::{Canvas, CpuRenderer, Evaluator, PuppetBuilder, Texture, grid_mesh};
use omuzaki_puppet_api::{BlendMode, MaskMode, MaskRef, NodeIx, NodeKind, PuppetDef};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Texture {
    let px: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
    Texture::from_straight(w, h, &px).unwrap_or_else(|| panic!("a {w}x{h} texture"))
}

fn draw(def: PuppetDef, textures: &[Texture], w: u32, h: u32, threads: usize) -> Canvas {
    let e = Evaluator::new(def).unwrap_or_else(|e| panic!("a valid puppet: {e}"));
    let mut canvas = Canvas::new(w, h);
    CpuRenderer::new(threads).draw(e.draw_list(), textures, Affine2::IDENTITY, &mut canvas);
    canvas
}

fn px(c: &Canvas, x: u32, y: u32) -> [f32; 4] {
    c.get(x, y).unwrap_or_else(|| panic!("({x}, {y}) is on the canvas"))
}

fn near(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 2.0 / 255.0)
}

/// A quad over `rect` with a 2x2-cell grid mesh, so it has inner seams.
fn quad(b: &mut PuppetBuilder, name: &str, parent: NodeIx, tex: omuzaki_puppet_api::TexIx, rect: [f32; 4]) -> NodeIx {
    b.part(name, parent, tex, grid_mesh(rect, 2, 2, [0.0, 0.0, 1.0, 1.0]))
}

#[test]
fn a_quad_covers_exactly_its_pixels_and_seams_never_double_blend() {
    let mut b = PuppetBuilder::new("q", [16.0, 16.0]);
    let tex = b.texture("half.png", [4, 4]);
    let root = b.root();
    quad(&mut b, "q", root, tex, [2.0, 3.0, 8.0, 6.0]);
    let canvas = draw(b.finish().unwrap_or_default(), &[solid(4, 4, [255, 0, 0, 128])], 16, 16, 1);
    let half = [128.0 / 255.0, 0.0, 0.0, 128.0 / 255.0];
    let mut covered = 0;
    for y in 0..16 {
        for x in 0..16 {
            let p = px(&canvas, x, y);
            let inside = (2..10).contains(&x) && (3..9).contains(&y);
            if inside {
                assert!(near(p, half), "({x}, {y}) inside reads {p:?}: a seam blended twice?");
                covered += 1;
            } else {
                assert_eq!(p, [0.0; 4], "({x}, {y}) outside");
            }
        }
    }
    assert_eq!(covered, 48);
}

#[test]
fn any_thread_count_draws_the_same_pixels() {
    let mut b = PuppetBuilder::new("q", [64.0, 64.0]);
    let tex = b.texture("t.png", [8, 8]);
    let root = b.root();
    let pivot = b.group("p", root, [32.0, 32.0]);
    if let Some(n) = b.node_mut(pivot) {
        n.transform.rotation = 0.4;
    }
    quad(&mut b, "q", pivot, tex, [-20.0, -12.0, 40.0, 24.0]);
    let texels: Vec<u8> =
        (0..64u32).flat_map(|i| [(i * 4) as u8, (255 - i * 3) as u8, 90, (100 + i * 2) as u8]).collect();
    let t = Texture::from_straight(8, 8, &texels).unwrap_or_else(|| panic!("8x8"));
    let def = b.finish().unwrap_or_default();
    let one = draw(def.clone(), std::slice::from_ref(&t), 64, 64, 1);
    for threads in [2, 3, 7] {
        assert_eq!(
            one.pixels(),
            draw(def.clone(), std::slice::from_ref(&t), 64, 64, threads).pixels(),
            "{threads} threads"
        );
    }
}

#[test]
fn masks_clip_inside_and_dodges_cut_out() {
    let mut b = PuppetBuilder::new("m", [16.0, 16.0]);
    let red = b.texture("red.png", [2, 2]);
    let root = b.root();
    let mut window = grid_mesh([4.0, 0.0, 4.0, 16.0], 1, 1, [0.0, 0.0, 1.0, 1.0]);
    window.uvs.clear();
    let mask = b.node("window", root, [0.0, 0.0], NodeKind::Mask(window));
    let inside = quad(&mut b, "inside", root, red, [0.0, 0.0, 16.0, 8.0]);
    let outside = quad(&mut b, "outside", root, red, [0.0, 8.0, 16.0, 8.0]);
    if let Some(d) = b.drawable_mut(inside) {
        d.masks.push(MaskRef { source: mask, mode: MaskMode::Mask });
    }
    if let Some(d) = b.drawable_mut(outside) {
        d.masks.push(MaskRef { source: mask, mode: MaskMode::Dodge });
    }
    let canvas = draw(b.finish().unwrap_or_default(), &[solid(2, 2, [255, 0, 0, 255])], 16, 16, 2);
    let red_px = [1.0, 0.0, 0.0, 1.0];
    assert!(near(px(&canvas, 5, 2), red_px), "inside the window, top half");
    assert_eq!(px(&canvas, 1, 2), [0.0; 4], "outside the window, top half");
    assert_eq!(px(&canvas, 5, 12), [0.0; 4], "the dodge cuts the window out of the bottom half");
    assert!(near(px(&canvas, 12, 12), red_px));
    assert_eq!(px(&canvas, 5, 12)[3], 0.0, "and the mask mesh itself never draws");
}

#[test]
fn a_composite_fades_as_one_layer() {
    let mut b = PuppetBuilder::new("c", [16.0, 16.0]);
    let white = b.texture("white.png", [2, 2]);
    let root = b.root();
    let group = b.node("group", root, [0.0, 0.0], NodeKind::Composite(omuzaki_puppet_api::Composite::default()));
    quad(&mut b, "a", group, white, [0.0, 0.0, 10.0, 16.0]);
    quad(&mut b, "b", group, white, [6.0, 0.0, 10.0, 16.0]);
    if let Some(d) = b.drawable_mut(group) {
        d.opacity = 0.5;
    }
    let canvas = draw(b.finish().unwrap_or_default(), &[solid(2, 2, [255, 255, 255, 255])], 16, 16, 1);
    // Overlapping parts inside a composite don't add up: the overlap is as faded as the rest.
    assert!(near(px(&canvas, 8, 8), [0.5, 0.5, 0.5, 0.5]), "{:?}", px(&canvas, 8, 8));
    assert!(near(px(&canvas, 2, 8), [0.5, 0.5, 0.5, 0.5]));
}

#[test]
fn blend_modes_reach_the_canvas() {
    let mut b = PuppetBuilder::new("b", [8.0, 8.0]);
    let base = b.texture("base.png", [2, 2]);
    let top = b.texture("top.png", [2, 2]);
    let root = b.root();
    let under = quad(&mut b, "under", root, base, [0.0, 0.0, 8.0, 8.0]);
    let over = quad(&mut b, "over", root, top, [0.0, 0.0, 8.0, 4.0]);
    if let Some(n) = b.node_mut(under) {
        n.zsort = 1.0;
    }
    if let Some(d) = b.drawable_mut(over) {
        d.blend = BlendMode::Multiply;
    }
    let textures = [solid(2, 2, [204, 102, 255, 255]), solid(2, 2, [128, 255, 128, 255])];
    let canvas = draw(b.finish().unwrap_or_default(), &textures, 8, 8, 1);
    let m = |a: f32, b: f32| a / 255.0 * b / 255.0;
    assert!(
        near(px(&canvas, 3, 1), [m(204.0, 128.0), m(102.0, 255.0), m(255.0, 128.0), 1.0]),
        "{:?}",
        px(&canvas, 3, 1)
    );
    assert!(near(px(&canvas, 3, 6), [0.8, 0.4, 1.0, 1.0]), "the lower half is the base");
}

#[test]
fn a_shrunk_checkerboard_averages_to_grey_instead_of_aliasing() {
    let mut b = PuppetBuilder::new("mip", [256.0, 256.0]);
    let tex = b.texture("check.png", [256, 256]);
    let root = b.root();
    quad(&mut b, "q", root, tex, [0.0, 0.0, 256.0, 256.0]);
    let check: Vec<u8> = (0..256u32 * 256)
        .flat_map(|i| if ((i % 256) + i / 256) % 2 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] })
        .collect();
    let t = Texture::from_straight(256, 256, &check).unwrap_or_else(|| panic!("256x256"));
    let e = Evaluator::new(b.finish().unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    let mut canvas = Canvas::new(16, 16);
    let view = Affine2::from_scale(Vec2::splat(1.0 / 16.0));
    CpuRenderer::new(1).draw(e.draw_list(), std::slice::from_ref(&t), view, &mut canvas);
    for y in 0..16 {
        for x in 0..16 {
            let p = px(&canvas, x, y);
            assert!((p[0] - 0.5).abs() < 0.06, "({x}, {y}) is {p:?}: a 16x shrink must average the checks");
        }
    }
}
