//! Contract for the person's own git (spec v3 §29 VC-04; ADR 0045; design in `docs/design/git.md`).
//!
//! [`Git`] is implemented by `omuzaki-git`, which runs the `git` binary as a child process, and by `FakeGit`, the
//! in-memory stand-in. Every method is blocking, takes the project folder, and takes a `&dyn` [`Cancel`] handle: a
//! [`CancelFlag`] the host sets when the op is cancelled, or [`NeverCancel`]. This crate holds only the types, the
//! trait and the numbers both implementations share
//! ([`READ_TIMEOUT`], [`NETWORK_TIMEOUT`], [`MIN_GIT_VERSION`], [`MAX_IMPORT_FILE_BYTES`]).
#![deny(missing_docs)]

mod cancel;
mod commit;
mod error;
mod git;
mod status;

pub use cancel::{Cancel, CancelFlag, NeverCancel};
pub use commit::{Commit, CommitDetail, CommitId, CommitTree, LogRange, MAX_IMPORT_FILE_BYTES, SkipReason, TreeEntry};
pub use error::{GitError, MIN_GIT_VERSION, NETWORK_TIMEOUT, READ_TIMEOUT};
pub use git::{Git, PullOutcome};
pub use status::{ChangeKind, ChangedFile, GitStatus};
