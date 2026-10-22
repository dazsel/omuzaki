//! Contract for the OS input source (ADR 0060, `docs/design/input-motion.md`): what keyboards, mice, pens and gamepads
//! give the performance, the bindings (PF-05) and triggers (PF-25) they fire, and locomotion intents (PF-30), with the
//! privacy rules made structural.
//!
//! - **No key identity in any public type** (design §3, D2). A key is its [`KeyZone`](zones::KeyZone), a cell of a
//!   fixed, coarse grid; the map from a key to its zone, chords, and every raw code live inside `omuzaki-input` only. A
//!   pointer is relative motion, never a screen position; a pen is a 32 x 32 cell. The one stated exception is
//!   [`ChordLabel`](binding::ChordLabel), a configured chord's display text, which only the binding editor receives and
//!   which cannot be serialised or displayed.
//! - **Capture is off by default, started only by a person, never restored, and runs only while an indicator is
//!   shown** (design §4, D3): [`InputControl::start`](feed::InputControl::start) fails with
//!   [`InputError::NoIndicator`](capture::InputError::NoIndicator) unless an installed
//!   [`IndicatorHandle`](capture::IndicatorHandle) has a fresh heartbeat. Anyone may pause or stop.
//! - **Bindings and motion share the source** (design §5, D4): the frame thread drains
//!   [`MotionFeed`](feed::MotionFeed) and [`IntentFeed`](feed::IntentFeed), the dispatcher polls
//!   [`BindingFeed`](feed::BindingFeed), and every hit arrives resolved to its [`BindingId`](binding::BindingId) and
//!   action; a release always ends what its press started.
//!
//! The surface handles ([`IndicatorHost`](feed::IndicatorHost), [`ChordStore`](feed::ChordStore),
//! [`ProtectList`](feed::ProtectList)) are handed to the surface directly and never put in `Services`, so no op, MCP
//! tool or agent can reach them.
//!
//! ## Paths
//!
//! Items are imported by module path (`omuzaki_input_api::zones::KeyZone`); there is no flat re-export list
//! (ADR 0054 §2).
#![deny(missing_docs)]

pub mod binding;
pub mod capture;
pub mod event;
pub mod feed;
pub mod zones;
