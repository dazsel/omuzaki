//! The process's CPU time so far, for the probe's CPU-per-frame column: every thread's, so the runtimes' worker pools
//! count. `None` where the platform doesn't say; the probe then prints "not reported".

use std::time::Duration;

/// Total CPU time of every thread of this process so far.
#[cfg(target_os = "linux")]
pub fn process_cpu() -> Option<Duration> {
    // Per-thread on-CPU nanoseconds, the scheduler's own account (finer than `/proc/self/stat`'s clock ticks).
    let mut total = 0u64;
    for task in std::fs::read_dir("/proc/self/task").ok()? {
        let schedstat = std::fs::read_to_string(task.ok()?.path().join("schedstat")).ok()?;
        total = total.checked_add(schedstat.split_whitespace().next()?.parse::<u64>().ok()?)?;
    }
    Some(Duration::from_nanos(total))
}

/// Total CPU time of every thread of this process so far: kernel plus user, from `GetProcessTimes`.
#[cfg(windows)]
#[allow(unsafe_code)] // one Win32 call on the current process's pseudo-handle, into four locals
pub fn process_cpu() -> Option<Duration> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo-handle is always valid for the calling process, and each pointer is a live local.
    unsafe { GetProcessTimes(GetCurrentProcess(), &raw mut created, &raw mut exited, &raw mut kernel, &raw mut user) }
        .ok()?;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME counts 100 ns ticks.
    Some(Duration::from_nanos(ticks(kernel).checked_add(ticks(user))?.checked_mul(100)?))
}

/// Not reported on this platform.
#[cfg(not(any(target_os = "linux", windows)))]
pub fn process_cpu() -> Option<Duration> {
    None
}
