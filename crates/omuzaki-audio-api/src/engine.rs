//! The engine as a service (design doc sections 7 and 9.1): [`AudioEngine`] for the live engine, [`OfflineRender`] for
//! deterministic tests, and [`AudioError`].
//!
//! Nothing in this module returns captured audio as audio to a caller that is an agent. The privacy checks themselves
//! (design doc section 7.2) live in the `audio.*` ops, which hold the `Actor`; the engine does what it is told.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::rng::Rng;

use crate::backend::BackendError;
use crate::clock::EngineClock;
use crate::device::{Binding, DeviceInfo, ProfileError};
use crate::graph::{GraphError, GraphSpec, MicMode, SourceRef};
use crate::ids::{BusId, DeviceRole, NodeId, PlayerId, SoundId};
use crate::node::{MAX_BLOCK, NodeError, ParamIndex};
use crate::player::Sound;
use crate::report::{AudioEvents, AudioStatus, GraphReport, LatencyEstimate, MeterPoint, Meters};
use crate::tap::{FeedRequest, FeedWriter, TapFormat, TapHandle, TapInfo, TapPoint, TapReader, TapRequest, TapSink};

/// Why an engine call failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum AudioError {
    /// The graph is not valid.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// The backend failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// The machine profile failed.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// A node could not be created.
    #[error(transparent)]
    Node(#[from] NodeError),
    /// No bus has this id in the running graph.
    #[error("no bus `{0}`")]
    UnknownBus(BusId),
    /// No node has this id in that bus.
    #[error("bus `{bus}` has no node `{node}`")]
    UnknownNode {
        /// The bus.
        bus: BusId,
        /// The missing node.
        node: NodeId,
    },
    /// The node has no parameter at this index.
    #[error("node `{node}` has no parameter {index}")]
    UnknownParam {
        /// The node.
        node: NodeId,
        /// The index that is out of range.
        index: u16,
    },
    /// The role has no bound device, so nothing can be opened for it.
    #[error("the role `{0}` is not bound to a device")]
    NotBound(DeviceRole),
    /// "Talk to co-host only" needs a bound `monitor_out`, and there is none.
    #[error("co-host only needs a monitor output: bind `monitor_out` first")]
    NeedsMonitorOutput,
    /// The tap cannot be opened (an unknown point, a format the point cannot give).
    #[error("cannot open this tap: {0}")]
    TapUnavailable(String),
    /// No sound has this id in the player bank.
    #[error("no sound `{0}` in the player bank")]
    UnknownSound(SoundId),
    /// This engine cannot do that (a mock, an engine built without a part).
    #[error("{0}")]
    Unsupported(String),
    /// A bug in the engine.
    #[error("internal audio engine error: {0}")]
    Internal(String),
}

/// The live audio engine, shared as a service (`Services::get::<dyn AudioEngine>()`).
///
/// `Send + Sync`, so it is a `omuzaki_ops_api::Service` by the blanket impl. Every method runs on a control or consumer
/// thread: none is called from the audio thread, and the engine never blocks the audio thread on any of them.
///
/// **Who may call what** is decided by the ops, not here: binding devices, entering `MicMode::Live` or
/// `CohostOnly`, and any change that would grow the set of (captured source, egress) pairs
/// ([`egress_pairs`](crate::graph::egress_pairs)) need a person (design doc section 7.2).
pub trait AudioEngine: Send + Sync {
    /// Every device the host has now.
    ///
    /// # Errors
    /// [`AudioError::Backend`] when the host cannot enumerate.
    fn devices(&self) -> Result<Vec<DeviceInfo>, AudioError>;

    /// Binds `role` to a device in the machine profile and opens it. Returns before the stream is running; watch
    /// [`AudioEvent::DeviceStateChanged`](crate::report::AudioEvent::DeviceStateChanged).
    ///
    /// # Errors
    /// [`AudioError::Profile`] when the profile cannot be written.
    fn bind(&self, role: &DeviceRole, binding: Binding) -> Result<(), AudioError>;

    /// Removes the binding of `role` and closes its stream.
    ///
    /// # Errors
    /// [`AudioError::Profile`] when the profile cannot be written.
    fn unbind(&self, role: &DeviceRole) -> Result<(), AudioError>;

    /// Diffs `graph` against the running one. Parameter-only differences (node parameters, gains, mutes, send gains,
    /// duck-rule numbers, voice-tap choices that resolve to the same points) become parameter messages, with no swap.
    /// Anything else is validated, compiled off the audio thread, and swapped in at a block boundary; surviving nodes
    /// keep their state. Returns once the change is queued.
    ///
    /// # Errors
    /// [`AudioError::Graph`] when the graph is not valid, [`AudioError::Node`] when a node cannot be created. The
    /// running graph is untouched on error.
    fn apply(&self, graph: &GraphSpec) -> Result<(), AudioError>;

    /// The graph that is running. The live setters ([`set_param`](Self::set_param),
    /// [`set_bus_gain_db`](Self::set_bus_gain_db), [`set_bus_muted`](Self::set_bus_muted),
    /// [`set_mic_mode`](Self::set_mic_mode)) update it too, so it is always true. The document stays the source of
    /// truth: the app writes it, then calls `apply` or a setter.
    fn graph(&self) -> GraphSpec;

    /// Sets a node parameter live: a `Copy` message the audio thread applies at the next block start, smoothed in the
    /// node.
    ///
    /// # Errors
    /// [`AudioError::UnknownBus`], [`AudioError::UnknownNode`] or [`AudioError::UnknownParam`].
    fn set_param(&self, bus: &BusId, node: &NodeId, param: ParamIndex, value: f32) -> Result<(), AudioError>;

    /// Sets a bus's fader live, in dB (`-inf` is silence). The ramp is [`crate::semantics::FaderRamp`].
    ///
    /// # Errors
    /// [`AudioError::UnknownBus`].
    fn set_bus_gain_db(&self, bus: &BusId, db: f32) -> Result<(), AudioError>;

    /// Mutes or unmutes a bus live (a ramp, not a cut).
    ///
    /// # Errors
    /// [`AudioError::UnknownBus`].
    fn set_bus_muted(&self, bus: &BusId, muted: bool) -> Result<(), AudioError>;

    /// The mic's state.
    fn mic_mode(&self) -> MicMode;

    /// Changes the mic's state, sends [`AudioEvent::MicModeChanged`](crate::report::AudioEvent::MicModeChanged), and returns where it landed: asking for `Live`
    /// while in `CohostOnly` lands on `Muted` ([`MicMode::after_leaving_cohost`]), so ending a private talk can never put
    /// the mic on stream by accident; the person asks again to go live.
    ///
    /// # Errors
    /// [`AudioError::NeedsMonitorOutput`] when entering `CohostOnly` with no bound `monitor_out`.
    fn set_mic_mode(&self, mode: MicMode) -> Result<MicMode, AudioError>;

    /// Opens a tap that feeds a consumer-owned ring through `sink`. Called by a consumer that already has a ring (the
    /// speech host). Dropping the returned [`TapHandle`] closes the tap.
    ///
    /// # Errors
    /// [`AudioError::TapUnavailable`] when the point or format cannot be tapped.
    fn tap(&self, request: &TapRequest, sink: Box<dyn TapSink>) -> Result<(TapHandle, TapInfo), AudioError>;

    /// Opens a tap into a ring the engine makes, and returns its reading end. Dropping the [`TapReader`] closes the tap.
    ///
    /// # Errors
    /// [`AudioError::TapUnavailable`] when the point or format cannot be tapped.
    fn tap_ring(&self, request: &TapRequest) -> Result<(TapReader, TapInfo), AudioError>;

    /// Opens a feed into a bus (a `SourceRef::Feed` names it) and returns its writing end. Dropping the [`FeedWriter`]
    /// closes the feed, and its source plays silence.
    ///
    /// # Errors
    /// [`AudioError::Internal`] when the ring cannot be made; an unknown feed id is accepted and plays once a bus names
    /// it.
    fn feed(&self, request: &FeedRequest) -> Result<FeedWriter, AudioError>;

    /// Puts a decoded sound in the player bank under `id` (replacing one there), for players to play. An engine
    /// without a player refuses.
    ///
    /// # Errors
    /// [`AudioError::Unsupported`] by default; [`AudioError::Internal`] when the bank is full.
    fn load_sound(&self, id: &SoundId, sound: Arc<Sound>) -> Result<(), AudioError> {
        let _ = (id, sound);
        Err(AudioError::Unsupported("this audio engine has no player".to_owned()))
    }

    /// Takes a sound out of the bank; voices playing it fade out first.
    ///
    /// # Errors
    /// [`AudioError::UnknownSound`]; [`AudioError::Unsupported`] by default.
    fn unload_sound(&self, id: &SoundId) -> Result<(), AudioError> {
        let _ = id;
        Err(AudioError::Unsupported("this audio engine has no player".to_owned()))
    }

    /// Plays a bank sound on `player` at `gain_db`; `ducks` raises the player's gate ([`gate_of`](crate::player::gate_of)) while it sounds.
    /// The player is made the first time it is named.
    ///
    /// # Errors
    /// [`AudioError::UnknownSound`]; [`AudioError::Unsupported`] by default.
    fn play(&self, player: &PlayerId, sound: &SoundId, gain_db: f32, ducks: bool) -> Result<(), AudioError> {
        let _ = (player, sound, gain_db, ducks);
        Err(AudioError::Unsupported("this audio engine has no player".to_owned()))
    }

    /// Fades out `sound` on `player`, or every sound it plays with `None`.
    ///
    /// # Errors
    /// [`AudioError::UnknownSound`]; [`AudioError::Unsupported`] by default.
    fn stop(&self, player: &PlayerId, sound: Option<&SoundId>) -> Result<(), AudioError> {
        let _ = (player, sound);
        Err(AudioError::Unsupported("this audio engine has no player".to_owned()))
    }

    /// The engine clock, shareable with any thread.
    fn clock(&self) -> Arc<EngineClock>;

    /// The health meter's data.
    fn status(&self) -> AudioStatus;

    /// The latency of every route in the running graph.
    fn report(&self) -> GraphReport;

    /// The latest levels at a bus or an input. Numbers only: agents may read them.
    fn meters(&self, at: &MeterPoint) -> Option<Meters>;

    /// Stores a validated changer-latency measurement (or clears it with `None`). The clap-test op opens two
    /// `DropNewest` taps with [`tap_ring`](Self::tap_ring), runs the estimator, and calls this; there is no clap test on
    /// the engine. The stored value feeds the changer's `capture_offset` and `D = auto`; it does not touch the A/V delay
    /// setting itself.
    ///
    /// # Errors
    /// [`AudioError::Profile`] when the machine profile cannot be written.
    fn set_changer_latency(&self, measured: Option<LatencyEstimate>) -> Result<(), AudioError>;

    /// A new queue of events: bounded at [`EVENT_QUEUE_CAPACITY`](crate::report::EVENT_QUEUE_CAPACITY) per subscriber, dropping the oldest and counting
    /// the drops.
    fn subscribe(&self) -> AudioEvents;
}

/// Planar audio: one `Vec<f32>` per channel, all the same length.
pub type Planar = Vec<Vec<f32>>;

/// How an offline render cuts its input into blocks. The output must not depend on it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockPlan {
    /// Every block has this many frames (the last may be shorter).
    Fixed {
        /// Frames per block, `1..=MAX_BLOCK`.
        frames: usize,
    },
    /// Blocks of seeded random sizes in `1..=max_frames`.
    Random {
        /// The seed: the same seed gives the same plan.
        seed: u64,
        /// The largest block, at most `MAX_BLOCK`.
        max_frames: usize,
    },
}

impl BlockPlan {
    /// The block sizes that cover exactly `total` frames. Each is in `1..=MAX_BLOCK`; a size outside that range in the
    /// plan is clamped into it.
    #[must_use]
    pub fn sizes(self, total: usize) -> Vec<usize> {
        let mut sizes = Vec::new();
        let mut left = total;
        let mut rng = match self {
            Self::Random { seed, .. } => Some(Rng::new(seed)),
            Self::Fixed { .. } => None,
        };
        while left > 0 {
            let size = match (self, rng.as_mut()) {
                (Self::Random { max_frames, .. }, Some(rng)) => {
                    let max = max_frames.clamp(1, MAX_BLOCK) as u64;
                    // The remainder is below `max`, which fits `usize`.
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        1 + (rng.next_u64() % max) as usize
                    }
                }
                (Self::Fixed { frames }, _) => frames.clamp(1, MAX_BLOCK),
                (Self::Random { .. }, None) => 1,
            };
            let size = size.min(left);
            sizes.push(size);
            left -= size;
        }
        sizes
    }
}

/// What an offline render is given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OfflineInput {
    /// How many frames to render.
    pub frames: usize,
    /// The audio of each source, planar at 48 kHz. A missing source is silence.
    pub sources: BTreeMap<SourceRef, Planar>,
    /// The taps to capture, one per point, in the given format.
    pub taps: BTreeMap<TapPoint, TapFormat>,
    /// The mic's state for the whole render.
    pub mic_mode: MicMode,
}

/// What an offline render returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OfflineOutput {
    /// Each bus's output after the fader.
    pub buses: BTreeMap<BusId, Planar>,
    /// What each output role would have played (the sum of the buses that name it).
    pub outputs: BTreeMap<DeviceRole, Planar>,
    /// Each requested tap, mono unless the format says otherwise.
    pub taps: BTreeMap<TapPoint, Vec<f32>>,
}

/// Deterministic rendering with no devices, no threads and no clock (design doc section 9.1).
///
/// Implemented twice, by the mock (the reference) and by the engine's executor in offline mode. A property test renders
/// random graphs through both with random block plans and requires them to agree to 1e-6; each must also be
/// bit-identical across runs and across block plans.
pub trait OfflineRender {
    /// Renders `input` through `graph`, cutting it into blocks as `plan` says.
    ///
    /// # Errors
    /// [`AudioError::Graph`] when the graph is not valid, [`AudioError::Node`] when a node cannot be created,
    /// [`AudioError::TapUnavailable`] when a tap cannot be captured.
    fn render(&self, graph: &GraphSpec, input: &OfflineInput, plan: BlockPlan) -> Result<OfflineOutput, AudioError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_plans_cover_the_input_exactly() {
        for total in [0usize, 1, 255, 256, 5000] {
            for plan in [
                BlockPlan::Fixed { frames: 256 },
                BlockPlan::Fixed { frames: 1 },
                BlockPlan::Fixed { frames: 4096 },
                BlockPlan::Random { seed: 7, max_frames: 300 },
            ] {
                let sizes = plan.sizes(total);
                assert_eq!(sizes.iter().sum::<usize>(), total, "{plan:?} {total}");
                assert!(sizes.iter().all(|&s| (1..=MAX_BLOCK).contains(&s)), "{plan:?}");
            }
        }
    }

    #[test]
    fn a_random_plan_is_seeded_and_varied() {
        let a = BlockPlan::Random { seed: 1, max_frames: 64 }.sizes(10_000);
        assert_eq!(a, BlockPlan::Random { seed: 1, max_frames: 64 }.sizes(10_000));
        assert_ne!(a, BlockPlan::Random { seed: 2, max_frames: 64 }.sizes(10_000));
        assert!(a.iter().any(|&s| s != a[0]));
    }

    #[test]
    fn the_engine_is_a_service() {
        fn assert_service<S: ?Sized + omuzaki_ops_api::Service>() {}
        assert_service::<dyn AudioEngine>();
    }
}
