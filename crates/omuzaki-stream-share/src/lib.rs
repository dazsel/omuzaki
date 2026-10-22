//! Texture-sharing outputs (spec v3 BR-01): the program picture, with transparency, straight into OBS and other apps on
//! the same computer.
//!
//! | Kind | OS | Receivers | Alpha sent |
//! |---|---|---|---|
//! | `spout` | Windows | OBS's Spout2 Capture source (Composite mode: Premultiplied Alpha) and other Spout receivers | premultiplied |
//! | `syphon` | macOS | OBS's Syphon Client source and other Syphon clients | straight |
//!
//! Both are [`Share`] outputs: `submit` copies the frame into a one-frame mailbox and returns; the output's own thread
//! swaps the newest frame out, reorders it to BGRA8 and hands it to the platform's [`Publisher`]. A frame that arrives
//! before the thread took the previous one replaces it and counts as dropped: a shared texture shows the newest picture,
//! never a queue. The publisher is opened on that thread, so its Direct3D or Metal objects never cross threads.
//!
//! Alpha follows what the main receiver does with it. Spout has no alpha-mode flag; OBS's Spout2 source offers a
//! "Premultiplied Alpha" composite mode, which takes the renderer's premultiplied picture with no conversion
//! (`spikes/broadcast-out`, measured: at most 1.1 levels of error). OBS's Syphon source has no such mode and blends its
//! texture as straight alpha, as OBS does for every source, so Syphon gets the straight form. The stream layer converts
//! (`AlphaNeed`); outputs never do.

mod share;
pub mod spout;
pub mod syphon;

pub use share::{Opener, Publisher, Share, rgba_to_bgra};

use omuzaki_stream_api::output::OutputKind;

omuzaki_stream_api::register_output!(OutputKind {
    name: spout::KIND,
    summary: "a Spout2 sender with transparency, for OBS's Spout2 Capture source and other apps on this Windows computer",
    available: spout::available,
    make: || Box::new(spout::output()),
});

omuzaki_stream_api::register_output!(OutputKind {
    name: syphon::KIND,
    summary: "a Syphon server with transparency, for OBS's Syphon Client source and other apps on this Mac",
    available: syphon::available,
    make: || Box::new(syphon::output()),
});

omuzaki_core::define_link!();
