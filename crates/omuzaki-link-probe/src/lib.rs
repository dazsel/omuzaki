//! A test-only WebAssembly App: the smallest binary that links a `registers` crate (`omuzaki-char`) the way every App
//! does, through its generated `link()` (ADR 0015, ADR 0054 §6).
//!
//! `cargo xtask wasm-link-proof` builds it for `wasm32-unknown-unknown` (one codegen unit per crate, from
//! `.cargo/config.toml`), instantiates the module in node and calls [`omuzaki_link_probe_registrations`], which links
//! and then lists every self-registration that made it into the module. xtask compares the list with what the source
//! declares. On wasm the linker drops any object file nothing references and rustc adds no reference to `#[used]`
//! statics, so this is the one place a missing `link()` call shows: natively every registration survives anyway.
//!
//! inventory's constructors run without help here: the module has command-style linkage (no export calls
//! `__wasm_call_ctors`), so wasm-ld calls the constructors at the top of each export.

// The two exports need fixed symbol names for node to find them, and `no_mangle` is an unsafe attribute. They take no
// pointers and do nothing unsafe.
#![allow(unsafe_code)]

use std::sync::OnceLock;

omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}

/// The registration list, built once per instance so the pointer and length exports describe the same bytes.
static LISTING: OnceLock<String> = OnceLock::new();

/// Links this App's registering crates, as every App does at startup, then lists every self-registration in the
/// module, one [`omuzaki_core::link::Registered`] line each (the form `cargo xtask registry` prints).
///
/// With the `unlinked` feature the build script generates an empty `link()`: the negative case, which must lose the
/// registrations on wasm.
pub fn listing() -> &'static str {
    LISTING.get_or_init(|| {
        link();
        let mut listing = String::new();
        for registered in omuzaki_core::link::registered() {
            listing.push_str(&registered.to_string());
            listing.push('\n');
        }
        listing
    })
}

/// The address of [`listing`]'s UTF-8 bytes in the module's memory, for the node runner.
#[unsafe(no_mangle)]
pub extern "C" fn omuzaki_link_probe_registrations() -> *const u8 {
    listing().as_ptr()
}

/// The length in bytes of [`listing`], for the node runner.
#[unsafe(no_mangle)]
pub extern "C" fn omuzaki_link_probe_registrations_len() -> usize {
    listing().len()
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg_attr(feature = "unlinked", ignore = "the negative case links nothing, by design")]
    fn the_native_listing_has_a_registration_of_the_linked_crate_per_line() {
        let listing = super::listing();
        assert!(listing.lines().any(|line| line.starts_with("omuzaki-char\t")), "{listing}");
        assert!(listing.lines().all(|line| line.split('\t').count() == 4), "{listing}");
    }
}
