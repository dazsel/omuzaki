//! The budget harness: measures the spec §10 budgets and reports them (G-BUD, M1 condition 3).
//!
//! - [`table`]: the budget table, parsed from the spec so no number is copied by hand
//! - [`timing`]: [`timing::Recorder`], the span API systems call per frame without allocating, and a start-up timer
//! - [`stats`]: mean, p50, p95, p99 and maximum of a metric's samples
//! - [`report`]: the markdown and JSON report, the comparison with the budgets and with the previous run
//! - [`runner`]: the nightly run: a synthetic workload and the process memory, written as `report.md` and `report.json`
//!
//! Every report names the machine (OS, architecture, vCPU count) and states its [`report::Class`] in the spec's sense.
//! The nightly run is `reported` and never gates. A caller that knows better sets `OMUZAKI_BUDGET_CLASS`:
//!
//! - `measured` and `scaled` need the [`report::Provenance`] spec §10 asks of a gating number
//!   (`OMUZAKI_BUDGET_TIER`, `_OS_BUILD`, `_GPU`, `_BACKEND`, and `_SCALING_RULE` for `scaled`), which the report prints;
//! - a strict `relative` run (`OMUZAKI_BUDGET_STRICT=1`) needs the same `OMUZAKI_BUDGET_SESSION` in this run and in the
//!   `OMUZAKI_BUDGET_PREVIOUS` report, and fails only on a budgeted metric that is more than 10% worse. The harness's
//!   own metrics have no budget, so such a run is refused (`NoBaseline`) until an area test crate reports one.
//!
//! The area test crates, which may depend on the systems being measured, register spans on a [`timing::Recorder`], drive
//! their own deterministic workload and build a [`report::Report`] from its tracks.

pub mod error;
pub mod report;
pub mod runner;
pub mod stats;
pub mod table;
pub mod timing;
