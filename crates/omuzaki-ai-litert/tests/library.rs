//! The runtime is registered as `tflite`, and, gated on the real archive, the pinned library loads with its version
//! and ABI checked. The gated test skips with a message without `OMUZAKI_LITERT_CACHE` (and `OMUZAKI_LITERT_SHA256`).
#![allow(clippy::doc_markdown)] // product names are prose

mod common;

use omuzaki_ai_api::{RuntimeId, find_runtime};
use omuzaki_ai_litert::builds::select_here;
use omuzaki_ai_litert::{RUNTIME_ID, load_archive, loaded};

#[test]
fn the_runtime_is_registered_as_tflite() {
    omuzaki_ai_litert::link();
    assert!(find_runtime(&RuntimeId::new(RUNTIME_ID).unwrap()).is_some());
}

#[test]
fn the_pinned_library_loads_once_with_its_version_and_abi_checked() {
    let Some((archive, sha256)) = common::litert_archive("the library test") else { return };
    let first = load_archive(&archive, &sha256).unwrap();
    assert_eq!(first.build, select_here().unwrap());
    // Once per process: a second call, or a lookup, gives the same function table, not a second load.
    assert_eq!(load_archive(&archive, &sha256).unwrap(), first);
    assert_eq!(loaded(), Some(first));
}
