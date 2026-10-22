//! [`OfflineMix`]: the engine's own mixer core in offline mode, the second implementation of [`OfflineRender`] (design
//! section 9.1). No devices, no threads, no clock: the input is cut into the plan's blocks and each runs through the
//! same [`Mix`] the live executor runs, so what this renders is what the engine plays.
//!
//! The property test (`tests/offline.rs`) holds it to the reference engine in `omuzaki-audio-mock` to 1e-6 on random
//! graphs and block plans. Taps at 16 kHz use the engine's polyphase decimator (design section 4.1), which the
//! reference does not model, so only the 48 kHz taps are compared.

use std::collections::BTreeMap;

use omuzaki_audio_api::engine::{AudioError, BlockPlan, OfflineInput, OfflineOutput, OfflineRender, Planar};
use omuzaki_audio_api::graph::{GraphSpec, MIC_BUS, MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole};
use omuzaki_audio_api::node::MAX_BLOCK;
use omuzaki_audio_api::semantics::MONO_TO_STEREO_GAIN;
use omuzaki_audio_api::tap::{TapFormat, TapPoint, TapRate, VoiceRouting, VoiceSetup, VoiceUse};

use crate::dsp::Decimator;
use crate::executor::voice_flags;
use crate::mix::{Inputs, Mix, Points, Stage};

/// The role of the raw voice input when the voice has its own device: offline there is one input and our own chain.
const RAW_VOICE_ROLE: &str = "raw_voice";

/// The engine's mixer core rendering offline.
#[derive(Copy, Clone, Debug, Default)]
pub struct OfflineMix;

/// Each source's samples for the current block, padded with silence past its end.
struct Staged {
    sources: BTreeMap<SourceRef, (usize, [Vec<f32>; 2])>,
}

impl Staged {
    fn new(input: &OfflineInput) -> Self {
        let sources = input
            .sources
            .iter()
            .filter(|(_, audio)| !audio.is_empty())
            .map(|(source, audio)| (source.clone(), (audio.len().min(2), [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]])))
            .collect();
        Self { sources }
    }

    /// Copies frames `start..start + n` of each source in; samples past a source's end are silence.
    fn stage(&mut self, input: &OfflineInput, start: usize, n: usize) {
        for (source, (channels, block)) in &mut self.sources {
            let Some(audio) = input.sources.get(source) else { continue };
            for (c, channel) in block.iter_mut().enumerate().take(*channels) {
                for (i, slot) in channel[..n].iter_mut().enumerate() {
                    *slot = audio[c].get(start + i).copied().unwrap_or(0.0);
                }
            }
        }
    }
}

impl Inputs for Staged {
    fn source(&self, source: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
        let (channels, block) = self.sources.get(source)?;
        Some((*channels, [&block[0][..n], &block[1][..n]]))
    }
}

/// One requested tap: where it reads, its format, and what it has made so far.
struct OfflineTap {
    requested: TapPoint,
    point: TapPoint,
    voice: Option<VoiceUse>,
    format: TapFormat,
    decimator: Decimator,
    samples: Vec<f32>,
}

impl OfflineTap {
    /// Appends one block of the tap's point, as the live tap would deliver it.
    fn take(&mut self, channels: usize, block: [&[f32]; 2], mode: MicMode) {
        let silent = self.voice.is_some_and(|consumer| voice_flags(mode, consumer).muted);
        let stereo = channels == 2;
        for (i, &left) in block[0].iter().enumerate() {
            let (l, r) = if silent { (0.0, 0.0) } else { (left, if stereo { block[1][i] } else { 0.0 }) };
            if self.format.rate == TapRate::Graph48k && !self.format.mono && stereo {
                self.samples.push(l);
                self.samples.push(r);
                continue;
            }
            let mixed = if stereo { (l + r) * MONO_TO_STEREO_GAIN } else { l };
            match self.format.rate {
                TapRate::Graph48k => self.samples.push(mixed),
                TapRate::Speech16k => {
                    if let Some(s) = self.decimator.push(mixed) {
                        self.samples.push(s);
                    }
                }
            }
        }
    }
}

struct Taps {
    taps: Vec<OfflineTap>,
    mode: MicMode,
}

impl Points for Taps {
    fn reached(&mut self, stage: Stage, bus: &BusId, channels: usize, block: [&[f32]; 2]) {
        for tap in &mut self.taps {
            let here = match (&tap.point, stage) {
                (TapPoint::BusPreChain(id), Stage::PreChain)
                | (TapPoint::BusPostChain(id), Stage::PostChain)
                | (TapPoint::BusPostFader(id), Stage::PostFader) => id == bus,
                _ => false,
            };
            if here {
                tap.take(channels, block, self.mode);
            }
        }
    }
}

/// Resolves a requested tap against `graph`, a voice tap as one input with our own chain.
fn resolve(graph: &GraphSpec, requested: &TapPoint) -> Result<(TapPoint, Option<VoiceUse>), AudioError> {
    let (point, voice) = match requested {
        TapPoint::Voice(consumer) => {
            let internal = |e: omuzaki_core::id::SlugError| AudioError::Internal(e.to_string());
            let mic = BusId::new(MIC_BUS).map_err(internal)?;
            let raw_voice = DeviceRole::new(RAW_VOICE_ROLE).map_err(internal)?;
            let routing = VoiceRouting { mic: &mic, raw_voice: &raw_voice };
            (graph.voice_taps.resolve(*consumer, VoiceSetup::OwnChain, routing).point, Some(*consumer))
        }
        other => (other.clone(), None),
    };
    if let TapPoint::BusPreChain(bus) | TapPoint::BusPostChain(bus) | TapPoint::BusPostFader(bus) = &point
        && graph.bus(bus).is_none()
    {
        return Err(AudioError::TapUnavailable(format!("the graph has no bus `{bus}`")));
    }
    Ok((point, voice))
}

fn append(total: &mut Planar, channels: usize, block: &[Vec<f32>; 2], n: usize) {
    for (c, channel) in total.iter_mut().enumerate().take(channels) {
        channel.extend_from_slice(&block[c][..n]);
    }
}

impl OfflineRender for OfflineMix {
    fn render(&self, graph: &GraphSpec, input: &OfflineInput, plan: BlockPlan) -> Result<OfflineOutput, AudioError> {
        let (mut mix, _) = Mix::compile(graph, 0, input.mic_mode)?;
        let mut taps = Taps { taps: Vec::with_capacity(input.taps.len()), mode: input.mic_mode };
        for (requested, format) in &input.taps {
            let (point, voice) = resolve(graph, requested)?;
            taps.taps.push(OfflineTap {
                requested: requested.clone(),
                point,
                voice,
                format: *format,
                decimator: Decimator::default(),
                samples: Vec::new(),
            });
        }
        let mut output = OfflineOutput::default();
        for bus in &mix.buses {
            output.buses.insert(bus.id.clone(), vec![Vec::with_capacity(input.frames); bus.channels]);
        }
        for out in &mix.outputs {
            output.outputs.insert(out.role.clone(), vec![Vec::with_capacity(input.frames); out.channels]);
        }
        let mut staged = Staged::new(input);
        let mut start = 0usize;
        for n in plan.sizes(input.frames) {
            staged.stage(input, start, n);
            for tap in &mut taps.taps {
                if let TapPoint::Input(role) = &tap.point {
                    let source = SourceRef::Device { role: role.clone() };
                    let zeros = [0.0f32; MAX_BLOCK];
                    let (channels, block) = staged.source(&source, n).unwrap_or((1, [&zeros[..n], &zeros[..n]]));
                    tap.take(channels, block, taps.mode);
                }
            }
            mix.process(n, start as u64, &staged, &mut taps);
            for bus in &mix.buses {
                if let Some(total) = output.buses.get_mut(&bus.id) {
                    append(total, bus.channels, &bus.post, n);
                }
            }
            for out in &mix.outputs {
                if let Some(total) = output.outputs.get_mut(&out.role) {
                    append(total, out.channels, &out.buf, n);
                }
            }
            start += n;
        }
        output.taps = taps.taps.into_iter().map(|t| (t.requested, t.samples)).collect();
        Ok(output)
    }
}
