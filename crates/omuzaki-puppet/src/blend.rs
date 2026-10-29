//! The blend modes as pure functions: the CPU reference every renderer is checked against (#1289).
//!
//! Pixels are premultiplied RGBA, colour values sRGB-encoded in `0..1`, and blending happens in that encoding, as the
//! painting tools the art comes from blend. The separable modes follow the W3C compositing formula: with `s` the
//! source and `d` the destination (premultiplied), `Cs = s.rgb / s.a`, `Cd = d.rgb / d.a`,
//!
//! ```text
//! out.rgb = (1 - d.a) * s.rgb + (1 - s.a) * d.rgb + s.a * d.a * B(Cd, Cs)
//! out.a   = s.a + d.a - s.a * d.a
//! ```
//!
//! where `B` is the mode's mix of two colours. `Normal` is source-over (`B = Cs`). `ClipToLower` is source-atop (the
//! source only where the destination is, keeping its alpha) and `SliceFromLower` is destination-out.

use omuzaki_puppet_api::BlendMode;

/// A premultiplied pixel.
pub type Px = [f32; 4];

fn hard_light(cd: f32, cs: f32) -> f32 {
    if cs <= 0.5 { cd * 2.0 * cs } else { screen(cd, 2.0 * cs - 1.0) }
}

fn screen(cd: f32, cs: f32) -> f32 {
    cd + cs - cd * cs
}

fn soft_light(cd: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cd - (1.0 - 2.0 * cs) * cd * (1.0 - cd)
    } else {
        let d = if cd <= 0.25 { ((16.0 * cd - 12.0) * cd + 4.0) * cd } else { cd.sqrt() };
        cd + (2.0 * cs - 1.0) * (d - cd)
    }
}

/// The mix function `B(Cd, Cs)` of a separable mode, on straight colours.
#[must_use]
pub fn mix(mode: BlendMode, cd: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Multiply => cd * cs,
        BlendMode::Screen => screen(cd, cs),
        BlendMode::Overlay => hard_light(cs, cd),
        BlendMode::Darken => cd.min(cs),
        BlendMode::Lighten => cd.max(cs),
        BlendMode::ColorDodge => {
            if cd <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cd / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cd >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cd) / cs).min(1.0)
            }
        }
        BlendMode::LinearDodge => (cd + cs).min(1.0),
        BlendMode::HardLight => hard_light(cd, cs),
        BlendMode::SoftLight => soft_light(cd, cs),
        BlendMode::Difference => (cd - cs).abs(),
        BlendMode::Exclusion => cd + cs - 2.0 * cd * cs,
        BlendMode::Subtract => (cd - cs).max(0.0),
        // Normal and the two Porter-Duff modes don't mix colours.
        _ => cs,
    }
}

/// `s` blended onto `d` with `mode`.
#[must_use]
pub fn blend(mode: BlendMode, s: Px, d: Px) -> Px {
    let (sa, da) = (s[3], d[3]);
    match mode {
        BlendMode::Normal => {
            let k = 1.0 - sa;
            [s[0] + d[0] * k, s[1] + d[1] * k, s[2] + d[2] * k, sa + da * k]
        }
        BlendMode::ClipToLower => {
            let k = 1.0 - sa;
            [s[0] * da + d[0] * k, s[1] * da + d[1] * k, s[2] * da + d[2] * k, da]
        }
        BlendMode::SliceFromLower => {
            let k = 1.0 - sa;
            [d[0] * k, d[1] * k, d[2] * k, da * k]
        }
        _ => {
            if sa <= 0.0 {
                return d;
            }
            let mut out = [0.0, 0.0, 0.0, sa + da - sa * da];
            for c in 0..3 {
                let cs = s[c] / sa;
                let cd = if da > 0.0 { d[c] / da } else { 0.0 };
                out[c] = (1.0 - da) * s[c] + (1.0 - sa) * d[c] + sa * da * mix(mode, cd, cs);
            }
            out
        }
    }
}

/// A premultiplied pixel with a multiplied `tint`, a screened `screen` tint and a factor `k` (opacity times mask).
#[must_use]
pub fn shade(s: Px, tint: [f32; 3], screen: [f32; 3], k: f32) -> Px {
    let a = s[3];
    let mut out = [0.0, 0.0, 0.0, a * k];
    for c in 0..3 {
        let tinted = s[c] * tint[c];
        // Screen on the straight colour: Cs + S - Cs*S, premultiplied by a.
        out[c] = (tinted + screen[c] * (a - tinted)) * k;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [BlendMode; 16] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::LinearDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::ClipToLower,
        BlendMode::SliceFromLower,
    ];

    fn close(a: Px, b: Px) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn opaque_layers_follow_the_mode_formulas() {
        let d = [0.6, 0.2, 0.9, 1.0];
        let s = [0.5, 0.5, 0.25, 1.0];
        assert!(close(blend(BlendMode::Normal, s, d), s));
        assert!(close(blend(BlendMode::Multiply, s, d), [0.3, 0.1, 0.225, 1.0]));
        assert!(close(blend(BlendMode::Screen, s, d), [0.8, 0.6, 0.925, 1.0]));
        assert!(close(blend(BlendMode::Darken, s, d), [0.5, 0.2, 0.25, 1.0]));
        assert!(close(blend(BlendMode::Lighten, s, d), [0.6, 0.5, 0.9, 1.0]));
        assert!(close(blend(BlendMode::LinearDodge, s, d), [1.0, 0.7, 1.0, 1.0]));
        assert!(close(blend(BlendMode::Difference, s, d), [0.1, 0.3, 0.65, 1.0]));
        assert!(close(blend(BlendMode::Subtract, s, d), [0.1, 0.0, 0.65, 1.0]));
        assert!(close(blend(BlendMode::ColorDodge, s, d), [1.0, 0.4, 1.0, 1.0]));
        assert!(close(blend(BlendMode::Overlay, s, d), [0.6, 0.2, 0.85, 1.0]));
    }

    #[test]
    fn the_golden_table_on_half_grey() {
        // src = (0.8, 0.4, 0.2, 1), dst = (0.5, 0.5, 0.5, 1): both opaque, so out = B(0.5, Cs) per channel.
        let s = [0.8, 0.4, 0.2, 1.0];
        let d = [0.5, 0.5, 0.5, 1.0];
        let soft = 0.5 + 0.6 * (0.5_f32.sqrt() - 0.5); // Cs = 0.8: Cd + (2Cs - 1)(sqrt(Cd) - Cd)
        let table = [
            (BlendMode::Normal, [0.8, 0.4, 0.2]),
            (BlendMode::Multiply, [0.4, 0.2, 0.1]),           // Cd * Cs
            (BlendMode::Screen, [0.9, 0.7, 0.6]),             // Cd + Cs - Cd * Cs
            (BlendMode::Overlay, [0.8, 0.4, 0.2]),            // Cd = 0.5: 2 * Cd * Cs = Cs
            (BlendMode::Darken, [0.5, 0.4, 0.2]),             // min
            (BlendMode::Lighten, [0.8, 0.5, 0.5]),            // max
            (BlendMode::ColorDodge, [1.0, 0.5 / 0.6, 0.625]), // min(1, Cd / (1 - Cs)): 2.5 clamps
            (BlendMode::LinearDodge, [1.0, 0.9, 0.7]),        // min(1, Cd + Cs)
            (BlendMode::ColorBurn, [0.375, 0.0, 0.0]),        // 1 - min(1, (1 - Cd) / Cs)
            (BlendMode::HardLight, [0.8, 0.4, 0.2]),          // Cs > 0.5: screen(0.5, 0.6) = 0.8; else 2 * 0.5 * Cs
            (BlendMode::SoftLight, [soft, 0.45, 0.35]),       // Cs <= 0.5: Cd - (1 - 2Cs) Cd (1 - Cd)
            (BlendMode::Difference, [0.3, 0.1, 0.3]),         // |Cd - Cs|
            (BlendMode::Exclusion, [0.5, 0.5, 0.5]),          // Cd + Cs - 2 Cd Cs, with Cd = 0.5
            (BlendMode::Subtract, [0.0, 0.1, 0.3]),           // max(0, Cd - Cs)
        ];
        for (mode, rgb) in table {
            let want = [rgb[0], rgb[1], rgb[2], 1.0];
            assert!(close(blend(mode, s, d), want), "{mode:?}: {:?} != {want:?}", blend(mode, s, d));
        }
        // Half-transparent src (straight (0.8, 0.4, 0.2), alpha 0.5, so premultiplied (0.4, 0.2, 0.1, 0.5)) on the
        // opaque grey: out = 0.5 * d + 0.5 * B, alpha 1; Normal is s + 0.5 * d.
        let half = [0.4, 0.2, 0.1, 0.5];
        assert!(close(blend(BlendMode::Normal, half, d), [0.65, 0.45, 0.35, 1.0]));
        assert!(close(blend(BlendMode::Multiply, half, d), [0.45, 0.35, 0.3, 1.0]));
        assert!(close(blend(BlendMode::Screen, half, d), [0.7, 0.6, 0.55, 1.0]));
    }

    #[test]
    fn symmetric_modes_commute_and_outputs_stay_finite_in_range() {
        let steps: Vec<f32> = (0..=10_u8).map(|i| f32::from(i) / 10.0).collect();
        for &a in &steps {
            for &b in &steps {
                for mode in [
                    BlendMode::Multiply,
                    BlendMode::Screen,
                    BlendMode::Darken,
                    BlendMode::Lighten,
                    BlendMode::LinearDodge,
                    BlendMode::Difference,
                    BlendMode::Exclusion,
                ] {
                    assert!((mix(mode, a, b) - mix(mode, b, a)).abs() < 1e-6, "{mode:?} at {a}, {b}");
                }
                for mode in MODES {
                    let m = mix(mode, a, b);
                    assert!(m.is_finite() && (-1e-6..=1.0 + 1e-6).contains(&m), "{mode:?} at {a}, {b}: {m}");
                }
            }
        }
    }

    #[test]
    fn every_separable_mode_over_nothing_is_the_source_and_nothing_over_anything_keeps_it() {
        let s = [0.2, 0.3, 0.1, 0.5];
        let d = [0.4, 0.1, 0.3, 0.8];
        for mode in MODES {
            if !matches!(mode, BlendMode::ClipToLower | BlendMode::SliceFromLower) {
                assert!(close(blend(mode, s, [0.0; 4]), s), "{mode:?} over transparent");
            }
            assert!(close(blend(mode, [0.0; 4], d), d), "transparent {mode:?}");
        }
    }

    #[test]
    fn clip_keeps_the_lower_alpha_and_slice_cuts_it() {
        let s = [0.5, 0.0, 0.0, 0.5];
        let d = [0.0, 0.0, 0.4, 0.4];
        let clip = blend(BlendMode::ClipToLower, s, d);
        assert!((clip[3] - 0.4).abs() < 1e-6);
        assert!(close(blend(BlendMode::ClipToLower, s, [0.0; 4]), [0.0; 4]), "nothing below, nothing drawn");
        assert!(close(blend(BlendMode::SliceFromLower, s, d), [0.0, 0.0, 0.2, 0.2]));
    }

    #[test]
    fn results_stay_premultiplied() {
        let samples = [0.0, 0.1, 0.5, 0.9, 1.0];
        for mode in MODES {
            for &sa in &samples {
                for &da in &samples {
                    for &c in &samples {
                        let out = blend(mode, [c * sa, 0.3 * sa, sa, sa], [0.7 * da, c * da, 0.2 * da, da]);
                        assert!(out[3] <= 1.0 + 1e-5 && out[3] >= -1e-5, "{mode:?} alpha {out:?}");
                        assert!(out[..3].iter().all(|&v| v <= out[3] + 1e-5 && v >= -1e-5), "{mode:?} {out:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn shading_tints_screens_and_fades() {
        let s = [0.5, 0.25, 0.0, 0.5];
        assert!(close(shade(s, [1.0; 3], [0.0; 3], 1.0), s));
        assert!(close(shade(s, [0.5, 1.0, 1.0], [0.0; 3], 1.0), [0.25, 0.25, 0.0, 0.5]));
        assert!(close(shade(s, [1.0; 3], [1.0; 3], 1.0), [0.5, 0.5, 0.5, 0.5]), "a white screen lifts to white");
        assert!(close(shade(s, [1.0; 3], [0.0; 3], 0.5), [0.25, 0.125, 0.0, 0.25]));
    }
}
