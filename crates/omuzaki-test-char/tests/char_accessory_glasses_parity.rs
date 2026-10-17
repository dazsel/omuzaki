//! The `glasses` accessory against the prototype's dump (CB-43, #332): glasses.
//!
//! The prototype's dump is the parity target for the accessory's size. The prototype's sample character wears no glasses, so
//! today every dump holds no such mesh and the test skips with a message (see `reference_meshes`); it compares as soon as
//! a dump with one exists. Without the dump (always, in CI and in the cloud) it skips too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod char_accessory_support;

use char_accessory_support::{built, compare_extents, reference_meshes};
use serde_json::json;

#[test]
fn the_prototypes_glasses_is_the_size_of_ours() {
    let Some(reference) = reference_meshes("glasses", &["glasses", "spectacle"]) else { return };
    // The head sits on the standard template's body in the prototype's units, so every template's accessory is checked.
    for template in ["soft-sd", "standard"] {
        compare_extents("glasses", &built(template, "glasses", json!({})), &reference);
    }
}
