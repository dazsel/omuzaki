//! The suite every [`MotionFeed`] passes: the hub's, each backend's through it, the mock's and the replay's.
//!
//! Given a feed and the events that went into it (per device, in the order each device sent them), [`run`] drains it
//! frame by frame and checks what the contract promises:
//! - **nothing after `up_to`:** every event a drain returns is due by that drain's `up_to`;
//! - **frames never drop:** every event that went in comes out, releases included, however many were due at once;
//! - **order per device:** each device's events (each pad's edges) come out in the order it sent them. Across devices
//!   nothing is promised (design §2), so nothing is checked;
//! - **pads:** every pad [`MotionFeed::pads`] lists answers [`MotionFeed::pad`] with its own id.

use std::collections::BTreeMap;

use omuzaki_core::time::Seconds;
use omuzaki_input_api::event::{MotionEvent, PadSet};
use omuzaki_input_api::feed::{MOTION_FRAME_MAX, MotionFeed, MotionFrame};

/// What a run saw.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Drains made.
    pub drains: usize,
    /// Events returned.
    pub events: usize,
    /// The most events one drain returned.
    pub fullest: usize,
}

/// A broken promise.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Violation {
    /// A drain returned an event due after its `up_to`.
    #[error("drain up to {up_to:?} returned {event:?}, which is due later")]
    AfterUpTo {
        /// The drain's `up_to`.
        up_to: Seconds,
        /// The event.
        event: MotionEvent,
    },
    /// A device's events came out different from what it sent: missing, extra or reordered.
    #[error("{stream}: sent {sent:?}, drained {drained:?}")]
    Stream {
        /// Which device (`source 3`, `pad 1`).
        stream: String,
        /// What it sent.
        sent: Vec<MotionEvent>,
        /// What came out.
        drained: Vec<MotionEvent>,
    },
    /// A drain returned more than a frame holds.
    #[error("a drain returned {0} events")]
    Overfull(usize),
    /// A listed pad has no levels, or levels of another pad.
    #[error("pad {0} is listed but answers {1:?}")]
    Pad(u8, Option<omuzaki_input_api::event::PadState>),
}

/// The stream an event belongs to: its device, or for pad edges its pad.
fn stream(event: &MotionEvent) -> String {
    match (event.source(), event) {
        (Some(src), _) => format!("source {}", src.index()),
        (None, MotionEvent::PadEdges { pad, .. }) => format!("pad {}", pad.index()),
        (None, _) => "none".to_owned(),
    }
}

fn by_stream<'a>(events: impl IntoIterator<Item = &'a MotionEvent>) -> BTreeMap<String, Vec<MotionEvent>> {
    let mut out: BTreeMap<String, Vec<MotionEvent>> = BTreeMap::new();
    for event in events {
        out.entry(stream(event)).or_default().push(*event);
    }
    out
}

/// Drains `feed` every `step` from `step` to `until`, then at `until` until it is empty, and checks the promises
/// against `sent`, the events that went in.
///
/// # Errors
/// The first broken promise.
pub fn run(
    feed: &mut dyn MotionFeed,
    sent: &[MotionEvent],
    until: Seconds,
    step: Seconds,
) -> Result<Summary, Violation> {
    let mut frame = MotionFrame::new();
    let mut drained = Vec::with_capacity(sent.len());
    let mut summary = Summary::default();
    let mut pads = PadSet::new();
    let mut tick = 1u32;
    let mut idle = 0;
    // Enough drains at `until` to empty a feed that held every event at once.
    let tail = sent.len() / MOTION_FRAME_MAX + 2;
    while idle < tail {
        let up_to = Seconds((f64::from(tick) * step.0).min(until.0));
        tick = tick.saturating_add(1);
        feed.drain(up_to, &mut frame);
        let events = frame.events();
        if events.len() > MOTION_FRAME_MAX {
            return Err(Violation::Overfull(events.len()));
        }
        if let Some(late) = events.iter().find(|e| e.at().0 > up_to.0) {
            return Err(Violation::AfterUpTo { up_to, event: *late });
        }
        summary.drains += 1;
        summary.events += events.len();
        summary.fullest = summary.fullest.max(events.len());
        drained.extend_from_slice(events);
        feed.pads(&mut pads);
        for pad in pads.iter() {
            match feed.pad(pad) {
                Some(levels) if levels.pad == pad => {}
                other => return Err(Violation::Pad(pad.index(), other)),
            }
        }
        if up_to.0 >= until.0 {
            idle = if events.is_empty() { idle + 1 } else { 0 };
        }
    }
    let (want, got) = (by_stream(sent), by_stream(&drained));
    for name in want.keys().chain(got.keys()) {
        let (sent, drained) = (want.get(name).cloned().unwrap_or_default(), got.get(name).cloned().unwrap_or_default());
        if sent != drained {
            return Err(Violation::Stream { stream: name.clone(), sent, drained });
        }
    }
    Ok(summary)
}
