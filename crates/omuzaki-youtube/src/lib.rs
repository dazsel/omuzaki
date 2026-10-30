//! The YouTube adapter (spec v3 BR-10): a live stream's chat, Super Chats, Super Stickers, memberships, gifted
//! memberships and new subscribers, into the show.
//!
//! - [`chat`]: YouTube's chat items as data and their normalisation into `chat.message`, `tip`, `sub`, `sub_gift` and
//!   `follow`
//! - [`api`]: the Data API calls, through the app's `HttpTransport`, and what each costs in quota
//! - [`YouTubeService`]: the polling connection, publishing into the show's `EventSink`
//! - the ops (in `ops`): `youtube.connect`, `youtube.disconnect`, `youtube.status`
//!
//! YouTube has no push socket for live chat, so the adapter polls at the interval YouTube asks for in each page and
//! counts what it spends of the project's daily quota, shown in `youtube.status`.
#![deny(missing_docs)]
// "YouTube" is a name, not code: the docs would read worse in backticks.
#![allow(clippy::doc_markdown)]

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

mod adapter;
pub mod api;
pub mod chat;
pub mod error;
pub mod ops;
mod service;

pub use adapter::{Endpoints, YouTubeStatus};
pub use service::YouTubeService;
