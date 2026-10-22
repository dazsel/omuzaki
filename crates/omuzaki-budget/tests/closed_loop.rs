//! The scheduler against the mock renderer: the renderer's reported cost follows the quality it was given, and the
//! scheduler has to bring the frame back inside the budget and then recover when the load goes away.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_budget::{Direction, Scheduler};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::Scene;
use omuzaki_render_api::{
    AlphaEncoding, Camera, FrameInfo, Pass, Quality, Renderer, ShadowQuality, TargetDesc, TargetId, budget,
};
use omuzaki_render_mock::MockRenderer;

/// A made-up cost model in ms: pixels dominate, shadows and post add a fixed amount.
fn cost_ms(q: &Quality, load: f64) -> f64 {
    let shadows = match q.shadows {
        ShadowQuality::Off => 0.0,
        ShadowQuality::Low => 0.3,
        ShadowQuality::High => 0.8,
    } / f64::from(q.shadow_interval);
    // Depth of field about ten times grain and grade, bloom between (#657).
    let post =
        [(q.post_effects.depth_of_field, 0.9), (q.post_effects.bloom, 0.25), (q.post_effects.grain_and_grade, 0.05)]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, ms)| ms)
            .sum::<f64>();
    let pixels = 1.4 * f64::from(q.render_scale).powi(2) * (0.5 + 0.125 * f64::from(q.samples));
    load * (pixels + shadows + post)
}

struct Rig {
    renderer: MockRenderer,
    target: TargetId,
    scene: Scene,
    scheduler: Scheduler,
    frame: u64,
}

impl Rig {
    fn new(requested: Quality, budget: Seconds) -> Self {
        let mut renderer = MockRenderer::new();
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight }).unwrap();
        Self {
            renderer,
            target,
            scene: Scene::new(Camera::default()),
            scheduler: Scheduler::new(requested, budget),
            frame: 0,
        }
    }

    /// One frame: the renderer reports what the current quality costs under `load`; the scheduler reacts.
    fn frame(&mut self, load: f64) -> Option<omuzaki_budget::Change> {
        self.frame += 1;
        self.scene.frame = FrameInfo { index: self.frame, time: Seconds(0.0) };
        let q = self.scheduler.quality();
        let ms = cost_ms(&q, load);
        self.renderer.set_pass_cost(Pass::Resolve, Seconds(0.0001), Seconds(ms / 1000.0));
        let report = self.renderer.render(self.target, &self.scene, &q).unwrap();
        self.scheduler.observe(&report)
    }

    fn run(&mut self, frames: u32, load: f64) -> Vec<omuzaki_budget::Change> {
        (0..frames).filter_map(|_| self.frame(load)).collect()
    }

    fn settled_ms(&self, load: f64) -> f64 {
        cost_ms(&self.scheduler.quality(), load)
    }
}

#[test]
fn an_overloaded_scene_is_brought_inside_the_budget_and_recovers() {
    let mut rig = Rig::new(Quality::high(), budget::HIGH_GPU);
    assert!(cost_ms(&Quality::high(), 1.0) > 4.0, "the editor quality starts over the 4 ms budget in this model");

    let downs = rig.run(3000, 1.0);
    assert!(!downs.is_empty() && downs.iter().all(|c| c.direction == Direction::Down));
    assert!(rig.settled_ms(1.0) <= 4.0, "settled at {} ms", rig.settled_ms(1.0));
    assert!(rig.run(3000, 1.0).is_empty(), "and then it stays put");

    // The load goes away (a lighter scene): it climbs back, one rung at a time, to the full request.
    let ups = rig.run(6000, 0.3);
    assert!(!ups.is_empty() && ups.iter().all(|c| c.direction == Direction::Up));
    assert_eq!(rig.scheduler.quality(), Quality::high());
}

#[test]
fn a_load_that_sits_in_the_dead_band_does_not_oscillate() {
    let mut rig = Rig::new(Quality::high(), budget::HIGH_GPU);
    rig.run(3000, 1.0);
    // A load right at the border of the next rung: whatever it does, it must not flap every few seconds.
    let changes: Vec<_> = (0..8000).filter_map(|_| rig.frame(0.8)).collect();
    assert!(changes.len() <= 4, "{} changes in 8000 frames", changes.len());
}
