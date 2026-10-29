//! The puppet budget numbers of `docs/design/puppet2d.md`: a reference puppet of 20 parts and 8,000 vertices (a
//! two-axis head turn deforming every part, two pendulums), timed for evaluation, physics and the CPU draw.
//!
//! ```text
//! cargo run --release -p omuzaki-puppet --example budget
//! ```

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "timings and small counts")]

use omuzaki_core::glam::Affine2;
use omuzaki_puppet::{Canvas, CpuRenderer, Evaluator, Physics, PuppetBuilder, Texture, fit_view, grid_mesh};
use omuzaki_puppet_api::{Interp, MapMode, Pendulum, PendulumModel};

fn p95(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (mean, v[(v.len() * 95) / 100])
}

fn main() {
    let mut b = PuppetBuilder::new("budget", [1024.0, 1536.0]);
    let tex = b.texture("t.png", [256, 256]);
    let root = b.root();
    let turn = b.param2("turn", [-30.0, -30.0], [30.0, 30.0], [0.0, 0.0], [&[-30.0, 0.0, 30.0], &[-30.0, 0.0, 30.0]]);
    let swing = b.param("swing", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let mut parts = Vec::new();
    for i in 0..20 {
        let x = -400.0 + (i % 5) as f32 * 160.0;
        let y = -600.0 + (i / 5) as f32 * 300.0;
        // 19 x 20 cells: 400 vertices a part.
        let part =
            b.part(&format!("part {i}"), root, tex, grid_mesh([x, y, 150.0, 280.0], 19, 19, [0.0, 0.0, 1.0, 1.0]));
        parts.push(part);
    }
    for (k, part) in parts.iter().enumerate() {
        let n = 400;
        let sets: Vec<Vec<[f32; 2]>> = (0..9)
            .map(|key| {
                (0..n)
                    .map(|v| [((key % 3) as f32 - 1.0) * (v % 7) as f32, ((key / 3) as f32 - 1.0) * (k % 5) as f32])
                    .collect()
            })
            .collect();
        b.bind_deform(turn, *part, Interp::Cubic, sets);
        let sway: Vec<Vec<[f32; 2]>> = (0..3).map(|key| vec![[(key as f32 - 1.0) * 20.0, 0.0]; n]).collect();
        b.bind_deform(swing, *part, Interp::Linear, sway);
    }
    for i in 0..2 {
        b.pendulum(Pendulum {
            name: format!("hair {i}"),
            anchor: root,
            param: swing,
            model: PendulumModel::Rigid,
            map: MapMode::AngleLength,
            gravity: 1.0,
            length: 200.0,
            frequency: 1.0,
            angle_damping: 0.4,
            length_damping: 0.5,
            output_scale: [1.0, 1.0],
            local_only: false,
        });
    }
    let def = b.finish().unwrap_or_else(|e| panic!("{e}"));
    let vertices: usize = def.nodes.iter().filter_map(|n| n.kind.deform_points()).sum();
    let mut eval = Evaluator::new(def).unwrap_or_else(|e| panic!("{e}"));
    let mut physics = Physics::new(2);
    let (mut evals, mut steps, mut draws) = (Vec::new(), Vec::new(), Vec::new());
    let px: Vec<u8> = (0..256u32 * 256).flat_map(|i| [(i % 251) as u8, 120, 200, 255]).collect();
    let textures = [Texture::from_straight(256, 256, &px).unwrap_or_else(|| panic!("texture"))];
    let mut renderer = CpuRenderer::default();
    let mut canvas = Canvas::new(1080, 1920);
    let view: Affine2 = fit_view([1024.0, 1536.0], 1080, 1920);
    for frame in 0..600 {
        let t = frame as f32 / 60.0;
        let started = std::time::Instant::now();
        physics.step(&mut eval, 1.0 / 60.0);
        let stepped = std::time::Instant::now();
        eval.set(turn, [25.0 * (t * 1.3).sin(), 12.0 * (t * 0.7).cos()]);
        eval.evaluate();
        let evaluated = std::time::Instant::now();
        if frame % 10 == 0 {
            canvas.fill([0.0; 4]);
            renderer.draw(eval.draw_list(), &textures, view, &mut canvas);
            draws.push(evaluated.elapsed().as_secs_f64() * 1e3);
        }
        steps.push((stepped - started).as_secs_f64() * 1e3);
        evals.push((evaluated - stepped).as_secs_f64() * 1e3);
    }
    let (em, e95) = p95(evals);
    let (sm, s95) = p95(steps);
    let (dm, d95) = p95(draws);
    println!("20 parts, {vertices} vertices, 2 pendulums");
    println!("evaluate: mean {em:.3} ms, p95 {e95:.3} ms");
    println!("physics.step (1/60 s, 4 sub-steps): mean {sm:.4} ms, p95 {s95:.4} ms");
    println!("CPU draw 1080x1920: mean {dm:.1} ms, p95 {d95:.1} ms");
}
