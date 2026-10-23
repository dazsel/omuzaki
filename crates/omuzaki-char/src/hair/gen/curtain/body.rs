//! Keeping the curtain off the body it hangs over.
//!
//! The curtain's envelope is traced from one character's silhouette, so on a broader body the hair would lie inside the
//! shoulders and upper arms. [`BodyKeepOut`] samples the body fit's regions (torso, arms, legs) as chains of elliptical
//! sections and moves a point that lies inside one out to its surface plus a margin. A point that is already clear is
//! returned untouched, so the prototype's own numbers survive wherever the hair clears the body.

use omuzaki_char_api::{BodyFit, BodyRegion, Section};
use omuzaki_core::glam::{Vec2, Vec3};

/// The regions hair can hang over: the neck is left out, because the guides root on the head just above it.
const REGIONS: [BodyRegion; 5] =
    [BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR, BodyRegion::LegL, BodyRegion::LegR];
/// Sections sampled along each region (end to end, so `SAMPLES - 1` segments).
const SAMPLES: usize = 64;
/// Passes over the regions: a point pushed out of the torso can land in an arm.
const PASSES: usize = 3;
/// How far past a chain's end a point still counts as in its end section, metres: a region ends in a cross-section, not
/// in a cliff, so a point just past it is in the way too.
const END_SLACK: f32 = 0.006;
/// How far a wedged point is carried back at a time, metres, and the most steps it takes.
const BACK_STEP: f32 = 0.004;
const BACK_STEPS: usize = 100;
/// How far into the next segment a segment's slice reaches, as a fraction of the segment.
const BEND_SLACK: f32 = 0.25;
/// The hair hangs this far behind the arms' back surface, metres, and eases onto that plane over [`BEHIND_SOFT`].
const BEHIND_MARGIN: f32 = 0.012;
const BEHIND_SOFT: f32 = 0.04;
/// Radii below this make the ellipse test meaningless.
const MIN_RADIUS: f32 = 1e-4;

/// The body's regions as chains of sections, for pushing points out of them.
#[derive(Clone, Debug)]
pub struct BodyKeepOut {
    chains: Vec<Vec<Section>>,
    /// The back surface of the arms: the frontal plane the hair hangs behind (`None` for a fit without arms).
    behind_arms: Option<f32>,
}

impl BodyKeepOut {
    /// The keep-out of `fit`: every region it reports, sampled end to end. A fit that reports none gives an empty keep-out.
    pub fn new(fit: &dyn BodyFit) -> Self {
        let chains = REGIONS
            .iter()
            .map(|region| (0..SAMPLES).filter_map(|i| fit.section(*region, sample_t(i))).collect::<Vec<Section>>())
            .filter(|chain| chain.len() >= 2)
            .collect::<Vec<Vec<Section>>>();
        // The arms' back surface, over their upper part, where the hair would otherwise hang down their outside.
        let behind_arms = [BodyRegion::ArmL, BodyRegion::ArmR]
            .iter()
            .flat_map(|region| (0..SAMPLES / 2).filter_map(move |i| fit.section(*region, sample_t(i))))
            .map(|s| s.centre.z - s.radii.y)
            .reduce(f32::min);
        Self { chains, behind_arms }
    }

    /// `p` drawn back behind the arms, for hair that falls past the shoulders: below `from_y` (the head's lowest point)
    /// the further it is the more its `z` is compressed (smoothly, so no strand folds) toward the arms' back surface
    /// less `BEHIND_MARGIN`; by `over` metres below it the compression is complete. A fit without arms, or a point
    /// already behind, is left alone.
    pub fn hang_behind(&self, p: Vec3, from_y: f32, over: f32) -> Vec3 {
        let Some(back) = self.behind_arms else { return p };
        if !p.is_finite() || over <= 0.0 {
            return p;
        }
        let limit = back - BEHIND_MARGIN;
        let start = limit - BEHIND_SOFT;
        let t = ((from_y - p.y) / over).clamp(0.0, 1.0);
        let weight = t * t * (3.0 - 2.0 * t);
        if p.z <= start || weight <= 0.0 {
            return p;
        }
        let compressed = start + BEHIND_SOFT * ((p.z - start) / BEHIND_SOFT).tanh();
        Vec3::new(p.x, p.y, p.z + (compressed - p.z) * weight)
    }

    /// The `z` of the arms' back surface, for tests.
    pub fn behind_arms(&self) -> Option<f32> {
        self.behind_arms
    }

    /// `p` moved out of every region by the smallest amount that leaves it `margin` clear of the region's surface;
    /// `p` itself if it is already clear. Non-finite points are returned as they are.
    pub fn push_out(&self, p: Vec3, margin: f32) -> Vec3 {
        if !p.is_finite() {
            return p;
        }
        let mut p = self.push_each(p, margin);
        // A point in a wedge between two regions (the armpit) is pushed from one into the other, whatever the number of
        // passes. Hair cannot lie there either, so it is carried back, off the torso and the arm together.
        for _ in 0..BACK_STEPS {
            if self.push_each(p, margin) == p {
                break;
            }
            p.z -= BACK_STEP;
        }
        p
    }

    /// `p` pushed out of each region in turn, a few times over.
    fn push_each(&self, p: Vec3, margin: f32) -> Vec3 {
        let mut p = p;
        for _ in 0..PASSES {
            let before = p;
            for chain in &self.chains {
                p = push_from(chain, p, margin);
            }
            if p == before {
                break;
            }
        }
        p
    }
}

#[allow(clippy::cast_precision_loss)] // a few dozen samples
fn sample_t(i: usize) -> f32 {
    i as f32 / (SAMPLES - 1) as f32
}

/// Every section of `chain` whose slice `p` lies in: one per segment `p` projects onto the span of (so two at a bend,
/// where the region doubles back on itself), interpolated, with the chain's two ends given [`END_SLACK`].
fn sections_at(chain: &[Section], p: Vec3) -> Vec<Section> {
    let mut found = Vec::new();
    for (i, pair) in chain.windows(2).enumerate() {
        let [a, b] = pair else { continue };
        let span = b.centre - a.centre;
        let length_squared = span.length_squared();
        if length_squared < 1e-12 {
            continue;
        }
        let s = (p - a.centre).dot(span) / length_squared;
        // The planes of two neighbouring segments leave a wedge between them where the chain bends, so each segment
        // reaches a little into its neighbours'; the two ends of the chain reach out by [`END_SLACK`].
        let end = END_SLACK / length_squared.sqrt();
        let low = if i == 0 { -end } else { -BEND_SLACK };
        let high = if i + 2 == chain.len() { 1.0 + end } else { 1.0 + BEND_SLACK };
        if !(low..=high).contains(&s) {
            continue;
        }
        let s = s.clamp(0.0, 1.0);
        let axis = a.axis.lerp(b.axis, s).try_normalize().unwrap_or(a.axis);
        found.push(Section { centre: a.centre.lerp(b.centre, s), axis, radii: a.radii.lerp(b.radii, s) });
    }
    found
}

/// `p` pushed out of each of the chain's ellipses at its height that it is inside.
fn push_from(chain: &[Section], p: Vec3, margin: f32) -> Vec3 {
    sections_at(chain, p).into_iter().fold(p, |p, section| push_from_section(&section, p, margin))
}

/// `p` pushed out of one section's ellipse, if it is inside.
fn push_from_section(section: &Section, p: Vec3, margin: f32) -> Vec3 {
    let axis = section.axis;
    // The ellipse's own frame: `x` across the body (world X flattened into the plane), `y` through it.
    let across = (Vec3::X - axis * axis.x).try_normalize().unwrap_or(Vec3::Z);
    let through = axis.cross(across);
    let radii = section.radii.max(Vec2::splat(MIN_RADIUS)) + Vec2::splat(margin);
    let offset = p - section.centre;
    let along = axis * offset.dot(axis);
    let local = Vec2::new(offset.dot(across), offset.dot(through));
    let rho = (local / radii).length();
    if rho >= 1.0 {
        return p;
    }
    // On the axis there is no direction to leave by: leave backward, where the hair hangs.
    let leave = if rho < 1e-4 { Vec2::new(0.0, -radii.y) } else { local / rho };
    section.centre + along + across * leave.x + through * leave.y
}
