//! The limits a model worker runs under, per OS. The registry passes the memory cap in [`MEMORY_ENV`].
//!
//! | | Linux | macOS | Windows |
//! |---|---|---|---|
//! | memory cap | `RLIMIT_AS`, set by the worker on itself before it reads a request | `RLIMIT_DATA`, likewise (macOS doesn't enforce `RLIMIT_AS`) | a Job Object's per-process memory limit, set by the registry |
//! | lower CPU priority | nice +10, by the worker | nice +10, by the worker | the Job Object's below-normal priority class |
//! | killed with the registry | the worker exits when its parent process changes (checked every [`WATCH_PARENT`]) | the same | the Job Object kills it when the registry's handle closes, as it does when the registry's process ends |
//!
//! An allocation over the cap fails, and the worker aborts: to the registry that is a crash like any other. A limit the
//! OS refuses is reported (by the worker on stderr, by the registry in its log) and the worker carries on; the stall
//! deadline still holds.

use std::process::Child;
#[cfg(unix)]
use std::time::Duration;

/// The variable that carries the memory cap, in MiB, to the worker.
pub(crate) const MEMORY_ENV: &str = "OMUZAKI_MODEL_WORKER_MEMORY_MB";

/// How often a worker on unix checks that the registry's process is still its parent.
#[cfg(unix)]
const WATCH_PARENT: Duration = Duration::from_millis(250);

/// The worker's niceness on unix: below the app's threads, above idle.
#[cfg(unix)]
const NICE: i32 = 10;

/// Applies the worker's own limits, before it reads anything. Called first thing by `serve`.
pub(crate) fn enter_worker() {
    #[cfg(unix)]
    {
        if let Some(mib) = std::env::var(MEMORY_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
            cap_memory(mib.saturating_mul(1024 * 1024));
        }
        if let Err(e) = rustix::process::setpriority_process(None, NICE) {
            eprintln!("omuzaki model worker: couldn't lower its priority: {e}");
        }
        watch_parent();
    }
}

#[cfg(unix)]
fn cap_memory(bytes: u64) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let resource = rustix::process::Resource::As;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let resource = rustix::process::Resource::Data;
    let limit = rustix::process::Rlimit { current: Some(bytes), maximum: Some(bytes) };
    if let Err(e) = rustix::process::setrlimit(resource, limit) {
        eprintln!("omuzaki model worker: couldn't cap its memory: {e}");
    }
}

/// Ends this worker when the process that started it ends (its parent changes to the reaper).
#[cfg(unix)]
fn watch_parent() {
    let parent = std::os::unix::process::parent_id();
    let watcher = std::thread::Builder::new().name("parent watch".into()).spawn(move || {
        loop {
            std::thread::sleep(WATCH_PARENT);
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(1);
            }
        }
    });
    if let Err(e) = watcher {
        eprintln!("omuzaki model worker: couldn't watch its parent: {e}");
    }
}

/// What holds a worker to its limits from the registry's side: on Windows its Job Object, closed (killing the worker)
/// when this is dropped; elsewhere nothing, since the worker limits itself.
pub(crate) struct Contained {
    /// Held only to be dropped with the worker's handle.
    #[cfg(windows)]
    _job: windows::Job,
}

/// Puts `child` under the registry-side limits. A refusal is logged and the worker runs without them; the stall
/// deadline still holds.
pub(crate) fn contain(child: &Child, memory_mib: u64) -> Contained {
    #[cfg(windows)]
    {
        Contained { _job: windows::Job::contain(child, memory_mib) }
    }
    #[cfg(not(windows))]
    {
        let _ = (child, memory_mib);
        Contained {}
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // Job Objects are only reachable through Win32 calls; each block says what it relies on.
mod windows {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PRIORITY_CLASS, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;

    /// An owned Job Object handle; null when it couldn't be made.
    pub(super) struct Job(HANDLE);

    // SAFETY: a Job Object handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub(super) fn contain(child: &Child, memory_mib: u64) -> Self {
            // SAFETY: null attributes and name are allowed and make an unnamed job with default security.
            let job = Job(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
            if job.0.is_null() {
                tracing::warn!("couldn't create a job object for a model worker: {}", std::io::Error::last_os_error());
                return job;
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
            info.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
            info.ProcessMemoryLimit = usize::try_from(memory_mib.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX);
            let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(u32::MAX);
            // SAFETY: `job` is a live job handle, and `info` is the struct this information class reads, `size` long.
            let set = unsafe {
                SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&raw const info).cast(), size)
            };
            // SAFETY: both handles are live: the job's above, the child's for as long as `child` is borrowed.
            let assigned = set != 0 && unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } != 0;
            if !assigned {
                tracing::warn!("couldn't put a model worker under its limits: {}", std::io::Error::last_os_error());
            }
            job
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the handle is ours and closed once. Closing it kills the worker (KILL_ON_JOB_CLOSE).
                unsafe { CloseHandle(self.0) };
            }
        }
    }
}
