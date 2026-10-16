//! The GPU encoder's tests (`raster::gpu::SceneEncoder`): unbalanced pushes and pops, determinism, the bounds under
//! nested transforms, and the allocation count of a warm re-encode. None needs a GPU: it checks the encoding, not pixels.

// The GPU rasteriser exists on native targets only (#2207).
#![cfg(not(target_arch = "wasm32"))]
// A counting allocator is the only way to *prove* an allocation budget; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face::raster::gpu::SceneEncoder;
use omuzaki_face_api::{
    Cap, Cmd, Colour, DrawList, FaceError, FaceLayerId, FaceLayerSpec, FaceRect, Join, Paint, PathRef, Stops,
};
use vello::Scene;

struct Counting;

thread_local! {
    // `const`-initialised without destructors, so touching them never allocates (this runs inside the allocator).
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f`; returns how many allocations this thread made during it.
fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// 0.2 m x 0.1 m at 500 px/m: a 100 x 50 image, plane origin at its centre (pixel 50, 25).
fn spec() -> FaceLayerSpec {
    FaceLayerSpec::for_rect(FaceRect { x0: -0.1, x1: 0.1, y0: -0.05, y1: 0.05 }, FaceLayerId::Base, 500.0)
}

fn encode(list: &DrawList, enc: &mut SceneEncoder, scene: &mut Scene) -> Result<(), FaceError> {
    scene.reset();
    enc.encode(&spec(), list, 500.0, scene)
}

fn square(list: &mut DrawList, c: Vec2, r: f32) -> PathRef {
    list.path()
        .polyline(&[c - Vec2::splat(r), c + Vec2::new(r, -r), c + Vec2::splat(r), c + Vec2::new(-r, r)])
        .close()
        .finish()
}

fn message(e: FaceError) -> String {
    match e {
        FaceError::Raster(m) | FaceError::Invalid(m) => m,
    }
}

#[test]
fn a_pop_with_nothing_pushed_is_an_error_naming_the_command() {
    let (mut enc, mut scene) = (SceneEncoder::new(), Scene::new());
    for (pop, name) in [(Cmd::PopClip, "PopClip"), (Cmd::PopTransform, "PopTransform")] {
        let mut list = DrawList::new();
        let p = square(&mut list, Vec2::ZERO, 0.01);
        list.fill(p, Paint::Solid(Colour::rgb(1, 2, 3)));
        list.push(pop);
        let m = message(encode(&list, &mut enc, &mut scene).unwrap_err());
        assert!(m.contains("command 1") && m.contains(name), "{m}");
    }
}

#[test]
fn a_push_that_is_never_popped_is_an_error_naming_the_command() {
    let (mut enc, mut scene) = (SceneEncoder::new(), Scene::new());
    let mut list = DrawList::new();
    let p = square(&mut list, Vec2::ZERO, 0.01);
    list.push(Cmd::PushTransform(Affine2::IDENTITY));
    list.push(Cmd::PushClip(p));
    let m = message(encode(&list, &mut enc, &mut scene).unwrap_err());
    assert!(m.contains("command 1") && m.contains("PushClip"), "{m}");
    list.push(Cmd::PopClip);
    let m = message(encode(&list, &mut enc, &mut scene).unwrap_err());
    assert!(m.contains("command 0") && m.contains("PushTransform"), "{m}");
    list.push(Cmd::PopTransform);
    encode(&list, &mut enc, &mut scene).unwrap();
}

/// True if the two scenes' encoding streams (paths, draws, transforms, styles, gradient stops) are identical.
fn same_encoding(a: &Scene, b: &Scene) -> bool {
    let (a, b) = (a.encoding(), b.encoding());
    a.path_tags == b.path_tags
        && a.path_data == b.path_data
        && a.draw_tags == b.draw_tags
        && a.draw_data == b.draw_data
        && a.transforms == b.transforms
        && a.styles == b.styles
        && a.resources.color_stops == b.resources.color_stops
}

/// A list using every command kind: clips, transforms, strokes, and solid, linear and radial paints.
fn busy_list() -> DrawList {
    let mut list = DrawList::new();
    let stops = Stops::new(&[
        (0.0, Colour::rgb(255, 0, 0)),
        (0.5, Colour::rgba(0, 255, 0, 128)),
        (1.0, Colour::rgb(0, 0, 255)),
    ]);
    for i in 0..12_u8 {
        let c = Vec2::new(f32::from(i) * 0.01 - 0.06, 0.0);
        let p = square(&mut list, c, 0.008);
        list.push(Cmd::PushTransform(Affine2::from_translation(Vec2::new(0.0, 0.002))));
        list.push(Cmd::PushClip(p));
        list.push(Cmd::Fill { path: p, paint: Paint::Radial { centre: c, radius: 0.01, stops }, opacity: 0.8 });
        list.push(Cmd::Fill {
            path: p,
            paint: Paint::Linear { from: c, to: c + Vec2::splat(0.01), stops },
            opacity: 1.0,
        });
        list.push(Cmd::Stroke {
            path: p,
            paint: Paint::Solid(Colour::rgb(9, 9, 9)),
            width: 0.002,
            cap: Cap::Butt,
            join: Join::Miter,
            opacity: 0.5,
        });
        list.stroke(p, Paint::Solid(Colour::rgb(200, 100, 50)), 0.001);
        list.push(Cmd::PopClip);
        list.push(Cmd::PopTransform);
    }
    list
}

#[test]
fn encoding_the_same_list_twice_is_byte_identical() {
    let list = busy_list();
    let (mut enc, mut a, mut b) = (SceneEncoder::new(), Scene::new(), Scene::new());
    encode(&list, &mut enc, &mut a).unwrap();
    encode(&list, &mut enc, &mut b).unwrap();
    assert!(a.encoding().n_paths > 0);
    assert!(same_encoding(&a, &b));
    // A fresh encoder agrees with a warm one.
    let mut c = Scene::new();
    encode(&list, &mut SceneEncoder::new(), &mut c).unwrap();
    assert!(same_encoding(&a, &c));
}

#[test]
fn bounds_under_two_nested_transforms_match_the_hand_worked_ones() {
    let mut list = DrawList::new();
    let path = list.path().polyline(&[Vec2::ZERO, Vec2::new(0.01, 0.0), Vec2::new(0.01, 0.005)]).close().finish();
    list.push(Cmd::PushTransform(Affine2::from_translation(Vec2::new(0.01, 0.02))));
    list.push(Cmd::PushTransform(Affine2::from_scale(Vec2::splat(2.0))));
    list.fill(path, Paint::Solid(Colour::rgb(0, 0, 0)));
    list.push(Cmd::PopTransform);
    list.push(Cmd::PopTransform);
    let (mut enc, mut scene) = (SceneEncoder::new(), Scene::new());
    assert!(enc.last_bounds().is_none());
    encode(&list, &mut enc, &mut scene).unwrap();
    // Plane: scale by 2, then translate: x in [0.01, 0.03], y in [0.02, 0.03]. Pixels: x = 500 x + 50, y = 25 - 500 y.
    let b = enc.last_bounds().unwrap();
    for (got, want) in [(b.x0, 55.0), (b.y0, 10.0), (b.x1, 65.0), (b.y1, 15.0)] {
        assert!((got - want).abs() < 1e-3, "{b:?}");
    }
}

/// Measured with vello 0.11 on the busy list: Vello allocates nothing in a warm re-encode either, because the
/// encoding streams keep their capacity across `Scene::reset`, so the bound is zero for the whole call.
#[test]
fn a_warm_reencode_allocates_nothing() {
    let list = busy_list();
    let (mut enc, mut scene) = (SceneEncoder::new(), Scene::new());
    for _ in 0..3 {
        encode(&list, &mut enc, &mut scene).unwrap();
    }
    let n = count_allocations(|| encode(&list, &mut enc, &mut scene).unwrap());
    assert_eq!(n, 0, "{n} allocations in a warm re-encode");
}
