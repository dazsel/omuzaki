//! The Twitch adapter (spec v3 BR-10): a channel's events, from `EventSub` over its WebSocket, into the show.
//!
//! - [`eventsub`]: the protocol as data, the subscriptions ([`TOPICS`](eventsub::TOPICS)) and the normalisation into
//!   the automation events `redeem`, `chat.message`, `follow`, `raid`, `cheer` and `sub`
//! - [`helix`]: the token check and the subscription call, through the app's `HttpTransport`
//! - [`TwitchService`]: the connection, publishing into the automation engine's `EventSink`
//! - the ops (in `ops`): `twitch.connect`, `twitch.disconnect`, `twitch.status`
//!
//! Written from Twitch's public `EventSub` and Helix references.

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

mod adapter;
pub mod error;
pub mod eventsub;
pub mod helix;
pub mod ops;
mod service;
mod socket;

pub use adapter::{Endpoints, Skipped, TwitchStatus};
pub use service::TwitchService;
