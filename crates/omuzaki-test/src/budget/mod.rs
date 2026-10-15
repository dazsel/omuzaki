//! The budget harness: measures the spec §10 budgets and reports them (G-BUD, M1 condition 3).
//!
//! - [`table`]: the budget table, parsed from the spec so no number is copied by hand
//! - [`timing`]: [`timing::Recorder`], the span API systems call per frame without allocating, and a start-up timer
//! - [`stats`]: mean, p50, p95, p99 and maximum of a metric's samples
//! - [`report`]: the markdown and JSON report, the comparison with the budgets and with the previous run
//! - [`runner`]: the nightly run: a synthetic workload and the process memory, written as `report.md` and `report.json`
//!
//! Every number taken here is `reported` in the spec's sense: it names the machine (OS, architecture, vCPU count) and
//! never counts as a tier's number. Nothing fails on a slow machine unless the run is asked to (`OMUZAKI_BUDGET_STRICT`).
//! The area test crates, which may depend on the systems being measured, register spans on a [`timing::Recorder`], drive
//! their own deterministic workload and build a [`report::Report`] from its tracks.

pub mod error;
pub mod report;
pub mod runner;
pub mod stats;
pub mod table;
pub mod timing;
