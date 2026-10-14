//! Reduce motion: the manual override wins, an OS change applies on the next frame, and the platform output parsers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use omuzaki_ui::ds::os_prefs::{
    MotionPrefs, OsMotionSource, parse_gnome_enable_animations, parse_macos_defaults, parse_portal_reduced_motion,
    parse_windows_mask, run_probe,
};
use omuzaki_ui::ds::{Settings, apply_settings, ds};

/// 0 unknown, 1 motion on, 2 reduced; moved by hand like a `FakeClock`.
struct Fake(AtomicU8, AtomicUsize);

impl Fake {
    fn set(&self, v: u8) {
        self.0.store(v, Ordering::Relaxed);
    }
}

impl OsMotionSource for Fake {
    fn reduce_motion(&self) -> Option<bool> {
        match self.0.load(Ordering::Relaxed) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    fn refresh(&self) {
        self.1.fetch_add(1, Ordering::Relaxed);
    }
}

/// A private directory per test process for probe captures.
fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("omuzaki-os-prefs-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir); // a failure shows up as the probe tests failing
    dir
}

fn prefs(initial: u8) -> (Arc<Fake>, MotionPrefs) {
    let fake = Arc::new(Fake(AtomicU8::new(initial), AtomicUsize::new(0)));
    (Arc::clone(&fake), MotionPrefs::new(fake))
}

#[test]
fn follows_the_os_without_an_override() {
    let (_, p) = prefs(2);
    assert!(p.effective());
    let (_, p) = prefs(1);
    assert!(!p.effective());
}

#[test]
fn unknown_os_value_leaves_motion_on() {
    let (_, p) = prefs(0);
    assert!(!p.effective());
}

#[test]
fn manual_override_wins_both_ways() {
    let (_, mut p) = prefs(2);
    p.set_manual(Some(false));
    assert!(!p.effective());
    let (_, mut p) = prefs(1);
    p.set_manual(Some(true));
    assert!(p.effective());
    p.set_manual(None);
    assert!(!p.effective());
}

#[test]
fn an_os_change_applies_on_the_next_frame() {
    let (fake, p) = prefs(1);
    let ctx = egui::Context::default();
    apply_settings(&ctx, p.resolve(Settings::default()));
    assert!(!ds(&ctx).settings.reduce_motion);
    fake.set(2);
    apply_settings(&ctx, p.resolve(Settings::default()));
    assert!(ds(&ctx).settings.reduce_motion);
}

#[test]
fn the_override_holds_through_os_changes() {
    let (fake, mut p) = prefs(1);
    p.set_manual(Some(true));
    fake.set(1);
    assert!(p.effective());
    fake.set(2);
    assert!(p.effective());
}

#[test]
fn resolve_keeps_the_other_settings() {
    let (_, p) = prefs(2);
    let base = Settings { reduce_motion: false, ..Settings::default() };
    let out = p.resolve(base);
    assert!(out.reduce_motion);
    assert_eq!((out.theme, out.scale, out.density), (base.theme, base.scale, base.density));
}

#[test]
fn windows_mask_parser() {
    let on = "\nHKEY_CURRENT_USER\\Control Panel\\Desktop\n    UserPreferencesMask    REG_BINARY    9E3E078012000000\n";
    let off = "    UserPreferencesMask    REG_BINARY    9C12038010000000\n";
    assert_eq!(parse_windows_mask(on), Some(false));
    assert_eq!(parse_windows_mask(off), Some(true));
    assert_eq!(parse_windows_mask("nothing"), None);
}

#[test]
fn macos_parser() {
    assert_eq!(parse_macos_defaults("1\n"), Some(true));
    assert_eq!(parse_macos_defaults("0\n"), Some(false));
    assert_eq!(parse_macos_defaults("does not exist"), None);
}

#[test]
fn linux_parsers() {
    assert_eq!(parse_portal_reduced_motion("(<<uint32 1>>,)\n"), Some(true));
    assert_eq!(parse_portal_reduced_motion("(<<uint32 0>>,)\n"), Some(false));
    assert_eq!(parse_portal_reduced_motion("Error: not found"), None);
    assert_eq!(parse_gnome_enable_animations("true\n"), Some(false));
    assert_eq!(parse_gnome_enable_animations("false\n"), Some(true));
    assert_eq!(parse_gnome_enable_animations("''"), None);
}

#[test]
fn focus_gain_asks_the_source_to_refresh_once() {
    let (fake, mut p) = prefs(1);
    p.observe_focus(true);
    p.observe_focus(false);
    p.observe_focus(false);
    assert_eq!(fake.1.load(Ordering::Relaxed), 0);
    p.observe_focus(true);
    p.observe_focus(true);
    assert_eq!(fake.1.load(Ordering::Relaxed), 1);
}

#[cfg(unix)]
#[test]
fn a_hung_probe_is_cut_off_and_reads_as_unknown() {
    let started = Instant::now();
    assert_eq!(run_probe(&scratch(), "sleep", &["30"], Duration::from_millis(200)), None);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn a_failed_or_missing_probe_reads_as_unknown() {
    assert_eq!(run_probe(&scratch(), "false", &[], Duration::from_secs(2)), None);
    assert_eq!(run_probe(&scratch(), "omuzaki-no-such-tool", &[], Duration::from_secs(2)), None);
    assert_eq!(run_probe(&scratch(), "echo", &["1"], Duration::from_secs(2)).as_deref(), Some("1\n"));
}

#[cfg(unix)]
#[test]
fn a_descendant_holding_stdout_open_cannot_outlast_the_deadline() {
    let started = Instant::now();
    // The shell exits at once; the background sleep inherits stdout for 3 s. The probe answers when the tool exits.
    assert_eq!(
        run_probe(&scratch(), "sh", &["-c", "sleep 3 & echo hi"], Duration::from_millis(300)).as_deref(),
        Some("hi\n")
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn probes_leave_no_capture_files_behind() {
    let dir = scratch().join("cleanup");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    // Success, failure, timeout, and a descendant that keeps the output open: every path cleans up.
    assert_eq!(run_probe(&dir, "echo", &["1"], Duration::from_secs(2)).as_deref(), Some("1\n"));
    assert_eq!(run_probe(&dir, "false", &[], Duration::from_secs(2)), None);
    assert_eq!(run_probe(&dir, "sleep", &["30"], Duration::from_millis(100)), None);
    assert_eq!(
        run_probe(&dir, "sh", &["-c", "sleep 2 & echo hi"], Duration::from_millis(300)).as_deref(),
        Some("hi\n")
    );
    assert_eq!(std::fs::read_dir(&dir).expect("read scratch").count(), 0);
}

#[cfg(unix)]
#[test]
fn output_past_the_cap_is_unknown_and_stops_early() {
    let dir = scratch().join("oversize");
    let _ = std::fs::create_dir_all(&dir);
    let started = Instant::now();
    // `yes` writes for ever; the size check kills it long before the 5 s deadline.
    assert_eq!(run_probe(&dir, "yes", &[], Duration::from_secs(5)), None);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(std::fs::read_dir(&dir).expect("read scratch").count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_descendant_does_not_outlive_the_probe() {
    let dir = scratch().join("descendant");
    let _ = std::fs::create_dir_all(&dir);
    let pid_file = dir.join("pid");
    let script = format!("sleep 30 & echo $! > {}; echo hi", pid_file.display());
    // The probe answers, and the background sleep inheriting the output file is killed with the tree.
    assert_eq!(run_probe(&dir, "sh", &["-c", &script], Duration::from_secs(2)).as_deref(), Some("hi\n"));
    let pid = std::fs::read_to_string(&pid_file).expect("pid written");
    let gone = || match std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
        Err(_) => true,
        Ok(stat) => stat.contains(") Z"),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while !gone() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(gone(), "the descendant is still running");
}

#[cfg(target_os = "linux")]
#[test]
fn a_background_flooder_is_dead_when_the_probe_returns() {
    let dir = scratch().join("flooder");
    let _ = std::fs::create_dir_all(&dir);
    // The flooder writes to the capture file for ever; it must be dead after the call, and the file gone.
    let out = run_probe(&dir, "sh", &["-c", "(while :; do echo x; done) & echo $!"], Duration::from_secs(2));
    let pid = out.and_then(|o| o.lines().next().map(str::to_owned));
    assert_eq!(std::fs::read_dir(&dir).expect("read scratch").count(), 0);
    if let Some(pid) = pid {
        let gone = || match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat.contains(") Z"),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gone() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(), "the flooder is still running");
    }
}
