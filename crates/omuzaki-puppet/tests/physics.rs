//! Pendulums against their closed forms (#1363): the small-swing period, critical damping, the spring's sag, and the
//! fixed step's independence from the frame rate.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test arithmetic on small numbers"
)]

use omuzaki_core::glam::Vec2;
use omuzaki_puppet::physics::STEP;
use omuzaki_puppet::{Evaluator, Physics, PuppetBuilder, grid_mesh};
use omuzaki_puppet_api::{BindTarget, Interp, MapMode, NodeIx, ParamIx, Pendulum, PendulumModel};

const LENGTH: f32 = 100.0;

/// A puppet whose `move` parameter slides the anchor sideways, and a pendulum writing `swing`.
fn rig(model: PendulumModel, damping: f32) -> (Evaluator, ParamIx, ParamIx) {
    let mut b = PuppetBuilder::new("p", [400.0, 400.0]);
    let tex = b.texture("t.png", [2, 2]);
    let root = b.root();
    let anchor = b.group("anchor", root, [0.0, 0.0]);
    b.part("bob", anchor, tex, grid_mesh([0.0, 0.0, 1.0, 1.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let mv = b.param("move", -100.0, 100.0, 0.0, &[-100.0, 100.0]);
    b.bind(mv, anchor, BindTarget::TranslateX, Interp::Linear, vec![-100.0, 100.0]);
    let swing = b.param2("swing", [-1.0, 0.0], [1.0, 2.0], [0.0, 1.0], [&[-1.0, 1.0], &[0.0, 2.0]]);
    b.pendulum(Pendulum {
        name: "hair".into(),
        anchor: NodeIx(1),
        param: swing,
        model,
        map: MapMode::AngleLength,
        gravity: 1.0,
        length: LENGTH,
        frequency: 2.0,
        angle_damping: damping,
        length_damping: 1.0,
        output_scale: [1.0, 1.0],
        local_only: false,
    });
    let e = Evaluator::new(b.finish().unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    (e, mv, swing)
}

/// Runs `seconds` at `fps`, recording the swing angle after each frame.
fn run(e: &mut Evaluator, physics: &mut Physics, swing: ParamIx, seconds: f32, fps: f32) -> Vec<f32> {
    let frames = (seconds * fps).round() as usize;
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        physics.step(e, 1.0 / fps);
        e.evaluate();
        out.push(e.value(swing)[0]);
    }
    out
}

#[test]
fn a_small_swing_has_the_period_of_the_formula() {
    let (mut e, mv, swing) = rig(PendulumModel::Rigid, 0.0);
    let mut physics = Physics::new(1);
    run(&mut e, &mut physics, swing, 0.1, 240.0);
    // A sudden small step sideways starts a swing.
    e.set(mv, [5.0, 0.0]);
    e.evaluate();
    let angles = run(&mut e, &mut physics, swing, 4.0, 240.0);
    // Zero crossings from + to - are one period apart.
    let crossings: Vec<usize> =
        angles.windows(2).enumerate().filter(|(_, w)| w[0] > 0.0 && w[1] <= 0.0).map(|(i, _)| i).collect();
    assert!(crossings.len() >= 3, "it swings: {crossings:?}");
    let g = 9.8 * 1000.0;
    let want = 2.0 * std::f32::consts::PI * (LENGTH / g).sqrt();
    let got = (crossings[2] - crossings[1]) as f32 * STEP;
    assert!((got - want).abs() / want < 0.02, "period {got} s against {want} s");
}

#[test]
fn critical_damping_settles_without_overshoot() {
    let (mut e, mv, swing) = rig(PendulumModel::Rigid, 1.0);
    let mut physics = Physics::new(1);
    run(&mut e, &mut physics, swing, 0.1, 60.0);
    e.set(mv, [5.0, 0.0]);
    e.evaluate();
    let angles = run(&mut e, &mut physics, swing, 3.0, 60.0);
    // The anchor moved right, so the bob trails left: the angle has one sign throughout and returns to rest.
    let first = angles.iter().copied().find(|a| a.abs() > 1e-4).unwrap_or(0.0);
    assert!(angles.iter().all(|a| a * first >= -1e-4), "it crossed over: {angles:?}");
    assert!(angles.last().is_some_and(|a| a.abs() < 1e-3), "it settles: {:?}", angles.last());
}

#[test]
fn a_spring_hangs_at_its_length_and_rests_straight_down() {
    let (mut e, _, swing) = rig(PendulumModel::Spring, 1.0);
    let mut physics = Physics::new(1);
    run(&mut e, &mut physics, swing, 3.0, 60.0);
    let [angle, length] = e.value(swing);
    assert!(angle.abs() < 1e-3, "straight down: {angle}");
    // At rest, gravity balances the spring: the hang is the configured length.
    assert!((length - 1.0).abs() < 0.01, "length ratio {length}");
    assert!(physics.bob(0).is_some_and(|b| (b - Vec2::new(0.0, LENGTH)).length() < 1.0));
}

#[test]
fn the_motion_at_step_boundaries_does_not_depend_on_the_frame_rate() {
    let mut finals = Vec::new();
    for fps in [30.0, 60.0, 120.0] {
        let (mut e, mv, swing) = rig(PendulumModel::Rigid, 0.2);
        let mut physics = Physics::new(1);
        run(&mut e, &mut physics, swing, 0.5, fps);
        e.set(mv, [20.0, 0.0]);
        e.evaluate();
        run(&mut e, &mut physics, swing, 1.0, fps);
        finals.push(e.value(swing)[0]);
    }
    assert!(finals[0].abs() > 1e-3, "it swings: {finals:?}");
    assert!(finals.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-5), "{finals:?}");
}
