//! `fx.denoise` (#2587): the node's own rules. A dry mix is the input exactly delayed, the steady state allocates
//! nothing, `reset` only re-clones, and what one frame costs. How well it cleans a voice, and that the microphone
//! mouth reads the cleaned voice at least as well as the noisy one, is in `omuzaki-test-track/tests/denoise.rs`, beside
//! the voice line and the lip-sync evaluation it uses.
#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod common;

use std::time::Instant;

use omuzaki_audio_api::node::{BlockMut, Node, NodeKind, SAMPLE_RATE};
use omuzaki_audio_fx::denoise::{Denoise, DenoiseKind, FRAME, LATENCY};
use omuzaki_audio_mock::conformance::{CountingAlloc, allocations_during};
use omuzaki_audio_mock::signals::white_noise;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const RATE: u32 = SAMPLE_RATE;

/// `x` through a fresh `fx.denoise` at `params`, with the node's delay taken out (the tail is padded with silence).
fn denoised(x: &[f32], params: &[(&str, f32)]) -> Vec<f32> {
    let mut node = common::node(&DenoiseKind, params);
    let mut padded = x.to_vec();
    padded.extend(std::iter::repeat_n(0.0, LATENCY));
    let out = common::run(&mut *node, &padded);
    out[LATENCY..].to_vec()
}

#[test]
fn amount_zero_is_the_dry_voice_exactly_delayed() {
    let x = white_noise(10 * FRAME, 3, 0.3);
    let out = denoised(&x, &[("amount", 0.0)]);
    for (a, b) in x.iter().zip(&out) {
        assert!((a - b).abs() < 1e-6, "{a} {b}");
    }
}

#[test]
fn process_allocates_nothing_after_the_first_frame_and_reset_only_re_clones() {
    let mut node = DenoiseKind
        .create(&omuzaki_audio_api::node::NodeConfig::default(), omuzaki_audio_api::graph::Channels::Mono)
        .unwrap();
    let x = white_noise(48_000, 5, 0.3);
    let mut buf = x[..FRAME].to_vec();
    node.process(&mut BlockMut::mono(&mut buf, 0));
    let mut chunks: Vec<Vec<f32>> = x[FRAME..].chunks(256).map(<[f32]>::to_vec).collect();
    let mut at = FRAME as u64;
    let steady = allocations_during(&mut || {
        for chunk in &mut chunks {
            let n = chunk.len() as u64;
            node.process(&mut BlockMut::mono(chunk, at));
            at += n;
        }
    });
    assert_eq!(steady, 0, "process allocated after the first frame");
    let reset = allocations_during(&mut || node.reset());
    println!("reset: {reset} allocations");
    assert!(reset <= 40, "{reset}");
}

/// The CPU per 10 ms frame, printed for the pull request (a release build gives the real number).
#[test]
fn cost_per_frame() {
    let mut node = Denoise::new();
    let x = white_noise(10 * RATE as usize, 6, 0.3);
    let mut buf = x.clone();
    let start = Instant::now();
    for (i, chunk) in buf.chunks_mut(256).enumerate() {
        node.process(&mut BlockMut::mono(chunk, (i * 256) as u64));
    }
    let per_frame_us = start.elapsed().as_secs_f64() * 1e6 / (x.len() / FRAME) as f64;
    println!("fx.denoise: {per_frame_us:.0} µs per 10 ms frame ({:.2}% of one core)", per_frame_us / 100.0);
}
