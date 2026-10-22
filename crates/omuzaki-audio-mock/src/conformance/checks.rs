//! The checks themselves. Each returns an [`Outcome`]; the caller records it and turns panics into failures.

use std::hint::black_box;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::ids::NodeKindId;
use omuzaki_audio_api::node::{
    BlockMut, ChannelSupport, MAX_BLOCK, Node, NodeError, NodeKind, NodeKindInfo, ParamIndex, SAMPLE_RATE,
};
use omuzaki_core::rng::Rng;

use super::alloc::allocations_during;
use super::run::{Plan, make, on_channels, process, settle_samples};
use super::{AllocationCheck, Config, Outcome, Setting, guarded};
use crate::signals::{click_train, impulse, samples_in, sine, white_noise};

/// What a per-bus check needs.
pub(crate) struct Context<'a> {
    pub(crate) kind: &'a dyn NodeKind,
    pub(crate) info: &'a NodeKindInfo,
    pub(crate) config: &'a Config,
    pub(crate) channels: Channels,
}

impl Context<'_> {
    fn node(&self, setting: Setting) -> Result<Box<dyn Node>, String> {
        make(self.kind, &self.config.node_config, self.channels, setting)
    }
}

fn fail(why: impl Into<String>) -> Outcome {
    Outcome::Fail(why.into())
}

/// Runs a check body that reports failures as `Err`.
fn outcome(body: impl FnOnce() -> Result<Outcome, String>) -> Outcome {
    body().unwrap_or_else(Outcome::Fail)
}

/// The kind's description is consistent and its factory agrees with it.
pub(crate) fn declaration(kind: &dyn NodeKind, info: &NodeKindInfo, config: &Config) -> Outcome {
    outcome(|| {
        NodeKindId::new(info.id).map_err(|e| format!("the id `{}` is not a valid kind id: {e}", info.id))?;
        if info.summary.trim().is_empty() {
            return Err("the summary is empty".into());
        }
        if info.latency.min > info.latency.max {
            return Err(format!("the latency range is backwards: {} to {}", info.latency.min, info.latency.max));
        }
        for (at, p) in info.params.iter().enumerate() {
            if !(p.min.is_finite() && p.max.is_finite() && p.default.is_finite() && p.smooth_ms.is_finite()) {
                return Err(format!("parameter `{}` has a value that is not finite", p.name));
            }
            if !(p.min <= p.default && p.default <= p.max) {
                return Err(format!("parameter `{}` has a default outside {} to {}", p.name, p.min, p.max));
            }
            if p.smooth_ms < 0.0 {
                return Err(format!("parameter `{}` has a negative smoothing time", p.name));
            }
            if info.params[..at].iter().any(|q| q.name == p.name) {
                return Err(format!("two parameters are called `{}`", p.name));
            }
        }
        let buses: &[Channels] = match info.channels {
            ChannelSupport::Mono => &[Channels::Mono],
            ChannelSupport::Any => &[Channels::Mono, Channels::Stereo],
        };
        for &channels in buses {
            let node = kind
                .create(&config.node_config, channels)
                .map_err(|e| format!("a node cannot be created on a {channels:?} bus it claims to support: {e}"))?;
            let latency = node.latency();
            if !(info.latency.min..=info.latency.max).contains(&latency) {
                return Err(format!(
                    "the node declares {latency} samples of latency outside the kind's range {} to {}",
                    info.latency.min, info.latency.max
                ));
            }
        }
        if info.channels == ChannelSupport::Mono
            && !matches!(kind.create(&config.node_config, Channels::Stereo), Err(NodeError::UnsupportedChannels(_)))
        {
            return Err("a mono-only kind must refuse a stereo bus with NodeError::UnsupportedChannels".into());
        }
        Ok(Outcome::Pass)
    })
}

/// The first index at or after `from` where `signal` reaches `fraction` of the peak of `signal[from..to]`.
fn onset(signal: &[f32], from: usize, to: usize, fraction: f32) -> Option<usize> {
    let window = &signal[from..to.min(signal.len())];
    let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 1e-9 {
        return None;
    }
    window.iter().position(|s| s.abs() >= fraction * peak).map(|i| from + i)
}

/// An impulse or a click train comes out as late as the node says.
pub(crate) fn latency(cx: &Context<'_>, setting: Setting) -> Outcome {
    outcome(|| {
        let mut node = cx.node(setting)?;
        let declared = node.latency();
        let tolerance = cx.config.latency_tolerance;
        let settle = settle_samples(cx.info);
        let (input, spots, window) = if cx.config.linear {
            let window = declared as usize + 2048;
            (impulse(settle + window, settle), vec![settle], window)
        } else {
            let period = declared as usize + 2048;
            let first = settle;
            let total = first + 6 * period;
            (click_train(total, first, period, 0.8), (0..6).map(|k| first + k * period).collect(), period)
        };
        let total = input.len();
        let output = process(&mut *node, &on_channels(&input, cx.channels), &Plan::Fixed(256).lengths(total), 0);
        if node.latency() != declared {
            return Err(format!("the latency changed from {declared} to {} while the node ran", node.latency()));
        }
        for (channel, out) in output.iter().enumerate() {
            for &at in &spots {
                let end = (at + window).min(total);
                let Some(found) = onset(out, at, end, if cx.config.linear { 0.1 } else { 0.25 }) else {
                    // A node can legitimately be silent at an extreme setting (a mix at zero); the default must not be.
                    return Ok(if setting == Setting::Default {
                        Outcome::Fail(format!("channel {channel} gave no output for the test signal at sample {at}"))
                    } else {
                        Outcome::Skipped(format!(
                            "channel {channel} is silent at {setting} settings, so there is nothing to measure"
                        ))
                    });
                };
                let measured = found - at;
                if measured.abs_diff(declared as usize) > tolerance as usize {
                    let what = if cx.config.linear { "the impulse" } else { "a click" };
                    return Err(format!(
                        "declares {declared} samples of latency but {what} at sample {at} came out {measured} samples late on channel {channel} (tolerance {tolerance})"
                    ));
                }
            }
        }
        Ok(Outcome::Pass)
    })
}

/// The index and values of the first difference between two outputs, if any.
fn first_difference(a: &[Vec<f32>], b: &[Vec<f32>]) -> Option<(usize, usize, f32, f32)> {
    for (channel, (x, y)) in a.iter().zip(b).enumerate() {
        if let Some(at) = x.iter().zip(y).position(|(p, q)| p.to_bits() != q.to_bits()) {
            return Some((channel, at, x[at], y[at]));
        }
    }
    None
}

/// Every cut of the input into blocks gives the same output, bit for bit.
pub(crate) fn block_sizes(cx: &Context<'_>, setting: Setting) -> Outcome {
    outcome(|| {
        // The declared latency is added so the delayed output is observed (and flushed) rather than only the silence in
        // front of it; the total stays a multiple of no block size, so every plan ends on a short block.
        let mut total = 4429 + cx.node(setting)?.latency() as usize;
        while [2, 7, 64, 256, MAX_BLOCK].iter().any(|b| total.is_multiple_of(*b)) {
            total += 1;
        }
        let input: Vec<Vec<f32>> =
            (0..cx.channels.count()).map(|c| white_noise(total, cx.config.seed.wrapping_add(c as u64), 0.5)).collect();
        let run = |plan: Plan| -> Result<Vec<Vec<f32>>, String> {
            let mut node = cx.node(setting)?;
            Ok(process(&mut *node, &input, &plan.lengths(total), 0))
        };
        let reference_plan = Plan::Fixed(MAX_BLOCK);
        let reference = run(reference_plan)?;
        for plan in [Plan::Fixed(1), Plan::Fixed(7), Plan::Fixed(64), Plan::Fixed(256), Plan::Random(cx.config.seed)] {
            if let Some((channel, at, a, b)) = first_difference(&reference, &run(plan)?) {
                return Err(format!(
                    "{plan} differ from {reference_plan} at frame {at} of channel {channel}: {a:e} against {b:e}"
                ));
            }
        }
        Ok(Outcome::Pass)
    })
}

/// `process`, `set_param`, `reset` and `report` neither allocate nor free.
pub(crate) fn allocation(cx: &Context<'_>, setting: Setting) -> Outcome {
    if cx.config.allocations == AllocationCheck::Skip {
        return Outcome::Skipped("the allocation check was turned off".into());
    }
    outcome(|| {
        // A zero count means nothing if the counting allocator is not the global one: look at a known allocation first.
        if allocations_during(&mut || drop(black_box(Box::new(0u64)))) == 0 {
            return Err("the counting allocator is not installed: put `#[global_allocator] static ALLOC: \
                        omuzaki_audio_mock::conformance::CountingAlloc = omuzaki_audio_mock::conformance::CountingAlloc;` \
                        in the test binary"
                .into());
        }
        // One instance per probe, each at `setting` and untouched by the others: `set_param` leaves every parameter at its
        // extreme, which would hide an allocation that only happens at the default or the minimum.
        let mut for_set_param = cx.node(setting)?;
        let mut for_process = cx.node(setting)?;
        let mut for_reset = cx.node(setting)?;
        let sizes = [1usize, 7, 64, 256, MAX_BLOCK, 100];
        // Everything the probed code touches is made before it starts.
        let mut left: Vec<Vec<f32>> = sizes.iter().map(|&n| white_noise(n, cx.config.seed, 0.5)).collect();
        let mut right = left.clone();
        let mut warm_left = white_noise(MAX_BLOCK, cx.config.seed, 0.5);
        let mut warm_right = warm_left.clone();
        let params: Vec<(ParamIndex, [f32; 2])> = cx
            .info
            .params
            .iter()
            .enumerate()
            .map(|(at, p)| (ParamIndex(u16::try_from(at).unwrap_or(u16::MAX)), [p.min, p.max]))
            .collect();
        let stereo = cx.channels == Channels::Stereo;

        let set_param = allocations_during(&mut || {
            for round in 0..8 {
                for (index, values) in &params {
                    for_set_param.set_param(*index, values[round % 2]);
                }
            }
        });
        let process_calls = allocations_during(&mut || {
            for round in 0..16 {
                let which = round % sizes.len();
                let n = sizes[which];
                let first = (round * MAX_BLOCK) as u64;
                let (l, r) = (&mut left[which][..n], &mut right[which][..n]);
                if stereo {
                    for_process.process(&mut BlockMut::stereo(l, r, first));
                } else {
                    for_process.process(&mut BlockMut::mono(l, first));
                }
                black_box(for_process.report());
            }
        });
        // `reset` is probed after the node has run, as it is in use, but the run is not part of the measurement.
        if stereo {
            for_reset.process(&mut BlockMut::stereo(&mut warm_left, &mut warm_right, 0));
        } else {
            for_reset.process(&mut BlockMut::mono(&mut warm_left, 0));
        }
        let reset = allocations_during(&mut || {
            for_reset.reset();
            black_box(for_reset.report());
            black_box(for_reset.latency());
        });
        for (name, count) in [("set_param", set_param), ("process or report", process_calls), ("reset", reset)] {
            if count > 0 {
                return Err(format!(
                    "`{name}` called the allocator {count} time(s) (allocating or freeing); the audio thread must never do either"
                ));
            }
        }
        Ok(Outcome::Pass)
    })
}

/// A minute of full-scale white noise stays finite, and silence after it decays below -120 dBFS.
pub(crate) fn finite_and_decay(cx: &Context<'_>, setting: Setting) -> (Outcome, Outcome) {
    let mut node = match cx.node(setting) {
        Ok(node) => node,
        Err(why) => return (Outcome::Fail(why.clone()), Outcome::Fail(why)),
    };
    let flush = node.latency() as usize;
    let mut rng = Rng::new(cx.config.seed ^ 0xF1_1173);
    let channels = cx.channels.count();
    let mut first_sample = 0u64;
    // Runs `frames` frames of `source` through the node and reports the first non-finite value and the peak.
    let mut run = |frames: usize, loud: bool| -> Result<(f32, [f32; 2]), String> {
        let mut peak = 0.0f32;
        // The peak of each channel over the last second.
        let mut tail = [0.0f32; 2];
        let mut left = vec![0.0f32; 256];
        let mut right = vec![0.0f32; 256];
        let mut done = 0;
        while done < frames {
            let n = 256.min(frames - done);
            for i in 0..n {
                left[i] = if loud { rng.range_f32(-1.0, 1.0) } else { 0.0 };
                right[i] = if loud { rng.range_f32(-1.0, 1.0) } else { 0.0 };
            }
            let block_start = first_sample;
            let (l, r) = (&mut left[..n], &mut right[..n]);
            let result = guarded_block(|| {
                if channels == 2 {
                    node.process(&mut BlockMut::stereo(l, r, block_start));
                } else {
                    node.process(&mut BlockMut::mono(l, block_start));
                }
            });
            result?;
            for (channel, data) in [&left[..n], &right[..n]].into_iter().take(channels).enumerate() {
                if let Some(i) = data.iter().position(|s| !s.is_finite()) {
                    return Err(format!(
                        "channel {channel} gave {} at frame {} of the {}",
                        data[i],
                        done + i,
                        if loud { "noise" } else { "silence" }
                    ));
                }
                let block_peak = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                peak = peak.max(block_peak);
                if !loud && frames - done <= SAMPLE_RATE as usize {
                    tail[channel] = tail[channel].max(block_peak);
                }
            }
            first_sample += n as u64;
            done += n;
        }
        Ok((peak, tail))
    };
    let noise_frames = samples_in(cx.config.noise_seconds);
    let finite = match run(noise_frames, true) {
        Ok(_) => Outcome::Pass,
        Err(why) => {
            return (Outcome::Fail(why), Outcome::Skipped("not run: the output was not finite".into()));
        }
    };
    // The silence also covers the declared latency, so the noise still queued inside the node comes out before the last
    // second that is measured.
    let decay = match run(samples_in(cx.config.decay_seconds) + flush, false) {
        Ok((_, tail)) => {
            // The loudest channel decides. -120 dBFS is 1e-6.
            let (channel, last) = tail
                .iter()
                .take(channels)
                .copied()
                .enumerate()
                .fold((0, 0.0f32), |a, (c, p)| if p > a.1 { (c, p) } else { a });
            if last < 1e-6 {
                Outcome::Pass
            } else {
                fail(format!(
                    "channel {channel} still has a peak of {last:e} ({:.1} dBFS) in the last second of {} s of silence; it must fall below -120 dBFS",
                    20.0 * last.log10(),
                    cx.config.decay_seconds
                ))
            }
        }
        Err(why) => fail(why),
    };
    (finite, decay)
}

/// Runs `body`, turning a panic in the node into an error string.
fn guarded_block(body: impl FnOnce()) -> Result<(), String> {
    match guarded(|| {
        body();
        Outcome::Pass
    }) {
        Outcome::Fail(why) => Err(why),
        _ => Ok(()),
    }
}

/// After `reset` the output does not depend on what the node processed before.
///
/// Two nodes settle on silence for the same time; the tested one then also processes a loud dirty signal and runs a
/// further odd number of samples of silence, so a free-running phase is somewhere else in it than in the reference.
/// Both are reset and run the test signal from the same graph index; the outputs must match bit for bit. The reference
/// is reset too, because a node that ran at all has advanced an oscillator's phase, and a correct `reset` puts it back:
/// comparing against a node that never reset would fail a conforming modulator, while two nodes with different run
/// lengths still expose a `reset` that leaves the phase alone. The settle time is long (about 30 smoothing constants) so
/// a parameter ramp has finished in both and the different run lengths cannot show in it.
pub(crate) fn reset(cx: &Context<'_>, setting: Setting) -> Outcome {
    outcome(|| {
        let channels = cx.channels;
        // The declared latency is added to the dirty signal (so it reaches the end of the node's delay line) and to the
        // test signal (so the delayed output is observed, not only the silence in front of it).
        let latency = cx.node(setting)?.latency() as usize;
        let (settle, dirty_frames, test_frames) = (6 * settle_samples(cx.info), 3000 + latency, 5000 + latency);
        // Odd, and not a multiple of any oscillator period a test would use.
        let extra = 1001;
        let dirty: Vec<Vec<f32>> =
            (0..channels.count()).map(|c| white_noise(dirty_frames, cx.config.seed ^ 0xD1 ^ c as u64, 0.8)).collect();
        let test: Vec<Vec<f32>> =
            (0..channels.count()).map(|c| white_noise(test_frames, cx.config.seed ^ 0xC1 ^ c as u64, 0.5)).collect();
        let silence = |frames: usize| on_channels(&vec![0.0; frames], channels);
        // Both nodes see the test signal from the same graph index, so a node that reads the clock treats them alike.
        let at = 1 << 20;
        let run_after_reset = |dirty: Option<&[Vec<f32>]>| -> Result<Vec<Vec<f32>>, String> {
            let mut node = cx.node(setting)?;
            process(&mut *node, &silence(settle), &Plan::Fixed(256).lengths(settle), 0);
            match dirty {
                Some(dirty) => {
                    // The odd stretch comes first, so the dirty signal is still inside the node when it is reset.
                    process(&mut *node, &silence(extra), &Plan::Fixed(256).lengths(extra), settle as u64);
                    process(&mut *node, dirty, &Plan::Fixed(256).lengths(dirty_frames), (settle + extra) as u64);
                }
                None => {
                    process(&mut *node, &silence(dirty_frames), &Plan::Fixed(256).lengths(dirty_frames), settle as u64);
                }
            }
            node.reset();
            Ok(process(&mut *node, &test, &Plan::Fixed(256).lengths(test_frames), at))
        };
        let after_dirty = run_after_reset(Some(&dirty))?;
        let after_quiet = run_after_reset(None)?;
        if let Some((channel, frame, a, b)) = first_difference(&after_dirty, &after_quiet) {
            return Err(format!(
                "after reset the output differs from that of a node that ran differently before its reset, at frame {frame} of channel {channel}: {a:e} against {b:e}"
            ));
        }
        Ok(Outcome::Pass)
    })
}

/// The largest second difference of samples inside `signal[from..to]`: each one uses its two neighbours, so the first
/// and last sample of the window are only neighbours.
fn max_second_difference(signal: &[f32], from: usize, to: usize) -> f32 {
    let to = to.min(signal.len());
    (from + 1..to.saturating_sub(1))
        .map(|n| (signal[n + 1] - 2.0 * signal[n] + signal[n - 1]).abs())
        .fold(0.0, f32::max)
}

/// Sweeping parameter `index` between its extremes mid-signal makes no click.
pub(crate) fn sweep(cx: &Context<'_>, index: usize) -> Outcome {
    outcome(|| {
        let spec = cx.info.params[index];
        let param = ParamIndex(u16::try_from(index).map_err(|_| "more than 65535 parameters".to_string())?);
        let mut node = cx.node(Setting::Default)?;
        node.set_param(param, spec.min);
        // A parameter change may reach the output anywhere from the change itself to one latency later, so each segment
        // and the settle time are a latency longer: the move is fully inside its window and so is the steady state.
        let latency = node.latency() as usize;
        let (segment, steady, block) = (1600usize + latency, 400usize, 97usize);
        let settle = settle_samples(cx.info) + latency;
        let tone = sine(settle + 8 * segment, 220.0, 0.25);
        let channels = cx.channels.count();
        // Settle at the minimum first, then move to the maximum and back, eight times.
        let mut output = vec![Vec::new(); channels];
        let mut at = 0;
        let mut changes = Vec::new();
        let mut run = |node: &mut Box<dyn Node>, frames: usize, at: &mut usize| {
            let part = on_channels(&tone[*at..*at + frames], cx.channels);
            let out = process(&mut **node, &part, &Plan::Fixed(block).lengths(frames), *at as u64);
            for (all, new) in output.iter_mut().zip(out) {
                all.extend(new);
            }
            *at += frames;
        };
        run(&mut node, settle, &mut at);
        for step in 0..8 {
            let value = if step % 2 == 0 { spec.max } else { spec.min };
            changes.push(at);
            node.set_param(param, value);
            run(&mut node, segment, &mut at);
        }
        if let Some((channel, frame)) =
            output.iter().enumerate().find_map(|(c, o)| o.iter().position(|s| !s.is_finite()).map(|i| (c, i)))
        {
            return Err(format!("the output is not finite at frame {frame} of channel {channel} during the sweep"));
        }
        if spec.smooth_ms <= 0.0 {
            // The kind declares the parameter unsmoothed (it applies at once, at a block start), so a step is allowed.
            return Ok(Outcome::Pass);
        }
        #[allow(clippy::cast_possible_truncation)] // a few hundred samples
        let smoothing = (f64::from(spec.smooth_ms) * f64::from(SAMPLE_RATE) / 1000.0).max(1.0) as f32;
        for (channel, out) in output.iter().enumerate() {
            let level = |from: usize, to: usize| out[from..to].iter().fold(0.0f32, |m, s| m.max(s.abs()));
            // What the signal's own curvature produces when the parameter does not move, at either end of every move.
            let natural =
                changes.iter().map(|&c| max_second_difference(out, c.saturating_sub(steady), c)).fold(0.0, f32::max);
            for (k, &change) in changes.iter().enumerate() {
                let end = change + segment;
                let jump = (level(change.saturating_sub(steady), change) + level(end - steady, end)) / smoothing;
                // A move over `smoothing` samples can change the output by its full size in that time; allow four times
                // that, plus three times the curvature the steady signal already has.
                let allowed = 3.0 * natural + 4.0 * jump + 1e-6;
                let found = max_second_difference(out, change.saturating_sub(2), end - steady);
                if found > allowed {
                    let value = if k % 2 == 0 { spec.max } else { spec.min };
                    return Err(format!(
                        "moving it to {value} at sample {change} made a click on channel {channel}: second difference {found:e}, \
                         but {allowed:e} is the most its {} ms of smoothing allows",
                        spec.smooth_ms
                    ));
                }
            }
        }
        Ok(Outcome::Pass)
    })
}
