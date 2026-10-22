//! Generation in the studio (spec v3 §14, `docs/design/connectors.md`): the image, texture, voice, music and model
//! services an artist brings, over the connector host's `connect.*` ops.
//!
//! - **The Generators page** (`settings::connectors::generators`, #1185): each generator with what it makes, whether
//!   it is ready, costs money and where it sends; its privacy notes and licence in plain words; Enabled, the address,
//!   the plan tier and Test connection, each through an op. Its key goes into the keychain through [`key_field`]
//!   (#1009), never through an op.
//! - **The Generated tray** ([`tray`], #1010): what the generators made, as tiles with their pictures, their
//!   connector, model, plan and cost, and a rights badge in words; Accept, Reject with a reason, Make another, batch
//!   decisions and a side-by-side comparison. A small form at its top starts a run.
//! - **The cost card** ([`confirm`], #1011): before a run that leaves the computer or costs money, the card says what
//!   is sent where, what it costs and what the results may be used for, and Run passes the price it showed.
//!
//! Every action is an op run as the person (`connect.*`, `settings.set`), so an agent over MCP can do the same,
//! except the key, which no op carries.

pub mod confirm;
pub mod key_field;
pub mod registration;
pub mod text;
pub mod tray;

use std::sync::Arc;

use omuzaki_connect::ops::list::Listed;
use omuzaki_connect_api::{Capability, Status};
use omuzaki_ops_api::Runner;
use omuzaki_store_api::{Commercial, Store};

use crate::ds::theme::Ds;
use crate::ds::tokens::Status as Tone;
use crate::panels::text::say;

/// What the tray needs from the app: the runner, to start runs on a thread of their own, and the store generated
/// files land in (the one the connector host writes), to show them.
#[derive(Clone)]
pub struct Generation {
    /// The app's runner.
    pub runner: Arc<dyn Runner>,
    /// The asset store, when the app has one.
    pub store: Option<Arc<dyn Store>>,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation").field("store", &self.store.is_some()).finish_non_exhaustive()
    }
}

/// A generator's status as a chip: its tone and its word.
pub fn status(ds: &Ds, status: &Status) -> (Tone, String) {
    let (tone, word) = match status {
        Status::Ready => (Tone::Ok, text::READY),
        Status::NeedsKey => (Tone::Warn, text::NEEDS_KEY),
        Status::NeedsPlan { .. } => (Tone::Warn, text::NEEDS_PLAN),
        Status::Unconfigured => (Tone::Warn, text::NOT_SET_UP),
        _ => (Tone::Info, text::DISABLED),
    };
    (tone, say(ds, word, &[]).into_owned())
}

/// A capability in words.
pub fn capability(ds: &Ds, capability: Capability) -> String {
    let word = match capability {
        Capability::Image => text::CAP_IMAGE,
        Capability::Texture => text::CAP_TEXTURE,
        Capability::Model3d => text::CAP_MODEL3D,
        Capability::Voice => text::CAP_VOICE,
        Capability::Music => text::CAP_MUSIC,
        Capability::Motion => text::CAP_MOTION,
        Capability::Llm => text::CAP_LLM,
        Capability::Vision => text::CAP_VISION,
        Capability::Dcc => text::CAP_DCC,
    };
    say(ds, word, &[]).into_owned()
}

/// Whether a file with this record may be published, and why not: the licence gate's rule (unknown is no, private
/// test material never), in words.
///
/// # Errors
/// The reason it may not be published, in words.
pub fn rights(ds: &Ds, commercial: Commercial, private_test: bool) -> Result<String, String> {
    let why = if private_test {
        text::WHY_PRIVATE
    } else {
        match commercial {
            Commercial::Ok => return Ok(say(ds, text::CAN_PUBLISH, &[]).into_owned()),
            Commercial::No => text::WHY_NONCOMMERCIAL,
            Commercial::Unknown => text::WHY_UNKNOWN,
        }
    };
    Err(say(ds, text::CANT_PUBLISH, &[("why", &say(ds, why, &[]))]).into_owned())
}

/// Whether `host` (an address's host, or a whole address) is this computer.
pub fn is_loopback(address: &str) -> bool {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if let Some(bracketed) = host.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or(bracketed)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// The generators that can run now and make `capability`.
pub fn ready_for(listed: &[Listed], capability: Capability) -> Vec<&Listed> {
    listed.iter().filter(|l| l.status == Status::Ready && l.capabilities.contains(&capability)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses_are_this_computer() {
        for local in
            ["http://127.0.0.1:8188", "http://localhost:8188/api", "http://[::1]:80", "127.0.0.2", "x.localhost"]
        {
            assert!(is_loopback(local), "{local}");
        }
        for remote in [
            "http://192.168.1.20:8188",
            "https://api.example.com",
            "http://localhost.example.com",
            "http://user@10.0.0.1",
        ] {
            assert!(!is_loopback(remote), "{remote}");
        }
    }
}
