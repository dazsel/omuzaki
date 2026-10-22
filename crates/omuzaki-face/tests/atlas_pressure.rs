//! The atlas-scale controller (#808): hysteresis, bounds, no flapping, no allocation, determinism, and the face system's
//! layer-scale setter changing the next raster's size.

// A counting allocator is the only way to *prove* an update allocates nothing; the workspace denies unsafe code by
// default. Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::time::Seconds;
use omuzaki_face::atlas::{AtlasConfig, AtlasController, AtlasPressure, AtlasScaleChange};
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{FaceSystem, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceLayerId, FaceView};
use omuzaki_face_mock::{MockPainter, mock_profile, mock_projection};

use AtlasPressure::{Over, Under, Within};

// ------------------------------------------------------------------------------------------------ allocation counting

struct Counting;

thread_local! {
    // `const`-initialised without destructors, so touching them never allocates (this runs inside the allocator).
    // Counting is per thread, so tests running side by side cannot see each other's allocations.
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

// ------------------------------------------------------------------------------------------------ helpers

const HOLD: u32 = 5;
const COOLDOWN: u32 = 20;

/// Half to full in quarters, moving after 5 updates of one pressure, 20 updates apart.
fn controller() -> AtlasController {
    AtlasController::new(AtlasConfig { min_scale: 0.5, max_scale: 1.0, step: 0.25, hold: HOLD, cooldown: COOLDOWN })
        .unwrap()
}

/// Feeds `pressure` `n` times and returns the update index (from 1) and change of every change.
fn feed(c: &mut AtlasController, pressure: AtlasPressure, n: u32) -> Vec<(u32, AtlasScaleChange)> {
    (1..=n).filter_map(|i| c.update(pressure).map(|change| (i, change))).collect()
}

// ------------------------------------------------------------------------------------------------ the controller

#[test]
fn over_held_steps_down_once_and_again_only_after_the_cooldown() {
    let mut c = controller();
    let changes = feed(&mut c, Over, HOLD - 1);
    assert!(changes.is_empty(), "moved before the pressure held: {changes:?}");
    let changes = feed(&mut c, Over, 1);
    assert_eq!(changes, vec![(1, AtlasScaleChange { from: 1.0, to: 0.75 })]);
    // Over keeps holding: nothing through the cooldown, then the next step on the first update after it.
    let changes = feed(&mut c, Over, COOLDOWN + 1);
    assert_eq!(changes, vec![(COOLDOWN + 1, AtlasScaleChange { from: 0.75, to: 0.5 })]);
    assert_eq!(c.scale(), 0.5);
}

#[test]
fn alternating_pressure_never_moves_the_scale() {
    let mut c = controller();
    for i in 0..1000 {
        assert_eq!(c.update(if i % 2 == 0 { Over } else { Under }), None, "update {i}");
    }
    // Within between runs breaks them too.
    for i in 0..1000 {
        assert_eq!(c.update([Over, Over, Within][i % 3]), None, "update {i}");
    }
    assert_eq!(c.scale(), 1.0);
}

#[test]
fn the_scale_stays_inside_its_bounds() {
    let mut c = controller();
    let downs = feed(&mut c, Over, 1000);
    assert_eq!(downs.len(), 2, "1.0 to 0.75 to 0.5, then the floor: {downs:?}");
    assert_eq!(c.scale(), 0.5);
    let ups = feed(&mut c, Under, 1000);
    assert_eq!(
        ups.iter().map(|(_, change)| *change).collect::<Vec<_>>(),
        vec![AtlasScaleChange { from: 0.5, to: 0.75 }, AtlasScaleChange { from: 0.75, to: 1.0 }]
    );
    assert_eq!(c.scale(), 1.0);
    assert!(feed(&mut c, Under, 1000).is_empty(), "Under at full scale changes nothing");
}

#[test]
fn a_step_that_does_not_divide_the_range_stops_at_the_floor() {
    let config = AtlasConfig { min_scale: 0.5, max_scale: 1.0, step: 0.3, hold: 2, cooldown: 0 };
    let mut c = AtlasController::new(config).unwrap();
    let scales: Vec<f32> = feed(&mut c, Over, 100).iter().map(|(_, change)| change.to).collect();
    assert_eq!(scales, vec![0.7, 0.5]);
}

#[test]
fn updates_allocate_nothing_and_repeat_exactly() {
    let script: Vec<AtlasPressure> =
        (0..5000_u32).map(|i| [Over, Over, Over, Within, Under][((i / 37) % 5) as usize]).collect();
    let run = || {
        let mut c = AtlasController::new(AtlasConfig::default()).unwrap();
        let mut changes = Vec::with_capacity(script.len());
        let allocations = count_allocations(|| {
            for &pressure in &script {
                changes.push(c.update(pressure));
            }
        });
        (changes, allocations)
    };
    let (first, allocations) = run();
    assert_eq!(allocations, 0);
    assert!(first.iter().any(Option::is_some), "the script moves the scale");
    assert_eq!(run().0, first, "the same pressures give the same changes");
}

#[test]
fn a_bad_config_is_refused_with_its_field() {
    let base = AtlasConfig::default();
    for (config, field) in [
        (AtlasConfig { min_scale: 0.0, ..base }, "min_scale"),
        (AtlasConfig { max_scale: 0.25, ..base }, "max_scale"),
        (AtlasConfig { step: f32::NAN, ..base }, "step"),
        (AtlasConfig { hold: 1, ..base }, "hold"),
    ] {
        let error = AtlasController::new(config).unwrap_err().to_string();
        assert!(error.contains(field), "{error}");
    }
}

// ------------------------------------------------------------------------------------------------ the setter

#[test]
fn after_the_setter_the_next_raster_has_the_new_size() {
    let profile = mock_profile();
    let mut system =
        FaceSystem::real(RealPainter::new(mock_projection(), DEFAULT_PX_PER_M), Box::new(SkiaRasterizer::new()));
    let channels = ChannelBuf::default();
    let size = |system: &FaceSystem, layer| {
        let raster = system.raster(layer).unwrap();
        (raster.width, raster.height)
    };
    system.update(&channels, &profile, Seconds(0.0), FaceView::default()).unwrap();
    let full = size(&system, FaceLayerId::Base);
    let full_brows = size(&system, FaceLayerId::Brows);
    assert!(!system.update(&channels, &profile, Seconds(0.0), FaceView::default()).unwrap().any_changed());

    system.set_layer_scale(0.5).unwrap();
    let update = system.update(&channels, &profile, Seconds(0.0), FaceView::default()).unwrap();
    for layer in FaceLayerId::ALL {
        assert!(update.is_changed(layer), "{layer:?} is reported changed after the scale changed");
    }
    let half = size(&system, FaceLayerId::Base);
    assert!(half.0.abs_diff(full.0 / 2) <= 1 && half.1.abs_diff(full.1 / 2) <= 1, "{full:?} at half is {half:?}");
    let half_brows = size(&system, FaceLayerId::Brows);
    assert!(half_brows.0.abs_diff(full_brows.0 / 2) <= 1, "{full_brows:?} at half is {half_brows:?}");
    let spec = system.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    assert_eq!(spec.size_px(), half, "the specs the stage makes textures from follow");

    system.set_layer_scale(1.0).unwrap();
    system.update(&channels, &profile, Seconds(0.0), FaceView::default()).unwrap();
    assert_eq!(size(&system, FaceLayerId::Base), full, "back to full scale, back to the full size");
}

#[test]
fn the_setter_refuses_a_bad_scale_and_a_painter_it_cannot_scale() {
    let mut system =
        FaceSystem::real(RealPainter::new(mock_projection(), DEFAULT_PX_PER_M), Box::new(SkiaRasterizer::new()));
    for scale in [0.0, -0.5, 1.5, f32::NAN] {
        assert!(system.set_layer_scale(scale).is_err(), "{scale}");
    }
    let mut mock = FaceSystem::new(Box::new(MockPainter::new(mock_projection())), Box::new(SkiaRasterizer::new()));
    assert!(mock.set_layer_scale(0.5).is_err());
}
