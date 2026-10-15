//! The host half of the CPU lane: owns the output, holds the last complete frame, presents at a steady rate, and runs
//! the kill loop against the worker.
//!
//! The output here is a tick that records what it would show; the real output call (Spout, NDI) is the other spike's
//! subject. What this measures is the host machinery around it: reading frames out of shared memory, holding the last one,
//! and never missing a tick while the worker dies and comes back.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use spout_out::frame_tag;

use crate::Error;
use crate::analysis::{self, Analysis, Present};
use crate::options::HostOptions;
use crate::proc_stats::{self, ProcSample};
use crate::protocol::Msg;
use crate::schedule::KillSchedule;
use crate::shm::{RingPath, SLOTS, SlotFile};
use crate::supervise::{self, OnFrame, Shared, SuperviseConfig};

/// Presents expected in a run, reserved up front so the presenter does not reallocate in its loop (10 minutes at 30 fps).
const PRESENT_CAPACITY: usize = 40_000;

/// A frame the host holds: its pixels, which frame it is, and when it became usable.
struct Held {
    index: u64,
    arrived_ms: f64,
    pixels: Vec<u8>,
}

/// The newest complete frame, the pool of spare buffers, and the per-frame read costs.
#[derive(Default)]
struct Inner {
    newest: Option<Held>,
    spare: Vec<Vec<u8>>,
    ingest_ms: Vec<f64>,
}

/// The shared last-frame store. The reader thread fills it; the presenter takes from it.
struct Store {
    ring: SlotFile,
    size: (u32, u32),
    inner: Mutex<Inner>,
    shared: Arc<Shared>,
}

impl Store {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads the announced slot into a spare buffer, checks the frame is whole and is the one announced, and makes it the
    /// newest. A frame that fails the check is refused and the previous one stays.
    fn ingest(&self, msg: &Msg) -> bool {
        let Msg::Frame { slot, index, .. } = *msg else { return false };
        let started = Instant::now();
        let mut pixels = self.lock().spare.pop().unwrap_or_default();
        pixels.resize(self.size.0 as usize * self.size.1 as usize * 4, 0);
        let whole = self.ring.read(slot, &mut pixels).is_ok()
            && frame_tag::decode(&pixels, self.size.0, self.size.1)
                .is_ok_and(|d| d.header_ok && d.footer_ok && u64::from(d.tag.frame) == index);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let arrived_ms = self.shared.now_ms();
        let mut inner = self.lock();
        inner.ingest_ms.push(ms);
        if !whole {
            inner.spare.push(pixels);
            return false;
        }
        if let Some(old) = inner.newest.replace(Held { index, arrived_ms, pixels }) {
            inner.spare.push(old.pixels);
        }
        true
    }

    /// Makes the newest frame the shown one, if there is one that is not already shown. True when it did.
    fn take_newest(&self, shown: &mut Option<Held>) -> bool {
        let mut inner = self.lock();
        let Some(newest) = inner.newest.take() else { return false };
        if let Some(old) = shown.replace(newest) {
            inner.spare.push(old.pixels);
        }
        true
    }

    fn has_frame(&self) -> bool {
        self.lock().newest.is_some()
    }
}

/// The steady-rate output loop: each tick shows the newest complete frame, or the previous one again when none arrived.
fn present_loop(store: &Store, shared: &Shared, stop: &AtomicBool, period: Duration) -> Vec<Present> {
    let mut presents = Vec::with_capacity(PRESENT_CAPACITY);
    while !store.has_frame() {
        if stop.load(Ordering::Acquire) {
            return presents;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let mut shown: Option<Held> = None;
    let mut next = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let fresh = store.take_newest(&mut shown);
        let now_ms = shared.now_ms();
        if let Some(held) = &shown {
            presents.push(Present { t_ms: now_ms, index: held.index, fresh, age_ms: now_ms - held.arrived_ms });
        }
        next += period;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    presents
}

/// The machine the numbers came from.
#[derive(Debug, Serialize)]
pub struct Machine {
    /// CPU model string.
    pub cpu: String,
    /// Logical cores available to the process.
    pub cores: usize,
    /// Adapter name, driver and device type as wgpu reports them.
    pub adapter: String,
    /// Graphics backend.
    pub backend: &'static str,
}

/// The host's own cost over a phase.
#[derive(Debug, Serialize)]
pub struct HostCost {
    /// Wall-clock seconds of the phase.
    pub wall_s: f64,
    /// Host CPU seconds (user + system).
    pub cpu_s: f64,
    /// Host CPU as a share of one core.
    pub cpu_percent_of_one_core: f64,
    /// CPU seconds used by the workers killed in the phase.
    pub worker_cpu_s: f64,
    /// Resident set at the end of the phase, KiB.
    pub rss_kib: u64,
    /// Resident set high-water mark, KiB.
    pub peak_rss_kib: u64,
    /// Open descriptors at the end of the phase.
    pub fds: usize,
}

fn cost(from: &ProcSample, to: &ProcSample) -> HostCost {
    let wall_s = to.at.duration_since(from.at).as_secs_f64();
    let cpu_s = to.cpu_s - from.cpu_s;
    HostCost {
        wall_s,
        cpu_s,
        cpu_percent_of_one_core: 100.0 * cpu_s / wall_s,
        worker_cpu_s: to.child_cpu_s - from.child_cpu_s,
        rss_kib: to.rss_kib,
        peak_rss_kib: to.peak_rss_kib,
        fds: to.fds,
    }
}

/// Everything one run reports.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The options the run used.
    pub kills: u32,
    /// Seed of the kill schedule.
    pub seed: u64,
    /// Frame size.
    pub size: (u32, u32),
    /// Output rate.
    pub fps: f64,
    /// Where the numbers came from.
    pub machine: Machine,
    /// Descriptors open before the first worker started.
    pub fds_at_start: usize,
    /// Host cost during the undisturbed baseline phase.
    pub host_baseline: Option<HostCost>,
    /// Host cost during the kill phase.
    pub host_kill_phase: HostCost,
    /// The timings.
    pub analysis: Analysis,
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| l.strip_prefix("model name")?.split_once(':').map(|(_, v)| v.trim().to_owned()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Asks a short-lived copy of this program which adapter the worker will get, so the host itself never opens a GPU.
fn probe_adapter() -> Result<String, Error> {
    let exe = std::env::current_exe().map_err(Error::io("finding this executable"))?;
    let out = Command::new(exe).arg("adapter").output().map_err(Error::io("running the adapter probe"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Runs the whole experiment.
///
/// # Errors
/// Fails when a process, the slot ring or `/proc` cannot be used, or a worker never produces a frame.
pub fn run(options: &HostOptions) -> Result<Report, Error> {
    let exe = std::env::current_exe().map_err(Error::io("finding this executable"))?;
    let adapter = probe_adapter()?;
    let frame_bytes = u64::from(options.size.0) * u64::from(options.size.1) * 4;
    let ring_path = PathBuf::from(format!("/dev/shm/omuzaki-wp-{}", std::process::id()));
    let _ring_guard = RingPath(ring_path.clone());
    let shared = Arc::new(Shared::new());
    let store = Arc::new(Store {
        ring: SlotFile::create(&ring_path, frame_bytes)?,
        size: options.size,
        inner: Mutex::new(Inner::default()),
        shared: Arc::clone(&shared),
    });

    let ticks = proc_stats::clock_ticks_per_second();
    let start = proc_stats::sample(ticks)?;
    let boundary: Arc<Mutex<Option<ProcSample>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));

    let spawn = |_gen: u32| {
        let mut command = Command::new(&exe);
        command
            .arg("worker")
            .arg("--shm")
            .arg(&ring_path)
            .arg("--size")
            .arg(format!("{}x{}", options.size.0, options.size.1))
            .arg("--fps")
            .arg(options.fps.to_string())
            .envs(options.worker_env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        command.spawn()
    };
    let on_frame: OnFrame = {
        let store = Arc::clone(&store);
        Arc::new(move |_gen, msg| store.ingest(msg))
    };
    let config = SuperviseConfig {
        kills: options.kills,
        baseline: Duration::from_secs(options.baseline_s),
        schedule: KillSchedule::new(options.seed, options.kill_window_ms.0, options.kill_window_ms.1),
    };

    let period = Duration::from_secs_f64(1.0 / options.fps);
    let presents = thread::scope(|scope| {
        let supervisor = scope.spawn(|| {
            let on_boundary = || {
                if let Ok(sample) = proc_stats::sample(ticks) {
                    *boundary.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sample);
                }
            };
            let result = supervise::run(&shared, config, spawn, &on_frame, on_boundary);
            // The presenter stops whether or not the run succeeded.
            stop.store(true, Ordering::Release);
            result
        });
        let presents = present_loop(&store, &shared, &stop, period);
        supervisor.join().map_err(|_| Error::Panicked).and_then(|result| result).map(|()| presents)
    })?;
    let end = proc_stats::sample(ticks)?;

    let timeline = shared.take();
    let boundary_sample = *boundary.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let ingest_ms = std::mem::take(&mut store.lock().ingest_ms);
    let analysis = analysis::analyse(&timeline, &presents, &ingest_ms, period.as_secs_f64() * 1000.0);
    Ok(Report {
        kills: options.kills,
        seed: options.seed,
        size: options.size,
        fps: options.fps,
        machine: Machine {
            cpu: cpu_model(),
            cores: thread::available_parallelism().map_or(1, usize::from),
            adapter,
            backend: "Vulkan (lavapipe, no GPU)",
        },
        fds_at_start: start.fds,
        host_baseline: boundary_sample.as_ref().map(|b| cost(&start, b)),
        host_kill_phase: cost(boundary_sample.as_ref().unwrap_or(&start), &end),
        analysis,
    })
}

/// How many slots hold frames (for the page).
pub const RING_SLOTS: u32 = SLOTS;
