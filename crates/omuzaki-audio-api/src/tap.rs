//! Taps and feeds (design doc section 4): how audio leaves the graph for the things that listen, and enters it from the
//! things that speak.
//!
//! - A **tap** copies one point of the graph into a lock-free ring that a consumer reads on its own thread (lip sync,
//!   captions, the AI performer's listening, recording, the clap test). The ring is [`crate::ring`].
//! - A **feed** is the mirror image: a ring a producer thread (TTS, a music decoder) writes into a bus.
//! - The **voice taps** of PF-12 are named by what the consumer wants ([`VoiceUse`]); the engine resolves them to a tap
//!   point from what is bound ([`VoiceSignal::resolve`]).
//!
//! **Mute is privacy.** When the person mutes `mic`, every voice tap goes silent and is flagged
//! [`TapRead::muted`](crate::ring::TapRead), the `listen` tap included: nobody hears, the AI included. In
//! `MicMode::CohostOnly` only `listen` stays open, and its runs are flagged `private`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{BusId, DeviceRole, FeedId};
use crate::ring::TapFlags;

/// The reading end of a tap's ring.
pub type TapReader = crate::ring::Consumer;
/// The writing end of a feed's ring.
pub type FeedWriter = crate::ring::Producer;

/// What a ring does when its reader falls behind.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DropPolicy {
    /// A contiguous stream (recognition, recording, the clap test): when full, refuse what does not fit and count it.
    DropNewest,
    /// A latest-window reader (lip sync, cues, meters): the writer never waits and the reader skips ahead.
    Overwrite,
}

/// Which consumer a voice tap serves (PF-12, design doc section 4.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceUse {
    /// Lip sync (FE-08, ML-05).
    Mouth,
    /// Loudness, onsets, laughter (PF-08).
    Cues,
    /// Captions: recognition (BR-13, ML-03).
    Captions,
    /// The AI performer's listening (AP-03).
    Listen,
}

/// Which version of the person's voice a consumer wants.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSignal {
    /// The voice before any changer: the raw mic.
    Raw,
    /// The voice as viewers hear it: after the changer and the chain, after the fader.
    Heard,
}

/// Where in the graph a tap reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TapPoint {
    /// A device input, as it comes in (before any bus).
    Input(DeviceRole),
    /// A bus after its sources are summed, before its chain.
    BusPreChain(BusId),
    /// A bus after its chain, before the fader.
    BusPostChain(BusId),
    /// A bus after the fader (and mute): what the bus sends on.
    BusPostFader(BusId),
    /// A voice signal for a consumer, resolved by the engine from what is bound.
    Voice(VoiceUse),
}

/// The sample rate of a tap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TapRate {
    /// 48 kHz, the graph's rate.
    Graph48k,
    /// 16 kHz mono from a fixed 3:1 polyphase decimator: the only format Silero VAD and Whisper take. Linear phase,
    /// about 100 taps, flat to 7 kHz, at least 70 dB down above 9 kHz; its group delay (about 1 ms) is in
    /// [`TapInfo::capture_offset`].
    Speech16k,
}

impl TapRate {
    /// The rate in Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Graph48k => 48_000,
            Self::Speech16k => 16_000,
        }
    }
}

/// A tap's sample format: planar `f32` at a rate, mono or the bus's channels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct TapFormat {
    /// The sample rate.
    pub rate: TapRate,
    /// Mono (a stereo point is down-mixed) or the point's own channels.
    pub mono: bool,
}

impl VoiceUse {
    /// The signal this consumer reads unless the person changes it.
    #[must_use]
    pub const fn default_signal(self) -> VoiceSignal {
        match self {
            Self::Mouth | Self::Cues => VoiceSignal::Heard,
            Self::Captions | Self::Listen => VoiceSignal::Raw,
        }
    }

    /// The format this consumer reads: 48 kHz mono for lip sync and cues, 16 kHz mono for recognition and listening.
    #[must_use]
    pub const fn format(self) -> TapFormat {
        let rate = match self {
            Self::Mouth | Self::Cues => TapRate::Graph48k,
            Self::Captions | Self::Listen => TapRate::Speech16k,
        };
        TapFormat { rate, mono: true }
    }

    /// The drop policy: `Overwrite` for the latest-window readers, `DropNewest` for recognition and listening.
    #[must_use]
    pub const fn policy(self) -> DropPolicy {
        match self {
            Self::Mouth | Self::Cues => DropPolicy::Overwrite,
            Self::Captions | Self::Listen => DropPolicy::DropNewest,
        }
    }

    /// The default ring length: 0.5 s for lip sync and cues, 10 s for recognition (the speech host's default).
    #[must_use]
    pub const fn default_capacity(self) -> Seconds {
        match self {
            Self::Mouth | Self::Cues => Seconds(0.5),
            Self::Captions | Self::Listen => Seconds(10.0),
        }
    }

    /// Whether this use hears the person while the mic is in `CohostOnly`: only `Listen` does.
    #[must_use]
    pub const fn hears_private_talk(self) -> bool {
        matches!(self, Self::Listen)
    }
}

/// The default ring length of a recording tap (`program` or a bus): 2 s.
pub const RECORDING_CAPACITY: Seconds = Seconds(2.0);

/// How the person's voice is wired, which decides what `raw` and `heard` mean (design doc section 4.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSetup {
    /// Two inputs: the raw mic (`raw_voice`) and the changer's output (`voice`).
    TwoInputs,
    /// One input, changed by our own chain (BR-24 or ML-14).
    OwnChain,
    /// One input that is already changed outside Omuzaki: there is no raw voice.
    AlreadyChanged,
}

/// The ids the voice taps resolve to. The defaults are `mic` and `raw_voice`; the caller owns the ids, so the contract
/// holds no string it would have to parse.
#[derive(Copy, Clone, Debug)]
pub struct VoiceRouting<'a> {
    /// The bus the voice sits on.
    pub mic: &'a BusId,
    /// The role of the raw mic when a changer is used.
    pub raw_voice: &'a DeviceRole,
}

/// The tap point a voice signal resolves to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedVoiceTap {
    /// Where to tap.
    pub point: TapPoint,
    /// What the tap carries. It differs from what was asked when `fell_back`.
    pub signal: VoiceSignal,
    /// `Raw` was asked for but is not available (the input is already changed): the tap carries `Heard`, and the UI
    /// says "recognition uses the changed voice".
    pub fell_back: bool,
}

impl VoiceSignal {
    /// Resolves this signal to a tap point from how the voice is wired:
    ///
    /// | | two inputs | one input, our chain | one input already changed |
    /// |---|---|---|---|
    /// | `Raw` | `Input(raw_voice)` | `BusPreChain(mic)` | falls back to `Heard` |
    /// | `Heard` | `BusPostFader(mic)` | `BusPostFader(mic)` | `BusPostFader(mic)` |
    #[must_use]
    pub fn resolve(self, setup: VoiceSetup, routing: VoiceRouting<'_>) -> ResolvedVoiceTap {
        let heard = |fell_back| ResolvedVoiceTap {
            point: TapPoint::BusPostFader(routing.mic.clone()),
            signal: VoiceSignal::Heard,
            fell_back,
        };
        match (self, setup) {
            (Self::Heard, _) => heard(false),
            (Self::Raw, VoiceSetup::AlreadyChanged) => heard(true),
            (Self::Raw, VoiceSetup::TwoInputs) => ResolvedVoiceTap {
                point: TapPoint::Input(routing.raw_voice.clone()),
                signal: Self::Raw,
                fell_back: false,
            },
            (Self::Raw, VoiceSetup::OwnChain) => ResolvedVoiceTap {
                point: TapPoint::BusPreChain(routing.mic.clone()),
                signal: Self::Raw,
                fell_back: false,
            },
        }
    }
}

/// Which signal each voice consumer reads: the person's choice, with the defaults of section 4.2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct VoiceTaps(BTreeMap<VoiceUse, VoiceSignal>);

impl VoiceTaps {
    /// The signal `consumer` reads.
    #[must_use]
    pub fn signal(&self, consumer: VoiceUse) -> VoiceSignal {
        self.0.get(&consumer).copied().unwrap_or_else(|| consumer.default_signal())
    }

    /// Sets the signal `consumer` reads.
    pub fn set(&mut self, consumer: VoiceUse, signal: VoiceSignal) {
        self.0.insert(consumer, signal);
    }

    /// The tap point `consumer` reads, from how the voice is wired.
    #[must_use]
    pub fn resolve(&self, consumer: VoiceUse, setup: VoiceSetup, routing: VoiceRouting<'_>) -> ResolvedVoiceTap {
        self.signal(consumer).resolve(setup, routing)
    }
}

impl Default for VoiceTaps {
    /// The research's and the spec's defaults: `mouth` and `cues` hear `heard`; `captions` and `listen` hear `raw`.
    fn default() -> Self {
        let uses = [VoiceUse::Mouth, VoiceUse::Cues, VoiceUse::Captions, VoiceUse::Listen];
        Self(uses.into_iter().map(|u| (u, u.default_signal())).collect())
    }
}

/// A request for a tap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapRequest {
    /// Where to read.
    pub point: TapPoint,
    /// The sample format.
    pub format: TapFormat,
    /// How much audio the ring holds.
    pub capacity: Seconds,
    /// What happens when the reader falls behind.
    pub policy: DropPolicy,
    /// Who reads it, for the health meter's per-consumer drop counts.
    pub consumer: String,
    /// The tap carries audio out of the device (going live, a hosted connector). Only features a person enabled open
    /// one, and `AudioStatus` lists every open one (design doc section 7.2).
    #[serde(default)]
    pub leaves_device: bool,
}

impl TapRequest {
    /// The request a voice consumer makes with its defaults (format, policy, capacity).
    #[must_use]
    pub fn voice(consumer: VoiceUse, name: impl Into<String>) -> Self {
        Self {
            point: TapPoint::Voice(consumer),
            format: consumer.format(),
            capacity: consumer.default_capacity(),
            policy: consumer.policy(),
            consumer: name.into(),
            leaves_device: false,
        }
    }
}

/// A consumer that already owns a ring passes one of these instead of taking the engine's ([`crate::AudioEngine::tap`]).
/// The speech host's `AudioInput` is one.
///
/// Called on the audio thread, once per block: it must be wait-free and allocation-free, and may wake its consumer at
/// most once per block (a non-blocking futex wake).
pub trait TapSink: Send {
    /// Takes `samples` and returns how many it accepted; the engine counts the rest as dropped.
    fn push(&mut self, samples: &[f32]) -> usize;

    /// Called on the audio thread before the first `push` after the `muted` or `private` flags change. A sink consumer
    /// can then carry `private` onward (the speech host marks utterances heard in `CohostOnly`). The default ignores it.
    fn flags(&mut self, _flags: TapFlags) {}
}

/// The caller's end of a tap opened with a sink ([`crate::AudioEngine::tap`]). Dropping it closes the tap: it sets a
/// flag the audio thread reads at the next block and never waits for.
#[derive(Debug)]
pub struct TapHandle {
    closed: Arc<AtomicBool>,
}

/// The engine's end of a [`TapHandle`]: polled once per block.
#[derive(Clone, Debug)]
pub struct TapClosed {
    closed: Arc<AtomicBool>,
}

impl TapClosed {
    /// Whether the handle was dropped.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

impl TapHandle {
    /// A handle and the flag the engine polls.
    #[must_use]
    pub fn new() -> (Self, TapClosed) {
        let closed = Arc::new(AtomicBool::new(false));
        (Self { closed: Arc::clone(&closed) }, TapClosed { closed })
    }
}

impl Drop for TapHandle {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

/// What the engine says about a tap it opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapInfo {
    /// The engine-clock index of the tap's first sample, at the tap's rate.
    pub first_sample: u64,
    /// The tap's sample rate in Hz.
    pub rate: u32,
    /// How much later than the raw voice this point hears the same sound (design doc section 5.1): 0 for the raw mic,
    /// the chain's declared latency after the chain, the measured changer latency plus the chain after a changer.
    /// A consumer stamps what it makes with `time_of(sample) - capture_offset`.
    pub capture_offset: Seconds,
    /// Whether the tap goes silent when the person mutes.
    pub honours_mute: bool,
}

/// A request for a feed into a bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeedRequest {
    /// The feed; a bus names it as `SourceRef::Feed`.
    pub id: FeedId,
    /// The rate the producer writes at, in Hz; the engine resamples at the edge.
    pub rate: u32,
    /// Whether the producer writes one channel.
    pub mono: bool,
    /// How much audio the ring holds. The producer writes ahead of time (TTS), so this is also its lead.
    pub capacity: Seconds,
    /// Who writes it.
    pub consumer: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (BusId, DeviceRole) {
        (BusId::new("mic").unwrap(), DeviceRole::new("raw_voice").unwrap())
    }

    #[test]
    fn the_defaults_are_the_research_ones() {
        let taps = VoiceTaps::default();
        assert_eq!(taps.signal(VoiceUse::Mouth), VoiceSignal::Heard);
        assert_eq!(taps.signal(VoiceUse::Cues), VoiceSignal::Heard);
        assert_eq!(taps.signal(VoiceUse::Captions), VoiceSignal::Raw);
        assert_eq!(taps.signal(VoiceUse::Listen), VoiceSignal::Raw);
        let lip = TapRequest::voice(VoiceUse::Mouth, "lip sync");
        assert_eq!((lip.format.rate.hz(), lip.policy, lip.capacity), (48_000, DropPolicy::Overwrite, Seconds(0.5)));
        let asr = TapRequest::voice(VoiceUse::Captions, "speech host");
        assert_eq!((asr.format.rate.hz(), asr.policy, asr.capacity), (16_000, DropPolicy::DropNewest, Seconds(10.0)));
        assert!(asr.format.mono && lip.format.mono);
        assert!(VoiceUse::Listen.hears_private_talk() && !VoiceUse::Captions.hears_private_talk());
    }

    #[test]
    fn raw_and_heard_resolve_from_what_is_bound() {
        let (mic, raw_voice) = ids();
        let routing = VoiceRouting { mic: &mic, raw_voice: &raw_voice };
        let r = |s: VoiceSignal, setup| s.resolve(setup, routing);
        let heard = TapPoint::BusPostFader(mic.clone());
        assert_eq!(r(VoiceSignal::Raw, VoiceSetup::TwoInputs).point, TapPoint::Input(raw_voice.clone()));
        assert_eq!(r(VoiceSignal::Raw, VoiceSetup::OwnChain).point, TapPoint::BusPreChain(mic.clone()));
        let fallback = r(VoiceSignal::Raw, VoiceSetup::AlreadyChanged);
        assert_eq!(
            (fallback.point.clone(), fallback.signal, fallback.fell_back),
            (heard.clone(), VoiceSignal::Heard, true)
        );
        for setup in [VoiceSetup::TwoInputs, VoiceSetup::OwnChain, VoiceSetup::AlreadyChanged] {
            let h = r(VoiceSignal::Heard, setup);
            assert_eq!((h.point, h.fell_back), (heard.clone(), false));
        }
    }

    #[test]
    fn dropping_a_tap_handle_raises_the_flag() {
        let (handle, closed) = TapHandle::new();
        assert!(!closed.is_closed());
        drop(handle);
        assert!(closed.is_closed());
    }

    #[test]
    fn a_changed_choice_wins_over_the_default() {
        let mut taps = VoiceTaps::default();
        taps.set(VoiceUse::Captions, VoiceSignal::Heard);
        assert_eq!(taps.signal(VoiceUse::Captions), VoiceSignal::Heard);
        let text = serde_json::to_string(&taps).unwrap();
        assert_eq!(serde_json::from_str::<VoiceTaps>(&text).unwrap(), taps);
    }
}
