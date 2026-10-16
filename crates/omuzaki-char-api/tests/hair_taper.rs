//! The clump width profile: the default linear shape is the one every earlier file has, and the smooth and plateau
//! shapes reproduce the prototype's fringe outline and braid strand.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation)] // grid parameters are small

use omuzaki_char_api::{Taper, TaperShape};
use omuzaki_face_api::{Check, Validate};

/// The width the linear profile had before shapes existed (`start`, `mid`, `tip`, `tip_at`), in `f32`.
fn linear_before(t: &Taper, x: f32) -> f32 {
    let (start, mid, tip, tip_at) = (t.start as f32, t.mid as f32, t.tip as f32, t.tip_at as f32);
    let x = x.clamp(0.0, 1.0);
    if x < tip_at {
        start + (mid - start) * (x / tip_at)
    } else {
        mid + (tip - mid) * ((x - tip_at) / (1.0 - tip_at).max(f32::EPSILON)).powf(0.8)
    }
}

fn smoothstep(x: f64, a: f64, b: f64) -> f64 {
    let s = ((x - a) / (b - a)).clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

/// The prototype's fringe outline, in `f64`.
fn fringe(w: f64, t: f64) -> f64 {
    let a = 0.040;
    (a + (w - a) * smoothstep(t, 0.0, 0.45)) * (1.0 - smoothstep(t, 0.5, 1.0)).powf(0.7) + 0.0015
}

fn fringe_taper(w: f64) -> Taper {
    Taper::new(0.040, w, 0.0015).with_shape(TaperShape::smooth(0.45, 0.5))
}

fn errors(t: &Taper) -> Vec<String> {
    let mut c = Check::new();
    t.check(&mut c);
    c.finish().err().map(|e| e.0.iter().map(|p| p.path.clone()).collect()).unwrap_or_default()
}

#[test]
fn the_linear_shape_is_byte_identical_to_the_profile_before_shapes() {
    for taper in [
        Taper::default(),
        Taper::new(0.05, 0.06, 0.03),
        Taper { tip_at: 0.75, ..Taper::new(0.04, 0.06, 0.01) },
        Taper { tip_at: 0.95, ..Taper::new(0.03, 0.05, 0.006) },
    ] {
        for step in 0..=400 {
            let x = step as f32 / 400.0;
            assert_eq!(taper.at(x).to_bits(), linear_before(&taper, x).to_bits(), "t {x}");
        }
    }
}

#[test]
fn the_smooth_shape_reproduces_the_prototypes_fringe_outline() {
    for w in [0.064, 0.072, 0.022, 0.071, 0.057] {
        let taper = fringe_taper(w);
        for step in 0..=1000 {
            let t = f64::from(step) / 1000.0;
            let ours = f64::from(taper.at(t as f32));
            assert!((ours - fringe(w, t)).abs() < 1e-6, "w {w} t {t}: {ours} against {}", fringe(w, t));
        }
    }
}

#[test]
fn the_smooth_shape_hits_the_directors_reference_values() {
    let cases = [
        (0.064, [0.041500, 0.044529, 0.055492, 0.065500, 0.065500, 0.040897, 0.014625, 0.001500]),
        (0.072, [0.041500, 0.045538, 0.060156, 0.073500, 0.073500, 0.045821, 0.016266, 0.001500]),
        (0.022, [0.041500, 0.039228, 0.031006, 0.023500, 0.023500, 0.015043, 0.006012, 0.001500]),
    ];
    let ts = [0.0, 0.1, 0.25, 0.45, 0.5, 0.75, 0.9, 1.0];
    for (w, expected) in cases {
        let taper = fringe_taper(w);
        for (t, want) in ts.into_iter().zip(expected) {
            let got = f64::from(taper.at(t as f32));
            assert!((got - want).abs() < 1e-6, "w {w} t {t}: {got} against {want}");
        }
    }
}

#[test]
fn the_smooth_tip_is_exactly_the_floor_and_never_nan() {
    for w in [0.022, 0.3] {
        let taper = fringe_taper(w);
        assert_eq!(taper.at(1.0), 0.0015_f32);
        assert!((0..=100).all(|i| taper.at(i as f32 / 100.0).is_finite()));
    }
    // Edges that meet make a step, not a division by zero.
    let step = Taper::new(0.04, 0.06, 0.002).with_shape(TaperShape::smooth(0.0, 1.0));
    assert!((0..=100).all(|i| step.at(i as f32 / 100.0).is_finite()));
}

#[test]
fn the_plateau_shape_is_the_braid_strands_width() {
    let taper = Taper::new(0.012, 0.024, 0.0096).with_shape(TaperShape::plateau(0.04, 0.08));
    let prototype = |t: f64| {
        0.024
            * if t > 0.92 { 1.0 + (0.4 - 1.0) * (t - 0.92) / 0.08 } else { 1.0 }
            * if t < 0.04 { 0.5 + 0.5 * t / 0.04 } else { 1.0 }
    };
    for step in 0..=1000 {
        let t = f64::from(step) / 1000.0;
        assert!((f64::from(taper.at(t as f32)) - prototype(t)).abs() < 1e-6, "t {t}");
    }
    // Ramps of no length are a flat strand and do not divide by zero.
    let flat = Taper::new(0.012, 0.024, 0.0096).with_shape(TaperShape::plateau(0.0, 0.0));
    assert!((0..=100).all(|i| flat.at(i as f32 / 100.0) == 0.024_f32));
}

#[test]
fn files_without_a_shape_parse_and_save_the_same() {
    let old = r#"{"start":0.03,"mid":0.05,"tip":0.006,"tip_at":0.8}"#;
    let taper: Taper = serde_json::from_str(old).unwrap();
    assert_eq!(taper, Taper::new(0.03, 0.05, 0.006));
    assert_eq!(serde_json::to_string(&taper).unwrap(), old);
    assert_eq!(
        serde_json::to_string(&Taper::default()).unwrap(),
        r#"{"start":0.04,"mid":0.05,"tip":0.01,"tip_at":0.8}"#
    );
}

#[test]
fn every_shape_round_trips() {
    for shape in [TaperShape::Linear, TaperShape::smooth(0.45, 0.5), TaperShape::plateau(0.04, 0.08)] {
        let taper = Taper::default().with_shape(shape);
        let back: Taper = serde_json::from_str(&serde_json::to_string(&taper).unwrap()).unwrap();
        assert_eq!(back, taper);
    }
}

#[test]
fn bad_shape_values_are_named() {
    let smooth = |a, b| Taper::default().with_shape(TaperShape::smooth(a, b));
    assert!(errors(&smooth(0.45, 0.5)).is_empty());
    assert!(errors(&smooth(0.5, 0.5)).is_empty());
    assert_eq!(errors(&smooth(0.6, 0.5)), ["shape/taper_from"]);
    assert_eq!(errors(&smooth(-0.1, 0.5)), ["shape/fill_to"]);
    assert_eq!(errors(&smooth(0.2, 1.5)), ["shape/taper_from"]);
    assert_eq!(errors(&smooth(f64::NAN, 0.5)), ["shape/fill_to"]);
    let plateau = Taper::default().with_shape(TaperShape::plateau(0.6, f64::NAN));
    assert_eq!(errors(&plateau), ["shape/ramp_in", "shape/ramp_out"]);
}
