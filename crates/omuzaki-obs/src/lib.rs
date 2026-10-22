//! OBS Studio remote control (obs-websocket v5) for the BR-19 broadcast bridge: the protocol as plain data, a client
//! that speaks it, the app's connection service and the `obs.*` ops.
//!
//! # Clean-room
//!
//! The types are written from the public protocol document only: `docs/generated/protocol.md` of the
//! `obsproject/obs-websocket` repository (heading "obs-websocket 5.x.x Protocol", RPC version 1), read on 2026-10-15
//! from `https://raw.githubusercontent.com/obsproject/obs-websocket/master/docs/generated/protocol.md`. That
//! repository is GPL-2.0-or-later, so no text or code was copied from it, from OBS Studio or from any client library,
//! and nothing links to them. Where the document and the task text differ, the document wins.
//!
//! # Names and branding
//!
//! "OBS" and "OBS Studio" are names used here in plain text only, to say what the protocol talks to. There is no logo
//! and no claim of affiliation. The OBS Project's brand guidance was not part of the source and is not checked in
//! here: it is noted for the legal review before release.
//!
//! # Modules
//!
//! | Module | Holds |
//! |---|---|
//! | [`protocol`] | the [`Frame`](protocol::Frame) and [`Message`](protocol::Message), every handshake message, the supported requests with their responses, and [`ObsEvent`](protocol::ObsEvent) |
//! | [`auth`] | the answer to the server's authentication challenge |
//! | [`limits`] | the default port, the RPC version, [`EventSubscription`](limits::EventSubscription) and [`CloseCode`](limits::CloseCode) |
//! | [`client`] | [`ObsClient`](client::ObsClient): one connection, requests matched by id, events to an [`EventHub`](client::EventHub) |
//! | [`service`] | [`ObsService`](service::ObsService): the app's one connection, kept up with backoff, shared by the ops, the UI and automations |
//! | [`settings`] | `obs.host`, `obs.port`, `obs.allow_remote`, `obs.autoconnect` |
//! | [`ops`] | `obs.connect`, `obs.disconnect`, `obs.status`, `obs.scenes`, `obs.sources` and the nine actions: `obs.scene.set`, `obs.source.set_visible`, `obs.stream.start`/`stop`, `obs.record.start`/`stop`, `obs.replay.start`/`stop`/`save` |
//! | `testing` | `FakeObs`, a loopback OBS for tests (`cfg(test)` or the `testing` feature) |
#![deny(missing_docs)]

pub mod auth;
pub mod client;
pub mod limits;
pub mod ops;
pub mod protocol;
pub mod service;
pub mod settings;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

// The go-live notice reads the character and scene contracts, which register their document keys (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_stage_api::link();
}
