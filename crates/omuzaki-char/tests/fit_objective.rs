//! The fit's objective on its own: the true body scores close to 1 against its own synthetic silhouettes on every
//! template, so the recovery test's bar measures the search and not the alignment. Before the objective matched the
//! model's mean column to the target's, the box's middle (whole pixels at the target's ends) left the true body up to
//! half a pixel off sideways: 0.989 on `standard` with seed 2, below the recovery test's 0.985 bar once hands show.

#![allow(clippy::unwrap_used)] // fixtures that must exist

use omuzaki_char::fit::solver::{FitInput, PARAM_ORDER, iou_of};
use omuzaki_char::fit::{FitParams, FitView, Target, render_target};
use omuzaki_core::rng::Rng;

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];
const VIEWS: [FitView; 3] = [FitView::Front, FitView::Side, FitView::Back];
/// The solver recovery test's view height.
const HEIGHT: usize = 256;
/// How close to 1 the true body scores against its own silhouettes.
const TRUE_BODY_IOU: f64 = 0.995;

/// A seeded character drawn as the solver's recovery test draws its own (`tests/fit.rs`).
fn random_params(seed: u64) -> FitParams {
    let mut rng = Rng::new(seed);
    let mut p = FitParams::default();
    for param in PARAM_ORDER {
        let (lo, hi) = param.range();
        let half = (hi - lo) / 2.0 * 0.25;
        let v = param.get(&p) + f64::from(rng.range_f32(-1.0, 1.0)) * half;
        param.set(&mut p, v.clamp(lo, hi));
    }
    p
}

#[test]
fn the_true_body_scores_close_to_one_against_its_own_silhouettes() {
    for seed in [2, 5, 11] {
        let truth = random_params(seed);
        for template in TEMPLATES {
            let targets: Vec<Target> = VIEWS
                .iter()
                .map(|&view| Target { view, mask: render_target(template, &truth, &[], view, HEIGHT).unwrap() })
                .collect();
            let iou = iou_of(&FitInput { template, targets: &targets, fixed: &[] }, &truth).unwrap();
            assert!(iou >= TRUE_BODY_IOU, "seed {seed}, {template}: the true body scores {iou}");
        }
    }
}
