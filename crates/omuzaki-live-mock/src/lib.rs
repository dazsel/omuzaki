//! The test kit for show automation (design `docs/design/automation.md` D16), over `omuzaki-live-api` only:
//! - [`harness`]: [`NodeHarness`](harness::NodeHarness) runs one node instance with scripted inputs at fake times, the
//!   op calls it makes, scripted replies and the outputs it fires, ordered exactly as the engine orders them. One
//!   node, one PR: a node task's tests are harness tests, with no engine, executor or network.
//! - [`runner`]: [`FakeRunner`](runner::FakeRunner) answers op calls from scripts, with a latency.
//! - [`wall`]: [`FakeWall`](wall::FakeWall), wall time from a start instant plus a fake clock.
//! - [`variables`]: [`MemoryVariables`](variables::MemoryVariables), a `VariableStore` in memory that counts saves.
//! - [`approvals`]: [`FakeApprovals`](approvals::FakeApprovals), an `Approvals` built in a test.
//! - [`fixtures`]: synthetic events with invented names and messages. Recorded chat is never a fixture.
#![deny(missing_docs)]

pub mod approvals;
pub mod fixtures;
pub mod harness;
mod log;
pub mod runner;
pub mod variables;
pub mod wall;
