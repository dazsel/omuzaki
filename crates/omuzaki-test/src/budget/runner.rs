//! The nightly run: what `tools/ci/budget-report.sh` executes.
//!
//! The harness crate depends on no implementation crate, so the run measures what it owns: a synthetic system that
//! busy-waits a fixed time (proof that a span reads back the time it covered, and the noise floor of the box) and the
//! process's peak memory. Real systems are measured by the area test crates through the same [`Report`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::error::BudgetError;
use super::report::{Class, Machine, Metric, Report};
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
}

impl Config {
    /// Read the configuration from `OMUZAKI_BUDGET_OUT` (required), `OMUZAKI_BUDGET_SPEC`, `OMUZAKI_BUDGET_PREVIOUS`
    /// `OMUZAKI_BUDGET_CLASS` (default `reported`) and `OMUZAKI_BUDGET_STRICT` (`1` turns it on).
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
/// Any [`BudgetError`] from reading the spec or the previous report, or from the recorder.
pub fn measure(config: &Config) -> Result<Report, BudgetError> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|source| BudgetError::Io { path: p.to_owned(), source });
    let mut startup = StartupTimer::begin();
    let budgets = parse_spec(&read(&config.spec)?)?;
    startup.phase("spec");
    let previous = config.previous.as_deref().map(|p| read(p).and_then(|t| Report::from_json(&t))).transpose()?;

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
    Ok(Report::build(Machine::current(), metrics, &budgets, previous.as_ref(), config.class, notes))
}

/// Measure, write `report.md` and `report.json` into the output folder and print the markdown.
///
/// # Errors
/// Any [`BudgetError`] from measuring or writing; [`BudgetError::Violations`] when `strict` and the report has any.
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
        let report = run(&config(dir.path())).unwrap();
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

    #[test]
    fn strict_runs_fail_on_a_regression_only_for_a_gating_class() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(dir.path());
        let mut previous = measure(&c).unwrap();
        // Pretend last night's busy-wait was twice as fast: today is a 100% slowdown.
        for row in &mut previous.rows {
            if row.metric.key == SYNTHETIC_KEY {
                row.metric.stats.p95 /= 2.0;
            }
        }
        let prev_path = dir.path().join("previous.json");
        std::fs::write(&prev_path, previous.to_json().unwrap()).unwrap();
        c.previous = Some(prev_path);
        assert!(matches!(run(&c), Err(BudgetError::Violations(_))));
        c.strict = false;
        assert!(run(&c).is_ok());
        // Reported numbers never gate, however much slower than last night.
        c.strict = true;
        c.class = Class::Reported;
        assert!(run(&c).is_ok());
    }
}
