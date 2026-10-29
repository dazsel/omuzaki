//! The crate's first-party library items (design D14): the files of `library/items/`, embedded by `build.rs` and
//! offered through one [`ItemSource`]. An item is a manifest whose recipe runs this crate's ops: the jewellery
//! (#2413) adds a `ring`, `earrings`, `bracelet` or `filigree-collar` part; the braids and the ahoge (#2411) add a group
//! to an existing hair part; `mature` applies the character style (#2410). Adding an item is adding a file.

use omuzaki_library_api::{ItemManifest, ItemSource};

include!(concat!(env!("OUT_DIR"), "/items.rs"));

/// The items of `library/items/`.
#[derive(Default)]
pub struct Seed;

impl ItemSource for Seed {
    fn id(&self) -> &'static str {
        "omuzaki-char"
    }

    /// Every file that parses; the crate's tests prove every one does, and checks and applies it.
    fn items(&self) -> Vec<ItemManifest> {
        FILES.iter().filter_map(|(_, text)| ItemManifest::parse(text).ok()).collect()
    }
}

omuzaki_library_api::register_library_source!(Seed);

/// The embedded files, as `(file stem, text)`, for the tests.
pub fn files() -> &'static [(&'static str, &'static str)] {
    FILES
}
