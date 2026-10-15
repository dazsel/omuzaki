//! Test harness for Omuzaki: golden-image comparison and the store behind it (spec v3 §34).
//!
//! - [`compare`]: [`compare::compare`] two images under a [`compare::Tolerance`], with PSNR and a red diff image
//! - [`goldens`]: the `check` flow and `--bless`; a golden is a manifest entry in git plus an image in the assets folder
//! - [`manifest`]: `tests/goldens/<suite>.toml`, the per-branch part: case -> image hash and tolerance
//! - [`objects`]: the content-addressed image store, `<OMUZAKI_GOLDENS>/objects/<hash>.png`
//! - [`dirs`]: where the manifests, the store and the evidence folder are found
//! - [`migrate`]: the one-off move from the old `<suite>/<name>.png` layout
//! - [`verify`]: every manifest case against the object store (missing or damaged images)
//! - [`gc`]: remove images no branch or worktree names any more (dry run unless told to apply)
//! - [`summary`]: the PR-comment text built from what the checks found
//! - [`meshcmp`]: [`meshcmp::compare_meshes`], per-vertex, Chamfer and Hausdorff mesh distances (the geometry parity oracle)
//! - [`regioncmp`]: [`compare::compare`] inside a pixel rectangle, with a markdown table and a side-by-side picture
//! - [`budget`]: the spec §10 budget harness: the budget table parsed from the spec, per-system timing spans, and a markdown
//!   and JSON report compared with the budgets and the previous night (`tools/ci/budget-report.sh`)
//! - [`eval`]: agent eval task files, fixtures and eval sets: format, discovery, loader, lint and provenance
//! - [`jsref`]: the JS prototype's reference dumps (under the assets folder) loaded into meshes, hair clumps, materials and face state
//!
//! The `omuzaki-test` binary wraps these for scripts and CI; see the crate `README.md` for the workflow. The library
//! depends on no implementation crate: tests across crates belong in the area test crates (`omuzaki-test-<area>`).
#![deny(missing_docs)]

pub mod budget;
pub mod compare;
pub mod dirs;
pub mod error;
pub mod eval;
mod fsio;
pub mod gc;
mod git;
pub mod goldens;
pub mod jsref;
pub mod manifest;
pub mod meshcmp;
pub mod migrate;
pub mod objects;
pub mod regioncmp;
pub mod summary;
pub mod verify;
