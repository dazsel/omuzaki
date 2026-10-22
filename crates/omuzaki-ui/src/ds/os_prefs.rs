//! Reduce motion from the operating system, with the person's own choice on top (SE-02, `design-system.md` section 8.1).
//!
//! [`MotionPrefs`] answers one question per frame: should the UI reduce motion? The answer is the manual override when
//! the person set one, otherwise whatever the OS says. The OS value comes from an [`OsMotionSource`]. [`SystemWatcher`]
//! is the real one: a background thread asks the platform at startup, when the window regains focus and on a slow
//! fallback timer, each probe under a hard timeout, and publishes the answer through an atomic, so the UI thread never
//! waits on a process. A failed or unknown probe publishes "unknown", which leaves the manual toggle in charge. Tests install a fake source and need no OS.
//!
//! Windows is asked directly (`SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)`, the "Animation effects" setting), so
//! no process is started there and none can outlive the probe (#794). Elsewhere each system's own command line tool is
//! used: macOS `defaults` (`accessibilityDisplayShouldReduceMotion` is backed by the `reduceMotion` accessibility key),
//! Linux the XDG desktop portal (`org.freedesktop.appearance`) then GNOME's `enable-animations`. The web build reads
//! `prefers-reduced-motion` in the browser shell and hands it in through [`OsMotionSource`]; this crate is native only.
//! The parsers are pure functions, tested on captured output.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::theme::Settings;

/// Where the OS preference comes from.
pub trait OsMotionSource: Send + Sync {
    /// `Some(true)` when the OS asks for less motion, `Some(false)` when it does not, `None` when unknown (then
    /// motion is left on).
    fn reduce_motion(&self) -> Option<bool>;

    /// Asks the source to look again soon (the window regained focus, where a settings change is most likely to have
    /// just happened). Sources that always read fresh ignore it.
    fn refresh(&self) {}
}

/// The person's choice plus the OS value, resolved each frame.
pub struct MotionPrefs {
    source: Arc<dyn OsMotionSource>,
    manual: Option<bool>,
    was_focused: bool,
}

impl MotionPrefs {
    /// Follows `source` until a manual choice is set.
    pub fn new(source: Arc<dyn OsMotionSource>) -> Self {
        Self { source, manual: None, was_focused: true }
    }

    /// Reports the window's focus each frame; a focus gain asks the source to refresh.
    pub fn observe_focus(&mut self, focused: bool) {
        if focused && !self.was_focused {
            self.source.refresh();
        }
        self.was_focused = focused;
    }

    /// Sets the manual override; `None` goes back to following the OS.
    pub fn set_manual(&mut self, manual: Option<bool>) {
        self.manual = manual;
    }

    /// The manual override, if the person set one.
    pub fn manual(&self) -> Option<bool> {
        self.manual
    }

    /// The effective setting: the override wins, otherwise the OS value, otherwise motion stays on. Reads the source
    /// afresh, so an OS change applies on the next frame.
    pub fn effective(&self) -> bool {
        self.manual.or_else(|| self.source.reduce_motion()).unwrap_or(false)
    }

    /// `settings` with `reduce_motion` resolved; call before [`apply_settings`](super::theme::apply_settings) each frame.
    pub fn resolve(&self, settings: Settings) -> Settings {
        Settings { reduce_motion: self.effective(), ..settings }
    }
}

const UNKNOWN: u8 = 0;
const MOTION_ON: u8 = 1;
const MOTION_REDUCED: u8 = 2;

/// The slow fallback poll, for changes made while the window had focus (or on a platform with no focus signal).
const FALLBACK_EVERY: Duration = Duration::from_secs(30);

/// No probe may run longer than this; a hung tool is killed and counts as unknown.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The real source: a background thread running the platform probe. Dropping it stops the thread.
pub struct SystemWatcher {
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    wake: Mutex<Option<Sender<()>>>,
}

/// The watcher thread's loop: probe, publish, then wait for a refresh or the fallback timer. Pending refreshes are
/// coalesced into one probe, and `stop` is checked before every probe so a dropped watcher launches no more tools.
fn watch(
    rx: &Receiver<()>,
    stop: &AtomicBool,
    published: &AtomicU8,
    fallback: Duration,
    probe: impl Fn() -> Option<bool>,
) {
    while !stop.load(Ordering::Relaxed) {
        published.store(encode(probe()), Ordering::Relaxed);
        match rx.recv_timeout(fallback) {
            // A refresh request or the fallback timer: probe again. The sender dropping ends the thread.
            Ok(()) | Err(RecvTimeoutError::Timeout) => while rx.try_recv().is_ok() {},
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl SystemWatcher {
    /// Starts watching: one probe now, one per [`refresh`](OsMotionSource::refresh), and one every 30 s. When the
    /// thread cannot start the watcher reports `None` for ever.
    pub fn start() -> Self {
        let state = Arc::new(AtomicU8::new(UNKNOWN));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<()>();
        let (published, halt) = (Arc::clone(&state), Arc::clone(&stop));
        let spawned = std::thread::Builder::new()
            .name("os-motion-pref".into())
            .spawn(move || watch(&rx, &halt, &published, FALLBACK_EVERY, probe_platform));
        if let Err(e) = spawned {
            tracing::warn!("reduce-motion watcher did not start: {e}");
        }
        Self { state, stop, wake: Mutex::new(Some(tx)) }
    }
}

impl Drop for SystemWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Dropping the sender wakes a waiting thread at once so it sees `stop`.
        if let Ok(mut guard) = self.wake.lock() {
            guard.take();
        }
    }
}

impl OsMotionSource for SystemWatcher {
    fn reduce_motion(&self) -> Option<bool> {
        match self.state.load(Ordering::Relaxed) {
            MOTION_ON => Some(false),
            MOTION_REDUCED => Some(true),
            _ => None,
        }
    }

    fn refresh(&self) {
        if let Ok(guard) = self.wake.lock()
            && let Some(tx) = guard.as_ref()
        {
            // A closed channel means the thread never started or has ended; nothing to wake.
            let _ = tx.send(());
        }
    }
}

fn encode(v: Option<bool>) -> u8 {
    match v {
        None => UNKNOWN,
        Some(false) => MOTION_ON,
        Some(true) => MOTION_REDUCED,
    }
}

/// Runs `program`, returning its stdout when it exits successfully within [`PROBE_TIMEOUT`]; a tool that is missing,
/// fails or hangs gives `None` (a hung one is killed).
#[cfg(not(target_os = "windows"))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    run_within(&std::env::temp_dir(), program, args, PROBE_TIMEOUT)
}

/// The most output a probe is read for; every parser looks at one short line.
const MAX_OUTPUT: u64 = 16 * 1024;

/// Where a probe's stdout is captured, unique per probe so concurrent probes never share a file.
fn capture_path(dir: &std::path::Path) -> std::path::PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    dir.join(format!("{CAPTURE_PREFIX}{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

const CAPTURE_PREFIX: &str = "omuzaki-probe-";

/// Runs the tool with stdout going to a private file instead of a pipe, so waiting is just `try_wait` under one
/// deadline: no reader thread to abandon (a descendant that keeps stdout open after the tool exits holds a file, not a
/// thread), no thread creation to fail, and a chatty tool cannot stall on a full pipe.
///
/// Storage and writer lifetime are bounded too. The wait loop checks the capture's size every 20 ms and kills the
/// tool's whole process tree once it passes [`MAX_OUTPUT`], so a runaway writer costs at most 20 ms of output. The
/// tree is killed on every path (success, failure, timeout, oversize), so no descendant outlives the probe to keep
/// writing, and the file is removed.
fn run_within(dir: &std::path::Path, program: &str, args: &[&str], limit: Duration) -> Option<String> {
    let path = capture_path(dir);
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).ok()?;
    let result = run_captured(program, args, limit, file, &path);
    // After the tree is dead nothing holds the file; if Windows still refuses the delete, the OS temp cleaner gets it.
    let _ = std::fs::remove_file(&path);
    result
}

fn run_captured(
    program: &str,
    args: &[&str],
    limit: Duration,
    file: std::fs::File,
    path: &std::path::Path,
) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::from(file)).stderr(Stdio::null());
    // Its own process group on Unix, so the whole tree can be signalled at once.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + limit;
    let outcome = loop {
        let oversize = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_OUTPUT);
        match child.try_wait() {
            Ok(Some(status)) if status.success() && !oversize => break read_capture(path),
            Ok(None) if !oversize && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            // A failed exit, a timeout, an oversize capture or a wait error: all read as unknown.
            _ => break None,
        }
    };
    let clean = kill_tree(&mut child);
    outcome.filter(|_| clean)
}

fn read_capture(path: &std::path::Path) -> Option<String> {
    let mut out = String::new();
    std::fs::File::open(path).ok()?.take(MAX_OUTPUT).read_to_string(&mut out).ok()?;
    Some(out)
}

/// Kills the tool and everything it started, then reaps it. Returns `false` when a descendant could not be signalled,
/// which makes the probe read as unknown.
///
/// Unix: the probe leads its own process group, so one `SIGKILL` to the group (a direct syscall: it cannot hang, and
/// its error comes back) ends every descendant, including a daemon a tool autolaunched (gsettings may start dbus).
/// `ESRCH`, the group already gone, is the goal reached. Windows reads the setting without a process; a tool run
/// through [`run_probe`] there is ended with `TerminateProcess`.
fn kill_tree(child: &mut std::process::Child) -> bool {
    #[cfg(unix)]
    let signalled = match rustix::process::kill_process_group(
        rustix::process::Pid::from_child(child),
        rustix::process::Signal::KILL,
    ) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => true,
        Err(e) => {
            tracing::warn!("reduce-motion probe: could not kill the process group: {e}");
            false
        }
    };
    #[cfg(not(unix))]
    let signalled = true;
    let _ = child.kill();
    let _ = child.wait();
    signalled
}

/// Windows' "Animation effects": off means reduce. A failed call reads as unknown.
#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    reason = "one Win32 query that writes a BOOL into a local it is handed; nothing outlives the call"
)]
fn probe_platform() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut animate: i32 = 0;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL (an i32) through the pointer, which points at `animate`, alive
    // and writable for the whole call; the other arguments are unused for this action and zero.
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animate).cast(), 0) };
    (ok != 0).then_some(animate == 0)
}

#[cfg(target_os = "macos")]
fn probe_platform() -> Option<bool> {
    run("defaults", &["read", "com.apple.universalaccess", "reduceMotion"]).as_deref().and_then(parse_macos_defaults)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn probe_platform() -> Option<bool> {
    let portal = run(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.Read",
            "org.freedesktop.appearance",
            "reduced-motion",
        ],
    )
    .as_deref()
    .and_then(parse_portal_reduced_motion);
    portal.or_else(|| {
        run("gsettings", &["get", "org.gnome.desktop.interface", "enable-animations"])
            .as_deref()
            .and_then(parse_gnome_enable_animations)
    })
}

/// Runs a probe command under the timeout, capturing its output in `dir`; public so the tests can prove a hung tool
/// is cut off and that nothing is left in `dir`.
pub fn run_probe(dir: &std::path::Path, program: &str, args: &[&str], limit: Duration) -> Option<String> {
    run_within(dir, program, args, limit)
}

/// `defaults read com.apple.universalaccess reduceMotion`: `1` or `0`.
pub fn parse_macos_defaults(output: &str) -> Option<bool> {
    match output.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// `gdbus call ... Settings.Read org.freedesktop.appearance reduced-motion`: `(<<uint32 1>>,)`, 1 = reduce.
pub fn parse_portal_reduced_motion(output: &str) -> Option<bool> {
    let n = output.split("uint32").nth(1)?.trim_start().chars().next()?;
    match n {
        '1' => Some(true),
        '0' => Some(false),
        _ => None,
    }
}

/// `gsettings get org.gnome.desktop.interface enable-animations`: `true` means animations on.
pub fn parse_gnome_enable_animations(output: &str) -> Option<bool> {
    match output.trim() {
        "true" => Some(false),
        "false" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(n: usize) -> (Sender<()>, Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        for _ in 0..n {
            tx.send(()).ok();
        }
        (tx, rx)
    }

    #[test]
    fn a_burst_of_refreshes_costs_one_probe() {
        let (tx, rx) = burst(10);
        drop(tx);
        let (stop, state, calls) = (AtomicBool::new(false), AtomicU8::new(UNKNOWN), AtomicUsize::new(0));
        watch(&rx, &stop, &state, Duration::from_secs(30), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(true)
        });
        // The startup probe, then one for the whole burst, then the closed channel ends the thread.
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(state.load(Ordering::Relaxed), MOTION_REDUCED);
    }

    #[test]
    fn dropping_during_a_refresh_burst_launches_no_more_probes() {
        let (_tx, rx) = burst(10);
        let (stop, state, calls) = (AtomicBool::new(false), AtomicU8::new(UNKNOWN), AtomicUsize::new(0));
        watch(&rx, &stop, &state, Duration::from_secs(30), || {
            calls.fetch_add(1, Ordering::Relaxed);
            stop.store(true, Ordering::Relaxed); // the watcher is dropped while this probe runs
            None
        });
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_stopped_watcher_never_probes() {
        let (_tx, rx) = burst(3);
        let (stop, state, calls) = (AtomicBool::new(true), AtomicU8::new(UNKNOWN), AtomicUsize::new(0));
        watch(&rx, &stop, &state, Duration::from_secs(30), || {
            calls.fetch_add(1, Ordering::Relaxed);
            None
        });
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
