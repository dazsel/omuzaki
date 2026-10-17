//! What the engine reports: latency by route, health, events, and meters (design doc sections 3.5, 5.4, 5.5).
//!
//! Everything here is **numbers**, which agents may read (SE-04 for audio). Captured audio is never returned.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::device::{DeviceInfo, DeviceState};
use crate::graph::MicMode;
use crate::graph::SourceRef;
use crate::ids::{BusId, DeviceRole};
use crate::tap::TapPoint;

/// Where a route ends.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteEnd {
    /// An output device, by role.
    Output {
        /// The role.
        role: DeviceRole,
    },
    /// A tap point.
    Tap {
        /// The point.
        point: TapPoint,
    },
}

/// How late one route is (BR-22): from a source, through its buses, to an output or a tap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteLatency {
    /// Where the route starts.
    pub from: SourceRef,
    /// Where it ends.
    pub to: RouteEnd,
    /// The sum of the declared latencies of the nodes along the route, in samples at 48 kHz.
    pub graph_samples: u32,
    /// Bridge buffering on the way in (zero for the clock device and for feeds).
    pub bridge_in: Seconds,
    /// Bridge buffering on the way out.
    pub bridge_out: Seconds,
    /// The devices' own latency (capture to callback, callback to playback), from the backend's timestamps.
    pub device: Seconds,
    /// The measured latency, when a loopback or clap measurement exists.
    pub measured: Option<Seconds>,
}

impl RouteLatency {
    /// The graph's share in seconds.
    #[must_use]
    pub fn graph(&self) -> Seconds {
        Seconds(f64::from(self.graph_samples) / f64::from(crate::node::SAMPLE_RATE))
    }

    /// The declared total: graph plus bridges plus devices. The measured value, when there is one, is separate.
    #[must_use]
    pub fn declared_total(&self) -> Seconds {
        Seconds(self.graph().0 + self.bridge_in.0 + self.bridge_out.0 + self.device.0)
    }
}

/// The declared latency of one bus's chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BusLatency {
    /// The bus.
    pub bus: BusId,
    /// The sum of its chain's declared latencies, in samples at 48 kHz.
    pub chain_samples: u32,
}

/// The latency picture of the compiled graph (`audio.latency.report`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphReport {
    /// Every route from a source to an output or a tap.
    pub routes: Vec<RouteLatency>,
    /// Each bus's chain latency. The `voice.fx` gates read the `mic` entry plus one 256-frame block.
    pub buses: Vec<BusLatency>,
}

/// The result of the clap test or one drift estimate (design doc sections 5.3 and 5.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LatencyEstimate {
    /// How much later the changed voice is than the raw one. Meaningful only when `rejected_reason` is `None`.
    pub lag: Seconds,
    /// The peak's normalised correlation, 0 to 1.
    pub confidence: f32,
    /// The spread (MAD) of the per-onset lags.
    pub jitter: Seconds,
    /// How many onsets paired up after shifting.
    pub onsets_matched: u32,
    /// Why the estimate was not trusted ("only 2 claps heard in the changed voice"). When set, nothing is changed.
    pub rejected_reason: Option<String>,
}

/// What drives the graph's block clock.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClockSource {
    /// A device's callback (`program_out`, else `voice`).
    Device,
    /// The internal timer, because no device is bound or the clock device was lost.
    Timer,
}

/// One bridge's health (every device except the clock device has one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BridgeStatus {
    /// The role it serves.
    pub role: DeviceRole,
    /// The device state.
    pub state: DeviceState,
    /// The device's rate when it is not 48 kHz and is resampled at the bridge.
    pub resampled_from: Option<u32>,
    /// Buffering latency right now.
    pub buffering: Seconds,
    /// Measured rate ratio against the clock device, in parts per million.
    pub drift_ppm: f32,
    /// Times the graph wanted samples the bridge did not have.
    pub underruns: u64,
    /// Times the bridge had to discard samples.
    pub overruns: u64,
}

/// Samples dropped by one consumer's tap, for the health meter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TapDrops {
    /// The consumer's name from its `TapRequest`.
    pub consumer: String,
    /// Samples lost so far: refused (a full `DropNewest` ring, or a sink that took fewer than it was given) plus
    /// overwritten (an `Overwrite` ring whose writer lapped the reader; `Producer::overwritten` in `ring`).
    pub dropped: u64,
}

/// An open tap marked `leaves_device`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LeavingTap {
    /// Who reads it.
    pub consumer: String,
    /// Where it reads.
    pub point: TapPoint,
}

/// The engine's health at a glance (the health meter's data).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioStatus {
    /// What drives the block clock.
    pub clock_source: ClockSource,
    /// The role of the clock device, when `clock_source` is `Device`.
    pub clock_role: Option<DeviceRole>,
    /// The graph's block size in frames, as the clock device delivers it (the real one, not the requested one).
    pub block_frames: u32,
    /// The state of every bound or opened role.
    pub devices: Vec<(DeviceRole, DeviceState)>,
    /// The bridges' health.
    pub bridges: Vec<BridgeStatus>,
    /// The mic's state, always visible: `CohostOnly` shows "Private: co-host only".
    pub mic_mode: MicMode,
    /// Whether the echo canceller is on.
    pub aec_on: bool,
    /// Tap drops per consumer.
    pub tap_drops: Vec<TapDrops>,
    /// Every open tap that carries audio out of the device, for the health meter ("audio leaving the device: ...").
    pub leaving_device: Vec<LeavingTap>,
    /// Model-node blocks missed so far ("voice conversion stalled: dry voice" when it is stalled).
    pub model_blocks_missed: u64,
}

/// Something that happened, for the UI, the health meter and agents (numbers and states only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioEvent {
    /// A binding's state changed.
    DeviceStateChanged {
        /// The role.
        role: DeviceRole,
        /// Its new state.
        state: DeviceState,
    },
    /// A lost device came back (the same id), after `gap`.
    Reconnected {
        /// The role.
        role: DeviceRole,
        /// How long it was gone.
        gap: Seconds,
    },
    /// A device with the same name as a lost one appeared under a new id. The engine does **not** rebind; the person
    /// confirms.
    PossibleMatch {
        /// The role that lost its device.
        role: DeviceRole,
        /// The newcomer.
        candidate: DeviceInfo,
    },
    /// The latency of the chain on the `heard` path changed (a preset added or removed a node with latency), so the A/V
    /// delay should ramp to it.
    LatencyChanged {
        /// The new declared latency of the heard path to the program output.
        heard_path: Seconds,
    },
    /// The mic's state changed.
    MicModeChanged {
        /// The new state.
        mode: MicMode,
    },
    /// The clock source changed (the clock device was lost or came back).
    ClockSourceChanged {
        /// The new source.
        source: ClockSource,
    },
}

/// How many events a subscriber's queue holds before it drops the oldest.
pub const EVENT_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Default)]
struct EventQueue {
    events: VecDeque<AudioEvent>,
    dropped: u64,
}

/// The engine's end of one subscriber's queue. Cloneable, so the engine keeps one per subscriber in a list.
///
/// Events are sent from control threads, never from the audio thread: the queue takes a short lock.
#[derive(Clone, Debug)]
pub struct EventSender {
    queue: Arc<Mutex<EventQueue>>,
}

/// One subscriber's bounded queue of events (`AudioEngine::subscribe`). Each subscriber has its own, so the UI, the
/// health meter and the ops never steal each other's events. When it is full, the oldest event is dropped and counted.
#[derive(Debug)]
pub struct AudioEvents {
    queue: Arc<Mutex<EventQueue>>,
}

/// A new queue: the engine keeps the sender, the subscriber gets the receiver.
#[must_use]
pub fn event_queue() -> (EventSender, AudioEvents) {
    let queue = Arc::new(Mutex::new(EventQueue::default()));
    (EventSender { queue: Arc::clone(&queue) }, AudioEvents { queue })
}

fn locked(queue: &Mutex<EventQueue>) -> MutexGuard<'_, EventQueue> {
    // A panic elsewhere cannot corrupt a deque of plain values, so a poisoned lock is still safe to use.
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

impl EventSender {
    /// Queues `event`, dropping the oldest one first if the queue is full.
    pub fn send(&self, event: AudioEvent) {
        let mut queue = locked(&self.queue);
        if queue.events.len() >= EVENT_QUEUE_CAPACITY {
            queue.events.pop_front();
            queue.dropped += 1;
        }
        queue.events.push_back(event);
    }

    /// Whether the subscriber still exists. The engine prunes senders whose subscriber was dropped.
    #[must_use]
    pub fn is_subscribed(&self) -> bool {
        Arc::strong_count(&self.queue) > 1
    }
}

impl AudioEvents {
    /// The oldest waiting event, if any.
    #[must_use]
    pub fn next(&self) -> Option<AudioEvent> {
        locked(&self.queue).events.pop_front()
    }

    /// Takes every waiting event, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<AudioEvent> {
        locked(&self.queue).events.drain(..).collect()
    }

    /// How many events were dropped because the queue was full, over the queue's life.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        locked(&self.queue).dropped
    }
}

/// Where a meter reads.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeterPoint {
    /// A bus, after its fader.
    Bus(BusId),
    /// A device input, by role.
    Input(DeviceRole),
}

/// One bus's (or device's) levels, as the audio thread last measured them. `Copy`: it is written and read without
/// allocation.
///
/// Peak and RMS (300 ms) are in dBFS. The last three fields are filled for the `program` bus only; elsewhere they hold
/// their neutral values (`NEG_INFINITY`, `NEG_INFINITY`, `0.0`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Meters {
    /// Peak level in dBFS.
    pub peak_db: f32,
    /// RMS level over 300 ms in dBFS.
    pub rms_db: f32,
    /// Short-term loudness (EBU R128, 3 s) in LUFS. `program` only.
    pub short_term_lufs: f32,
    /// The true peak (BS.1770-4) in dBTP. `program` only.
    pub true_peak_dbtp: f32,
    /// How far the program limiter is pulling the signal down, in dB. `program` only.
    pub limiter_gain_reduction_db: f32,
}

impl Meters {
    /// The reading of a bus with no signal and no limiter.
    pub const SILENT: Self = Self {
        peak_db: f32::NEG_INFINITY,
        rms_db: f32::NEG_INFINITY,
        short_term_lufs: f32::NEG_INFINITY,
        true_peak_dbtp: f32::NEG_INFINITY,
        limiter_gain_reduction_db: 0.0,
    };
}

impl Default for Meters {
    fn default() -> Self {
        Self::SILENT
    }
}

/// A [`Meters`] the audio thread writes and any thread reads, as `f32` bits in atomics: latest wins, no locks, no
/// allocation. The five fields are separate atomics, so a reader can see peak from one block and RMS from the next;
/// meters are for eyes and numbers, not for sample-accurate decisions.
#[derive(Debug)]
pub struct MeterCell {
    cells: [AtomicU32; 5],
}

impl MeterCell {
    /// A cell holding [`Meters::SILENT`].
    #[must_use]
    pub fn new() -> Self {
        let cell = Self { cells: std::array::from_fn(|_| AtomicU32::new(0)) };
        cell.store(Meters::SILENT);
        cell
    }

    /// Writes the latest reading (audio thread).
    pub fn store(&self, m: Meters) {
        let values = [m.peak_db, m.rms_db, m.short_term_lufs, m.true_peak_dbtp, m.limiter_gain_reduction_db];
        for (cell, value) in self.cells.iter().zip(values) {
            cell.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// Reads the latest reading (any thread).
    #[must_use]
    pub fn load(&self) -> Meters {
        let [peak_db, rms_db, short_term_lufs, true_peak_dbtp, limiter_gain_reduction_db] =
            std::array::from_fn(|i| f32::from_bits(self.cells[i].load(Ordering::Relaxed)));
        Meters { peak_db, rms_db, short_term_lufs, true_peak_dbtp, limiter_gain_reduction_db }
    }
}

impl Default for MeterCell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meter_cell_returns_what_was_stored_including_infinities() {
        let cell = MeterCell::new();
        assert_eq!(cell.load(), Meters::SILENT);
        let m = Meters {
            peak_db: -3.0,
            rms_db: -18.5,
            short_term_lufs: -16.0,
            true_peak_dbtp: -1.2,
            limiter_gain_reduction_db: 2.5,
        };
        cell.store(m);
        assert_eq!(cell.load(), m);
    }

    #[test]
    fn a_full_event_queue_drops_the_oldest_and_counts_it() {
        let (tx, rx) = event_queue();
        let mode = |n: usize| AudioEvent::ClockSourceChanged {
            source: if n.is_multiple_of(2) { ClockSource::Device } else { ClockSource::Timer },
        };
        for n in 0..EVENT_QUEUE_CAPACITY + 3 {
            tx.send(mode(n));
        }
        assert_eq!(rx.dropped(), 3);
        let all = rx.drain();
        assert_eq!(all.len(), EVENT_QUEUE_CAPACITY);
        assert_eq!(all[0], mode(3), "the three oldest are gone");
        assert!(rx.next().is_none());
        assert!(tx.is_subscribed());
        drop(rx);
        assert!(!tx.is_subscribed());
    }

    #[test]
    fn subscribers_do_not_steal_each_others_events() {
        let (tx_a, rx_a) = event_queue();
        let (tx_b, rx_b) = event_queue();
        let e = AudioEvent::MicModeChanged { mode: MicMode::Muted };
        tx_a.send(e.clone());
        tx_b.send(e.clone());
        assert_eq!((rx_a.next(), rx_b.next()), (Some(e.clone()), Some(e)));
    }

    #[test]
    fn route_latency_adds_up_in_seconds() {
        let route = RouteLatency {
            from: SourceRef::Device { role: DeviceRole::new("voice").unwrap() },
            to: RouteEnd::Output { role: DeviceRole::new("program_out").unwrap() },
            graph_samples: 480,
            bridge_in: Seconds(0.004),
            bridge_out: Seconds(0.001),
            device: Seconds(0.02),
            measured: None,
        };
        assert!((route.graph().0 - 0.01).abs() < 1e-12);
        assert!((route.declared_total().0 - 0.035).abs() < 1e-12);
    }
}
