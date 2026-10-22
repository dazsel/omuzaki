//! The nightly run: what `tools/ci/budget-report.sh` executes.
//!
//! The harness crate depends on no implementation crate, so the run measures what it owns: a synthetic system that
//! busy-waits a fixed time (proof that a span reads back the time it covered, and the noise floor of the box) and the
//! process's peak memory. Real systems are measured by the area test crates through the same [`Report`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::error::BudgetError;
use super::report::{Class, Machine, Metric, Provenance, Report};
use super::table::{Unit, parse_spec};
use super::timing::{Recorder, StartupTimer};

/// Key of the synthetic busy-wait system. Not a §10 row, so it is reported and never judged.
pub const SYNTHETIC_KEY: &str = "harness.synthetic";
/// How long the synthetic system busy-waits per frame.
pub const SYNTHETIC_WORK: Duration = Duration::from_millis(2);
/// Frames of the synthetic system.
pub const SYNTHETIC_FRAMES: usize = 500;

/// Where the run reads and writes.
#[derive(Debug, Clone)]
pub struct Config {
    /// The spec holding the §10 table.
    pub spec: PathBuf,
    /// Folder that receives `report.md` and `report.json`.
    pub out: PathBuf,
    /// The previous night's `report.json`, if there is one.
    pub previous: Option<PathBuf>,
    /// What the numbers are. Only a class other than [`Class::Reported`] can fail a strict run, because `reported`
    /// numbers never gate (spec §10); a cloud runner is `reported`.
    pub class: Class,
    /// Fail on the findings this class gates on instead of only reporting them.
    pub strict: bool,
    /// What the caller states about the hardware and the session: required by `measured` and `scaled`, and the session
    /// by a strict `relative` run.
    pub provenance: Option<Provenance>,
}

impl Config {
    /// Read the configuration from `OMUZAKI_BUDGET_OUT` (required), `OMUZAKI_BUDGET_SPEC`, `OMUZAKI_BUDGET_PREVIOUS`,
    /// `OMUZAKI_BUDGET_CLASS` (default `reported`), `OMUZAKI_BUDGET_STRICT` (`1` turns it on) and the provenance
    /// variables of [`Provenance::from_env`].
    ///
    /// # Errors
    /// [`BudgetError::Spec`] when `OMUZAKI_BUDGET_OUT` is not set; [`BudgetError::Class`] for an unknown class.
    pub fn from_env() -> Result<Self, BudgetError> {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        Ok(Self {
            spec: var("OMUZAKI_BUDGET_SPEC").unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/product/02-studio-spec-v3.md")
            }),
            out: var("OMUZAKI_BUDGET_OUT")
                .ok_or_else(|| BudgetError::Spec("OMUZAKI_BUDGET_OUT is not set".to_owned()))?,
            previous: var("OMUZAKI_BUDGET_PREVIOUS"),
            class: var("OMUZAKI_BUDGET_CLASS").map_or(Ok(Class::Reported), |c| Class::parse(&c.to_string_lossy()))?,
            strict: std::env::var("OMUZAKI_BUDGET_STRICT").is_ok_and(|v| v == "1"),
            provenance: Provenance::from_env(),
        })
    }
}

/// Busy-wait for `d` on the monotonic clock.
fn busy_wait(d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}

/// Peak resident memory of this process in bytes (`VmHWM`), where the OS exposes it.
fn peak_rss_bytes() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib: f64 =
        status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?.trim().strip_suffix("kB")?.trim().parse().ok()?;
    Some(kib * 1024.0)
}

/// Run the measurements and build the report, without touching the disk beyond reading the spec and the previous report.
///
/// # Errors
/// Any [`BudgetError`] from reading the spec or the previous report, or from the recorder;
/// [`BudgetError::Provenance`] for a `measured` or `scaled` run that does not state its provenance;
/// [`BudgetError::NoSession`] for a strict `relative` run whose session is not the previous report's.
pub fn measure(config: &Config) -> Result<Report, BudgetError> {
    Provenance::require(config.provenance.as_ref(), config.class)?;
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|source| BudgetError::Io { path: p.to_owned(), source });
    let mut startup = StartupTimer::begin();
    let budgets = parse_spec(&read(&config.spec)?)?;
    startup.phase("spec");
    let previous = config.previous.as_deref().map(|p| read(p).and_then(|t| Report::from_json(&t))).transpose()?;
    if let (true, Some(before)) = (config.strict && config.class == Class::Relative, &previous) {
        let session = |p: Option<&Provenance>| p.and_then(|p| p.session.clone());
        let ours = session(config.provenance.as_ref());
        if ours.is_none() || ours != session(before.provenance.as_ref()) {
            return Err(BudgetError::NoSession);
        }
    }

    let mut recorder = Recorder::new(SYNTHETIC_FRAMES);
    let id = recorder.register(SYNTHETIC_KEY)?;
    startup.phase("recorder");
    for _ in 0..SYNTHETIC_FRAMES {
        let span = recorder.span(id);
        busy_wait(SYNTHETIC_WORK);
        recorder.end(span)?;
    }

    let mut metrics: Vec<Metric> =
        recorder.tracks().filter_map(|(k, s, dropped)| Metric::from_ns(k, s, dropped)).collect();
    metrics.extend(Metric::from_ns("harness.startup", &[startup.total_ns()], 0));
    let mut notes = vec![format!(
        "`{SYNTHETIC_KEY}` busy-waits {} ms per frame for {SYNTHETIC_FRAMES} frames, unpaced.",
        SYNTHETIC_WORK.as_millis()
    )];
    match peak_rss_bytes() {
        Some(b) => metrics.extend(Metric::single("harness.peak_rss", Unit::Bytes, b)),
        None => notes.push(
            "peak memory is not available on this OS; VRAM is not measured here (no GPU in the harness).".to_owned(),
        ),
    }
    Ok(Report::build(Machine::current(), metrics, &budgets, previous.as_ref(), config.class, notes)
        .with_provenance(config.provenance.clone()))
}

/// Measure, write `report.md` and `report.json` into the output folder and print the markdown.
///
/// # Errors
/// Any [`BudgetError`] from measuring or writing; [`BudgetError::Violations`] when `strict` and the report has any;
/// [`BudgetError::NoBaseline`] when `strict` and `relative` compared no budgeted metric; [`BudgetError::NothingToJudge`]
/// when `strict` and `measured` or `scaled` judged none.
pub fn run(config: &Config) -> Result<Report, BudgetError> {
    let report = measure(config)?;
    let write = |name: &str, text: String| {
        let path = config.out.join(name);
        std::fs::write(&path, text).map_err(|source| BudgetError::Io { path, source })
    };
    std::fs::create_dir_all(&config.out).map_err(|source| BudgetError::Io { path: config.out.clone(), source })?;
    let markdown = report.to_markdown();
    write("report.md", markdown.clone())?;
    write("report.json", report.to_json()?)?;
    println!("{markdown}");
    // Without a budgeted metric compared with the previous run nothing can fail, so a strict relative run would pass
    // having judged nothing (the harness's own metrics have no §10 budget).
    if config.strict && config.class == Class::Relative && report.compared_budgeted() == 0 {
        return Err(BudgetError::NoBaseline);
    }
    // Likewise a `measured` or `scaled` run judges nothing when no metric has a budget for its tier.
    if config.strict && matches!(config.class, Class::Measured | Class::Scaled) && report.judged() == 0 {
        return Err(BudgetError::NothingToJudge(format!("{:?}", config.class).to_lowercase()));
    }
    let failures = report.failures();
    if config.strict && !failures.is_empty() {
        return Err(BudgetError::Violations(failures.join("; ")));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::report::Verdict;

    fn config(dir: &Path) -> Config {
        Config {
            spec: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/product/02-studio-spec-v3.md"),
            out: dir.to_owned(),
            previous: None,
            class: Class::Relative,
            strict: true,
            provenance: None,
        }
    }

    // The entry `tools/ci/budget-report.sh` runs; ignored so a plain `cargo test` never writes a report.
    #[test]
    #[ignore = "the nightly entry: run by tools/ci/budget-report.sh"]
    fn nightly_from_env() {
        run(&Config::from_env().unwrap()).unwrap();
    }

    #[test]
    fn the_busy_wait_system_shows_up_and_the_report_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(&Config { strict: false, ..config(dir.path()) }).unwrap();
        let row = report.rows.iter().find(|r| r.metric.key == SYNTHETIC_KEY).unwrap();
        // A busy-wait never returns early, so 2 ms is a hard floor; the ceiling is loose for a loaded runner.
        assert!(row.metric.stats.p50 >= 2.0 && row.metric.stats.p50 < 50.0, "{:?}", row.metric.stats);
        assert_eq!((row.metric.samples, row.verdict), (SYNTHETIC_FRAMES as u64, Verdict::NoBudget));
        let json = std::fs::read_to_string(dir.path().join("report.json")).unwrap();
        assert_eq!(Report::from_json(&json).unwrap(), report);
        assert!(std::fs::read_to_string(dir.path().join("report.md")).unwrap().contains(SYNTHETIC_KEY));
    }

    #[test]
    fn a_missing_spec_or_previous_report_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path());
        c.previous = Some(dir.path().join("nope.json"));
        assert!(matches!(measure(&c), Err(BudgetError::Io { .. })));
        c.previous = None;
        c.spec = dir.path().join("nope.md");
        assert!(matches!(measure(&c), Err(BudgetError::Io { .. })));
    }

    fn provenance() -> Provenance {
        Provenance {
            tier: "reference".into(),
            os_build: "linux 6.18".into(),
            gpu_adapter: "software".into(),
            backend: "vulkan".into(),
            scaling_rule: Some("§10 GPU rule".into()),
            session: Some("s1".into()),
        }
    }

    #[test]
    fn measured_and_scaled_runs_need_their_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config { class: Class::Measured, strict: false, ..config(dir.path()) };
        let missing = |c: &Config| match measure(c) {
            Err(BudgetError::Provenance { missing, .. }) => missing,
            other => panic!("{other:?}"),
        };
        assert!(missing(&c).contains("OMUZAKI_BUDGET_TIER") && missing(&c).contains("OMUZAKI_BUDGET_BACKEND"));
        c.provenance = Some(Provenance { scaling_rule: None, ..provenance() });
        let report = measure(&c).unwrap();
        assert!(report.to_markdown().contains("Provenance: tier reference, OS build linux 6.18, GPU software"));
        c.class = Class::Scaled;
        assert_eq!(missing(&c), "OMUZAKI_BUDGET_SCALING_RULE");
        c.provenance = Some(provenance());
        assert!(measure(&c).is_ok());
        // Strict, with the harness's own metrics (none has a §10 budget), it would pass having judged nothing.
        c.strict = true;
        assert!(matches!(run(&c), Err(BudgetError::NothingToJudge(class)) if class == "scaled"));
        c.class = Class::Measured;
        assert!(matches!(run(&c), Err(BudgetError::NothingToJudge(class)) if class == "measured"));
    }

    #[test]
    fn a_strict_relative_run_needs_a_session_shared_with_the_previous_report_and_a_budgeted_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config { provenance: Some(provenance()), ..config(dir.path()) };
        // No previous report: nothing was compared.
        assert!(matches!(run(&c), Err(BudgetError::NoBaseline)));
        let mut before = measure(&Config { strict: false, ..c.clone() }).unwrap();
        let prev_path = dir.path().join("previous.json");
        std::fs::write(&prev_path, before.to_json().unwrap()).unwrap();
        c.previous = Some(prev_path.clone());
        // The same session, but the harness's own metrics have no budget, so nothing could fail: refused.
        assert!(matches!(run(&c), Err(BudgetError::NoBaseline)));
        // Another session, or none: refused before anything is compared.
        before.provenance = Some(Provenance { session: Some("s0".into()), ..provenance() });
        std::fs::write(&prev_path, before.to_json().unwrap()).unwrap();
        assert!(matches!(run(&c), Err(BudgetError::NoSession)));
        c.provenance = None;
        assert!(matches!(run(&c), Err(BudgetError::NoSession)));
    }

    #[test]
    fn a_relative_run_that_is_not_strict_reports_a_slowdown_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config { provenance: Some(provenance()), strict: false, ..config(dir.path()) };
        let mut previous = measure(&c).unwrap();
        // Pretend last night's busy-wait was a thousand times as fast: a slowdown of a metric with no budget that no
        // amount of load on this machine can hide (halving it could: a slow first run left today under twice it, #2266).
        for row in &mut previous.rows {
            if row.metric.key == SYNTHETIC_KEY {
                row.metric.stats.p95 /= 1000.0;
            }
        }
        let prev_path = dir.path().join("previous.json");
        std::fs::write(&prev_path, previous.to_json().unwrap()).unwrap();
        c.previous = Some(prev_path);
        let report = run(&c).unwrap();
        assert!(report.rows.iter().any(|r| r.metric.key == SYNTHETIC_KEY && r.regression));
        c.class = Class::Reported;
        c.strict = true;
        assert!(run(&c).is_ok(), "reported numbers never gate");
    }
}
