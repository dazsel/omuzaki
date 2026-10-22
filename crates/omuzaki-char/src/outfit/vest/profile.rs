//! The vest's outline: where its rim (neck and V) and hem run round the torso, as heights along the torso region.
//!
//! The prototype cuts the vest from height functions of the angle round the body (`vestTop`, `vestHem`, `vestAngles`).
//! Here every height is a fraction `t` of the torso region, so the vest follows whatever torso the fit has; the
//! placeholders below are the prototype's metres on the soft-sd body ([`along`] converts), kept as named constants in
//! this one place.

use std::f64::consts::{FRAC_PI_2, PI};

use crate::kit::curve::{fraction, lerp, sstep};

/// The soft-sd torso's bottom, metres: the height `t = 0` is at on the body the placeholders were fitted to.
const REFERENCE_BOTTOM: f64 = 0.325;
/// The soft-sd torso's length, metres.
const REFERENCE_LENGTH: f64 = 0.22;

/// The fraction of the torso region a height of the soft-sd body (metres) is at: how the placeholders are written.
pub const fn along(y: f64) -> f64 {
    (y - REFERENCE_BOTTOM) / REFERENCE_LENGTH
}

/// A length of the soft-sd body (metres) as a fraction of its torso.
pub const fn of_torso(length: f64) -> f64 {
    length / REFERENCE_LENGTH
}

/// Placeholder: the rim's height at the shoulders (`VY.topY`).
pub const RIM_HEIGHT: f64 = along(0.5265);
/// Placeholder: how far the V tip is below the rim (`VY.topY - VY.tipV`).
pub const NECK_DEPTH: f64 = of_torso(0.5265 - 0.432);
/// Placeholder: the hem's height at the centre front (`VY.tipHem`).
pub const HEM_FRONT: f64 = along(0.336);
/// Placeholder: the hem's height at the sides (`VY.sideHem`).
pub const HEM_SIDE: f64 = along(0.362);
/// Placeholder: the hem's height at the centre back (the prototype's `0.356`).
pub const HEM_BACK: f64 = along(0.356);
/// Placeholder: how far the rim dips at the back of the neck (the prototype's `0.0125`).
pub const BACK_DIP: f64 = of_torso(0.0125);

/// Placeholder: how far the armholes cut down from the rim at the sides, as a fraction of the torso. The prototype has no
/// armholes (its sleeves are the shirt's); here the arm hangs against the side of the torso, so the vest must leave it
/// room from the underarm up (0.46 of the torso puts the floor 0.456 up it, 0.425 m on the soft-sd body).
pub const ARMHOLE_DEPTH: f64 = 0.46;

/// The front rim reaches the shoulder height at this `|sin phi|` (about 47 degrees either side of the V).
const V_KNEE: f64 = 0.74;
/// The curve of the V's sides.
const V_POWER: f64 = 1.1;
/// The front hem reaches the side height at this `|sin phi|`.
const HEM_KNEE: f64 = 0.85;
/// The curve of the front hem.
const HEM_POWER: f64 = 0.9;
/// The back rim dips within this `|sin phi|` of the centre back.
const DIP_KNEE: f64 = 0.60;
/// How the angles bunch toward the front: `phi` goes as `|u|^FRONT_BIAS` for `u` from -1 to 1.
const FRONT_BIAS: f64 = 1.5;

/// The armhole's floor is flat within this many radians of the side (`phi = +-pi/2`)...
const ARMHOLE_FLAT: f64 = 0.40;
/// ...and climbs back to the rim over this many more.
const ARMHOLE_RAMP: f64 = 0.20;

/// The angles within this of an armhole's ramp get [`ARMHOLE_SPLIT`] times the columns, so the steep climb out of an
/// armhole is cut into many small quads. Each column's rows run evenly from its hem to its rim, so where the rim climbs
/// fast the rows of neighbouring columns are far apart in height, and a quad between them is a long sheared one whose
/// diagonal chords across the chest's curve (into the body, on a broad chest). The flat floor needs no split.
const ARMHOLE_ZONE: f64 = 0.1;
/// How many columns replace each of the prototype's on the armhole's ramp.
const ARMHOLE_SPLIT: usize = 5;

/// The least gap between the V tip and the highest hem, as a fraction of the torso: under this the panel is a sliver.
pub const MIN_PANEL: f64 = 0.08;

/// The vest's outline, as heights along the torso region (`0` at its bottom, `1` at its top).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Profile {
    /// The rim's height at the shoulders and the sides of the neck.
    pub rim: f64,
    /// How far the V tip is below the rim.
    pub neck_depth: f64,
    /// The hem at the centre front.
    pub hem_front: f64,
    /// The hem at the sides.
    pub hem_side: f64,
    /// The hem at the centre back.
    pub hem_back: f64,
    /// How far the armholes cut down from the rim at the sides; 0 for none (the prototype's).
    pub armhole: f64,
}

impl Profile {
    /// The V tip's height: the rim's lowest point, at the centre front.
    pub fn tip(&self) -> f64 {
        self.rim - self.neck_depth
    }

    /// The rim's height at angle `phi` (0 at the front): a power-curve notch in the front half, a shallow dip in the
    /// back half, and an armhole cut into each side.
    pub fn top(&self, phi: f64) -> f64 {
        let (s, c) = (phi.sin().abs(), phi.cos());
        let neck = if c >= 0.0 {
            self.tip() + self.neck_depth * (s / V_KNEE).min(1.0).powf(V_POWER)
        } else {
            self.rim - BACK_DIP * (1.0 - s / DIP_KNEE).max(0.0).powi(2)
        };
        neck - self.armhole * armhole_cut(phi)
    }

    /// The hem's height at angle `phi`. The two halves meet at the sides.
    pub fn hem(&self, phi: f64) -> f64 {
        let (s, c) = (phi.sin().abs(), phi.cos());
        if c >= 0.0 {
            self.hem_front + (self.hem_side - self.hem_front) * (s / HEM_KNEE).min(1.0).powf(HEM_POWER)
        } else {
            self.hem_back + (self.hem_side - self.hem_back) * s * s
        }
    }

    /// The height at fraction `f` of the way from the hem (0) to the rim (1) at angle `phi`.
    pub fn between(&self, phi: f64, f: f64) -> f64 {
        let hem = self.hem(phi);
        hem + f * (self.top(phi) - hem)
    }

    /// The angles the shell's columns are at: the prototype's [`angles`], with each interval near an armhole's ramp cut
    /// into [`ARMHOLE_SPLIT`] when there is one. The first and last are the same point at the back.
    pub fn columns(&self, n: usize) -> Vec<f64> {
        let base = angles(n);
        if self.armhole <= 0.0 {
            return base;
        }
        let near = |phi: f64| {
            let from_side = (phi.abs() - FRAC_PI_2).abs();
            (ARMHOLE_FLAT - ARMHOLE_ZONE..ARMHOLE_FLAT + ARMHOLE_RAMP + ARMHOLE_ZONE).contains(&from_side)
        };
        let mut out = vec![base[0]];
        for pair in base.windows(2) {
            let split = if near(f64::midpoint(pair[0], pair[1])) { ARMHOLE_SPLIT } else { 1 };
            out.extend((1..=split).map(|k| lerp(pair[0], pair[1], fraction(k, split))));
        }
        out
    }

    /// The least height of the rim over the highest hem: the panel's smallest extent, as a fraction of the torso.
    pub fn panel(&self) -> f64 {
        let hems = self.hem_front.max(self.hem_side).max(self.hem_back);
        (self.tip() - hems).min(self.rim - BACK_DIP - hems).min(self.rim - self.armhole - hems)
    }
}

/// How much of the armhole is cut at `phi`: 1 on its floor, easing to 0 at its edges.
fn armhole_cut(phi: f64) -> f64 {
    let from_side = (phi.abs() - FRAC_PI_2).abs();
    1.0 - sstep(ARMHOLE_FLAT, ARMHOLE_FLAT + ARMHOLE_RAMP, from_side)
}

/// `n + 1` angles from `-pi` to `pi`, bunched toward the front where the V tip needs the rows (the prototype's
/// `vestAngles`). The first and last are the same point at the back.
pub fn angles(n: usize) -> Vec<f64> {
    (0..=n)
        .map(|k| {
            let u = crate::kit::curve::fraction(k, n) * 2.0 - 1.0;
            u.signum() * u.abs().powf(FRONT_BIAS) * PI
        })
        .collect()
}
