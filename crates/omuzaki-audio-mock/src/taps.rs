//! Taps for the mock engines: which signal a requested tap point reads, and how a block of that signal becomes the
//! samples a consumer gets (channel mix, 16 kHz decimation, and the privacy silence of the voice taps).

use omuzaki_audio_api::engine::{AudioError, Planar};
use omuzaki_audio_api::graph::{GraphSpec, MIC_BUS, MicMode};
use omuzaki_audio_api::ids::{BusId, DeviceRole};
use omuzaki_audio_api::ring::TapFlags;
use omuzaki_audio_api::tap::{TapFormat, TapPoint, TapRate, VoiceRouting, VoiceSetup, VoiceUse};

use crate::mixer::convert;

/// The role of the raw voice input when the voice has its own device (design section 4.2). The mock never has one: it
/// resolves voice taps as "one input, our own chain".
const RAW_VOICE_ROLE: &str = "raw_voice";

/// Where a requested tap reads: the point in the graph, and the voice consumer when it was asked for by use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TapPlan {
    /// The graph point the signal is copied from (never `TapPoint::Voice`).
    pub(crate) point: TapPoint,
    /// Set for a `TapPoint::Voice` request: the mic mode silences it.
    pub(crate) voice: Option<VoiceUse>,
}

impl TapPlan {
    /// Resolves `requested` against `graph`. A voice tap uses the design's table with one input and our own chain
    /// (`VoiceSetup::OwnChain`), so `raw` reads the `mic` bus before its chain and `heard` after its fader.
    ///
    /// # Errors
    /// [`AudioError::TapUnavailable`] when the point names a bus the graph does not have.
    pub(crate) fn resolve(graph: &GraphSpec, requested: &TapPoint) -> Result<Self, AudioError> {
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
        Ok(Self { point, voice })
    }

    /// Whether the tap carries the person's voice and so stops when they mute.
    pub(crate) fn honours_mute(&self) -> bool {
        self.voice.is_some() || matches!(self.point, TapPoint::BusPostFader(_))
    }
}

/// What the mic mode does to a voice consumer (design section 4.2): `Muted` silences all of them, `CohostOnly` all but
/// `listen`, which stays open and is flagged private.
pub(crate) fn voice_flags(mode: MicMode, consumer: VoiceUse) -> TapFlags {
    let listens = consumer.hears_private_talk();
    TapFlags {
        muted: match mode {
            MicMode::Live => false,
            MicMode::Muted => true,
            MicMode::CohostOnly => !listens,
        },
        private: mode == MicMode::CohostOnly && listens,
    }
}

/// Turns blocks of one tap point into the samples a consumer reads. Stateful for the 16 kHz format.
#[derive(Debug)]
pub(crate) struct TapProcessor {
    plan: TapPlan,
    format: TapFormat,
    /// Graph-clock index of the next 48 kHz sample the processor sees.
    index: u64,
    /// The running sum of the current group of three samples (16 kHz), and whether the group began at a multiple of 3.
    group: f32,
    group_started: bool,
}

impl TapProcessor {
    /// A processor for a tap opened when the graph clock was at `start`.
    pub(crate) fn new(plan: TapPlan, format: TapFormat, start: u64) -> Self {
        Self { plan, format, index: start, group: 0.0, group_started: false }
    }

    /// The plan this processor follows.
    pub(crate) fn plan(&self) -> &TapPlan {
        &self.plan
    }

    /// The flags for the samples the next block produces.
    pub(crate) fn flags(&self, mode: MicMode) -> TapFlags {
        self.plan.voice.map_or_else(TapFlags::default, |consumer| voice_flags(mode, consumer))
    }

    /// Appends what `raw` (one block of the plan's point at 48 kHz) gives the consumer to `out`.
    ///
    /// - A silenced voice tap gets zeros, so positions stay contiguous.
    /// - `mono` mixes a stereo point to one channel (`(L + R) / sqrt(2)`); without it a stereo point is interleaved,
    ///   left first.
    /// - `Speech16k` is always mono. **The mock's decimator is the mean of each aligned group of three samples**
    ///   (output `k` is samples `3k` to `3k + 2`): deterministic and obviously 3:1, but not the polyphase filter of
    ///   design section 4.1, whose response mask is the real engine's to meet.
    pub(crate) fn process(&mut self, raw: &Planar, mode: MicMode, out: &mut Vec<f32>) {
        let frames = raw[0].len();
        let silent = self.plan.voice.is_some_and(|consumer| voice_flags(mode, consumer).muted);
        let full_rate = self.format.rate == TapRate::Graph48k;
        for n in 0..frames {
            let frame = if silent { [0.0; 2] } else { [raw[0][n], raw.get(1).map_or(0.0, |c| c[n])] };
            let channels = raw.len();
            if full_rate && !self.format.mono {
                out.extend_from_slice(&frame[..channels]);
            } else {
                let mixed = convert(frame, channels, 1)[0];
                if full_rate {
                    out.push(mixed);
                } else {
                    self.decimate(mixed, out);
                }
            }
            self.index += 1;
        }
    }

    fn decimate(&mut self, sample: f32, out: &mut Vec<f32>) {
        if self.index.is_multiple_of(3) {
            self.group = 0.0;
            self.group_started = true;
        }
        if !self.group_started {
            return;
        }
        self.group += sample;
        if self.index % 3 == 2 {
            out.push(self.group / 3.0);
        }
    }
}

/// The rate-converted index of the tap's first sample: the first aligned group at or after `clock`, for 16 kHz.
pub(crate) fn first_sample(rate: TapRate, clock: u64) -> u64 {
    match rate {
        TapRate::Graph48k => clock,
        TapRate::Speech16k => clock.div_ceil(3),
    }
}
