//! Contract for the audio engine (spec v3 §8, §9, §10; ADR 0018; design `docs/design/audio-api.md`).
//!
//! One real-time **audio graph** runs at 48 kHz on the audio thread of one clock device. Device inputs come in through
//! bridges; buses sum their sources, run an effect chain, apply gain, mute and ducking, and send to other buses; the
//! `program` bus goes out to devices. **Taps** copy a signal out of the graph into lock-free rings for the things that
//! listen; **feeds** carry audio in from the things that speak. Every node declares a fixed latency, so the engine can
//! report how late every route is.
//!
//! This crate is the contract only: types, traits, and the few pure functions the contract owns. There is no engine, no
//! device access and no thread here, so it builds for `wasm32-unknown-unknown`. The implementations are
//! `omuzaki-audio-mock` (the reference offline engine and the conformance suite, which is not part of this contract) and `omuzaki-audio` (the real engine).
//!
//! | Module | What |
//! |---|---|
//! | [`ids`] | typed ids: buses, nodes, roles, feeds, players, gates, node kinds |
//! | [`device`] | devices, roles, bindings, device state, the machine profile |
//! | [`backend`] | the seam to the OS: [`AudioBackend`] and its streams |
//! | [`graph`] | the mixer as data, its validation, and [`egress_pairs`](graph::egress_pairs) for the privacy rule |
//! | [`semantics`] | the exact arithmetic both engines reproduce: fader ramp, mute, mono to stereo, ducking |
//! | [`node`] | the [`Node`](node::Node) and [`NodeKind`](node::NodeKind) contract and `register_node_kind!` |
//! | [`ring`] | the SPSC ring behind taps and feeds |
//! | [`tap`] | taps, voice taps and feeds |
//! | [`clock`] | the engine clock |
//! | [`report`] | latency, health, events and meters |
//! | [`engine`] | [`AudioEngine`], [`OfflineRender`] and [`AudioError`] |
//! | [`preset`] | effect presets |
//! | [`voice`] | voice activity and the lip-sync visemes the face hears ([`VoiceSource`](voice::VoiceSource)) |
//!
//! # Real-time rules
//!
//! The audio thread never allocates, frees, locks, blocks or logs. Everything it calls here is `Copy` or borrows:
//! [`node::Node`] methods, [`ring::Producer::push`], [`clock::EngineClock::publish`], [`report::MeterCell::store`],
//! [`semantics::FaderRamp`]. `tests/no_alloc.rs` asserts it with a counting allocator.
#![deny(missing_docs)]

pub mod backend;
pub mod clock;
pub mod device;
pub mod engine;
pub mod graph;
pub mod ids;
pub mod node;
pub mod player;
pub mod preset;
pub mod report;
pub mod ring;
pub mod semantics;
pub mod tap;
pub mod voice;

pub use backend::{AudioBackend, BackendError, CallbackTime, DeviceStream, StreamConfig};
pub use clock::{ClockSnapshot, EngineClock};
pub use device::{Binding, ChannelMap, DeviceId, DeviceInfo, DeviceState, Direction, ProfileStore};
pub use engine::{AudioEngine, AudioError, BlockPlan, OfflineInput, OfflineOutput, OfflineRender};
pub use graph::{
    BusSpec, Channels, DuckRule, DuckTrigger, GraphError, GraphSpec, MicMode, SendSpec, SourceRef, egress_pairs,
};
pub use ids::{BusId, DeviceRole, FeedId, GateId, NodeId, NodeKindId, PlayerId, SoundId};
pub use node::{BlockMut, MAX_BLOCK, Node, NodeKind, SAMPLE_RATE};
pub use preset::FxPreset;
pub use report::{AudioEvent, AudioStatus, BridgeStatus, GraphReport, LatencyEstimate, Meters, RouteLatency};
pub use tap::{TapInfo, TapPoint, TapReader, TapRequest, TapSink};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
