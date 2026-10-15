//! The live-metrics snapshot: how the studio is doing right now (fps, frame time, latency, late frames, audio
//! underruns, budget use), as one small plain-data record any thread can read.
//!
//! [`metrics`] makes the hub: one writer per section ([`FrameWriter`], [`PerformWriter`], [`TrackerWriter`],
//! [`AudioWriter`], none `Clone`, so one writer each) and a cloneable [`MetricsReader`]. A writer publishes through a
//! seqlock built from safe atomics: it takes no lock, never waits and allocates nothing. A reader never blocks a
//! writer; a read that races a publish is retried and never returned torn. After 16 tries the section reads as `None`.
//!
//! The record holds numbers and enum states only (no names, device ids or paths). The owner of each subsystem calls
//! its writer; the renderer, tracker and audio engine know nothing about metrics. Design:
//! `docs/research/2026-10-live-metrics-snapshot.md` §2 to §4.

// `loom` models the cell with `RUSTFLAGS="--cfg loom"`; cargo does not know that cfg.
#![allow(unexpected_cfgs)]
#![deny(missing_docs)]

mod cell;
mod sections;
mod words;

pub mod budget;

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use budget::{BudgetKey, BudgetUse};
use cell::SeqCell;
use sections::{AUDIO_WORDS, FRAME_WORDS, PERFORM_WORDS, TRACKER_WORDS};
pub use sections::{
    AudioMetrics, CalibrationPhase, FacePressure, FrameMetrics, PerformMetrics, TrackerMetrics, TrackingState,
};

/// One consistent reading of every section, with the budget rows derived from them.
///
/// A section is `None` until its writer has published once, or when a read lost every retry. Every value is live, not
/// the §10 gate statistic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct LiveMetrics {
    /// The frame loop's section.
    pub frame: Option<FrameMetrics>,
    /// The perform loop's section.
    pub perform: Option<PerformMetrics>,
    /// The tracker's section.
    pub tracker: Option<TrackerMetrics>,
    /// The audio engine's section.
    pub audio: Option<AudioMetrics>,
    /// §10 rows derived from the sections at read time; a row whose reading is absent is left out.
    pub budget: Vec<BudgetUse>,
}

/// The cells, shared by the writers and the readers.
#[derive(Debug)]
struct Hub {
    frame: SeqCell<FRAME_WORDS>,
    perform: SeqCell<PERFORM_WORDS>,
    tracker: SeqCell<TRACKER_WORDS>,
    audio: SeqCell<AUDIO_WORDS>,
}

/// Makes the hub: one writer per section and a cloneable reader. Allocates once, here.
pub fn metrics() -> (MetricsWriters, MetricsReader) {
    let hub = Arc::new(Hub {
        frame: SeqCell::new(),
        perform: SeqCell::new(),
        tracker: SeqCell::new(),
        audio: SeqCell::new(),
    });
    let writers = MetricsWriters {
        frame: FrameWriter { hub: Arc::clone(&hub) },
        perform: PerformWriter { hub: Arc::clone(&hub) },
        tracker: TrackerWriter { hub: Arc::clone(&hub) },
        audio: AudioWriter { hub: Arc::clone(&hub) },
    };
    (writers, MetricsReader { hub })
}

/// The writers, one per section. Move each to the thread that owns its subsystem.
#[derive(Debug)]
#[non_exhaustive]
pub struct MetricsWriters {
    /// For the stage frame loop.
    pub frame: FrameWriter,
    /// For the perform thread.
    pub perform: PerformWriter,
    /// For the control thread that owns the tracker.
    pub tracker: TrackerWriter,
    /// For the audio control thread.
    pub audio: AudioWriter,
}

macro_rules! writer {
    ($(#[$doc:meta])* $name:ident, $section:ident, $cell:ident) => {
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name {
            hub: Arc<Hub>,
        }

        impl $name {
            /// Publishes `m` as the section's newest value. `&mut self` makes this the only writer of the cell. It
            /// takes no lock, never waits and allocates nothing.
            pub fn publish(&mut self, m: &$section) {
                self.hub.$cell.store(&m.to_words());
            }
        }
    };
}

writer!(
    /// The only writer of the frame section.
    FrameWriter, FrameMetrics, frame
);
writer!(
    /// The only writer of the perform section.
    PerformWriter, PerformMetrics, perform
);
writer!(
    /// The only writer of the tracker section.
    TrackerWriter, TrackerMetrics, tracker
);
writer!(
    /// The only writer of the audio section.
    AudioWriter, AudioMetrics, audio
);

/// Reads the hub from any thread. Cheap to clone.
#[derive(Clone, Debug)]
pub struct MetricsReader {
    hub: Arc<Hub>,
}

impl MetricsReader {
    /// The current reading. Infallible and never blocks a writer. It allocates only the `budget` vector, on the
    /// reader's thread.
    pub fn read(&self) -> LiveMetrics {
        let frame = self.hub.frame.load().and_then(|w| FrameMetrics::from_words(&w));
        let perform = self.hub.perform.load().and_then(|w| PerformMetrics::from_words(&w));
        let tracker = self.hub.tracker.load().and_then(|w| TrackerMetrics::from_words(&w));
        let audio = self.hub.audio.load().and_then(|w| AudioMetrics::from_words(&w));
        let budget = budget::derive(frame.as_ref(), tracker.as_ref(), audio.as_ref());
        LiveMetrics { frame, perform, tracker, audio, budget }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use omuzaki_core::time::Seconds;

    use super::*;

    #[test]
    fn every_section_is_none_before_its_first_publish() {
        let (mut w, reader) = metrics();
        let live = reader.read();
        assert_eq!((live.frame, live.perform, live.tracker, live.audio), (None, None, None, None));
        assert_eq!(live.budget, Vec::new());

        let mut m = PerformMetrics::new(Seconds(2.0));
        m.skipped_steps = 3;
        w.perform.publish(&m);
        let live = reader.read();
        assert_eq!(live.perform, Some(m), "published section reads back");
        assert_eq!(live.frame, None, "the others stay None");
    }

    #[test]
    fn the_newest_publish_wins_and_readers_share_the_hub() {
        let (mut w, reader) = metrics();
        let other = reader.clone();
        for fps in [30.0, 60.0] {
            let mut m = FrameMetrics::new(Seconds(1.0));
            m.fps = fps;
            w.frame.publish(&m);
        }
        assert_eq!(other.read().frame.map(|f| f.fps), Some(60.0));
    }

    #[test]
    fn budget_rows_follow_the_sections() {
        let (mut w, reader) = metrics();
        let mut t = TrackerMetrics::new(Seconds(1.0));
        t.track_ms_p95 = 10.0;
        w.tracker.publish(&t);
        let live = reader.read();
        assert_eq!(live.budget.len(), 1);
        assert_eq!(live.budget[0].key, BudgetKey::TrackFace);
        assert!((live.budget[0].ratio - 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_schema_and_json_carry_only_numbers_and_states() {
        let (mut w, reader) = metrics();
        w.audio.publish(&AudioMetrics::new(Seconds(1.0)));
        let json = serde_json::to_value(reader.read()).expect("serialises");
        assert_eq!(json["audio"]["underruns"], 0);
        assert_eq!(json["frame"], serde_json::Value::Null);
        let back: LiveMetrics = serde_json::from_value(json).expect("deserialises");
        assert_eq!(back, reader.read());
        let schema = serde_json::to_value(schemars::schema_for!(LiveMetrics)).expect("schema");
        assert!(schema["properties"].get("budget").is_some());
    }
}
