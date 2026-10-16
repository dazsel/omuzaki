//! [`OfflineEngine`]: the reference implementation of [`OfflineRender`] (design section 9.1).
//!
//! It cuts the input into the blocks the plan says, runs each through the [`Mixer`](crate::mixer::Mixer) and collects
//! the buses, the output roles and the taps. Deterministic: no clock, no threads, no randomness except the plan's seed.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_audio_api::engine::{AudioError, BlockPlan, OfflineInput, OfflineOutput, OfflineRender, Planar};
use omuzaki_audio_api::graph::GraphSpec;
use omuzaki_audio_api::ids::{BusId, DeviceRole};
use omuzaki_audio_api::tap::TapPoint;

use crate::mixer::{Mixer, check_gain};
use crate::taps::{TapPlan, TapProcessor};

/// A fader change at an exact sample of an offline render.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Aims a bus's fader at this many dB.
    BusGainDb {
        /// The bus.
        bus: BusId,
        /// The new gain.
        db: f32,
    },
    /// Mutes or unmutes a bus.
    BusMuted {
        /// The bus.
        bus: BusId,
        /// Whether it is muted from now on.
        muted: bool,
    },
}

/// A [`Change`] and the sample it takes effect at.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedChange {
    /// The graph-clock sample from which the change is in force: sample `at` is the first one its ramp moves.
    pub at: u64,
    /// What changes.
    pub change: Change,
}

/// The reference offline engine. Written to be obviously correct, not fast.
#[derive(Copy, Clone, Debug, Default)]
pub struct OfflineEngine;

impl OfflineEngine {
    /// Like [`OfflineRender::render`], with faders changed at exact samples.
    ///
    /// A block never straddles a change: the plan's blocks are cut at each change's sample, so the ramps (which are per
    /// sample) restart exactly there, whatever the plan. A change at or after `input.frames` has no effect. Changes at
    /// one sample apply in the order given.
    ///
    /// # Errors
    /// As [`OfflineRender::render`], [`AudioError::UnknownBus`] for a change that names a missing bus, and
    /// [`AudioError::Graph`] for a gain that is NaN or above `MAX_GAIN_DB`. Changes are checked before rendering.
    pub fn render_with_changes(
        &self,
        graph: &GraphSpec,
        input: &OfflineInput,
        plan: BlockPlan,
        changes: &[TimedChange],
    ) -> Result<OfflineOutput, AudioError> {
        let mut mixer = Mixer::new(graph, input.mic_mode)?;
        for timed in changes {
            validate(graph, &timed.change)?;
        }
        let mut processors = Vec::with_capacity(input.taps.len());
        for (point, format) in &input.taps {
            processors.push((point, TapProcessor::new(TapPlan::resolve(graph, point)?, *format, 0), Vec::new()));
        }
        let want: BTreeSet<TapPoint> = processors.iter().map(|(_, p, _)| p.plan().point.clone()).collect();
        let mut output = OfflineOutput::default();
        for bus in &mixer.buses {
            output.buses.insert(bus.id.clone(), vec![Vec::new(); bus.channels.count()]);
        }
        // Every role the graph names is answered, even when no frame is rendered.
        for (role, width) in mixer.output_widths() {
            output.outputs.insert(role.clone(), vec![Vec::new(); *width]);
        }
        let mut pending: Vec<&TimedChange> = changes.iter().collect();
        pending.sort_by_key(|c| c.at);
        let mut pending = pending.into_iter().peekable();
        let mut start = 0usize;
        for planned in plan.sizes(input.frames) {
            let end = start + planned;
            while start < end {
                while let Some(change) = pending.next_if(|c| c.at <= start as u64) {
                    apply(&mut mixer, &change.change)?;
                }
                let next_change = pending.peek().map_or(end, |c| usize::try_from(c.at).map_or(end, |at| at.min(end)));
                let frames = next_change.max(start + 1) - start;
                let capture = mixer.process(&input.sources, start, frames, &want);
                collect(&mut output, capture.buses, capture.outputs);
                for (_, processor, samples) in &mut processors {
                    if let Some(raw) = capture.points.get(&processor.plan().point) {
                        processor.process(raw, input.mic_mode, samples);
                    }
                }
                start += frames;
            }
        }
        output.taps = processors.into_iter().map(|(point, _, samples)| (point.clone(), samples)).collect();
        Ok(output)
    }
}

/// Checks a change before anything is rendered: the bus exists and a gain is one the live engine would accept.
fn validate(graph: &GraphSpec, change: &Change) -> Result<(), AudioError> {
    let (Change::BusGainDb { bus, .. } | Change::BusMuted { bus, .. }) = change;
    if graph.bus(bus).is_none() {
        return Err(AudioError::UnknownBus(bus.clone()));
    }
    match change {
        Change::BusGainDb { bus, db } => check_gain(bus, *db),
        Change::BusMuted { .. } => Ok(()),
    }
}

fn apply(mixer: &mut Mixer, change: &Change) -> Result<(), AudioError> {
    match change {
        Change::BusGainDb { bus, db } => mixer.set_bus_gain_db(bus, *db),
        Change::BusMuted { bus, muted } => mixer.set_bus_muted(bus, *muted),
    }
}

/// Appends one block's buses and outputs to the totals.
fn collect(output: &mut OfflineOutput, buses: BTreeMap<BusId, Planar>, outputs: BTreeMap<DeviceRole, Planar>) {
    for (id, block) in buses {
        append(output.buses.entry(id).or_default(), &block);
    }
    for (role, block) in outputs {
        append(output.outputs.entry(role).or_default(), &block);
    }
}

/// Appends the channels of `block` to `total`, creating channels as needed.
fn append(total: &mut Planar, block: &Planar) {
    if total.len() < block.len() {
        total.resize(block.len(), Vec::new());
    }
    for (channel, samples) in total.iter_mut().zip(block) {
        channel.extend_from_slice(samples);
    }
}

impl OfflineRender for OfflineEngine {
    fn render(&self, graph: &GraphSpec, input: &OfflineInput, plan: BlockPlan) -> Result<OfflineOutput, AudioError> {
        self.render_with_changes(graph, input, plan, &[])
    }
}
