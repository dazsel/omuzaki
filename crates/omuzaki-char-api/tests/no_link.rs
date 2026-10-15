//! Facts read and merge in a binary that never calls `omuzaki_char_api::link()`. This is its own file because every file
//! in `tests/` is its own binary: a test that must run without the link can't share one with a test that calls it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{PartId, Published, Publishes, Stature, StatureFact};

#[test]
fn facts_merge_and_read_in_a_binary_that_never_calls_link() {
    let part = PartId::new("body").unwrap();
    let stature = Stature { scale: 1.0, height_m: 1.7 };
    let mut published = Published::default();
    published.absorb(&part, Publishes::default().with::<StatureFact>(stature)).unwrap();
    assert_eq!(*published.get::<StatureFact>(), Some(stature));
}
