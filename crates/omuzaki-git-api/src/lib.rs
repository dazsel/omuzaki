//! Contract for the person's own git (spec v3 §29 VC-04; ADR 0045; design in `docs/design/git.md`).
//!
//! [`Git`](git::Git) is implemented by `omuzaki-git`, which runs the `git` binary as a child process, and by `FakeGit`,
//! the in-memory stand-in. Every method is blocking, takes the project folder, and takes a `&dyn`
//! [`Cancel`](cancel::Cancel) handle: a [`CancelFlag`](cancel::CancelFlag) the host sets when the op is cancelled, or
//! [`NeverCancel`](cancel::NeverCancel). This crate holds only the types, the trait and the numbers both
//! implementations share. Callers import by path (ADR 0054 §2), for example `omuzaki_git_api::git::Git`:
//! - [`git`]: the [`Git`](git::Git) trait and [`PullOutcome`](git::PullOutcome)
//! - [`cancel`]: the [`Cancel`](cancel::Cancel) trait and its two handles
//! - [`commit`]: commit ids, log ranges, commit details and trees, and
//!   [`MAX_IMPORT_FILE_BYTES`](commit::MAX_IMPORT_FILE_BYTES)
//! - [`status`]: the working-tree [`GitStatus`](status::GitStatus) and its changed files
//! - [`error`]: [`GitError`](error::GitError) and the shared limits ([`READ_TIMEOUT`](error::READ_TIMEOUT),
//!   [`NETWORK_TIMEOUT`](error::NETWORK_TIMEOUT), [`MIN_GIT_VERSION`](error::MIN_GIT_VERSION))
#![deny(missing_docs)]

pub mod cancel;
pub mod commit;
pub mod error;
pub mod git;
pub mod status;
