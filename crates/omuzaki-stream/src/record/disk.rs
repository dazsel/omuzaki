//! The disk guard (`docs/design/record.md` D6; #850): a take never fills the disk. It refuses to start, and stops
//! cleanly with its files readable, when the free space above `record.min_free_mb` would last under ten seconds at the
//! format's rate, and warns below ten minutes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::glam::UVec2;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::settings::RecordFormat;

/// Seconds of space left below which a take warns.
pub const WARN_SECONDS: f64 = 600.0;
/// Seconds of space left below which a take refuses to start or stops.
pub const STOP_SECONDS: f64 = 10.0;
/// The default free space kept, in MiB.
pub const DEFAULT_MIN_FREE_MB: u64 = 2048;

/// How much free space a take keeps, and when it warns and stops.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiskGuard {
    /// Bytes always left free.
    pub min_free: u64,
    /// Below this many seconds of space left, warn.
    pub warn_seconds: f64,
    /// Below this many seconds of space left, stop.
    pub stop_seconds: f64,
}

impl Default for DiskGuard {
    fn default() -> Self {
        Self { min_free: DEFAULT_MIN_FREE_MB << 20, warn_seconds: WARN_SECONDS, stop_seconds: STOP_SECONDS }
    }
}

/// What the guard makes of the free space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DiskState {
    /// Plenty of room.
    Ok {
        /// Seconds of space left at the rate.
        seconds_left: f64,
    },
    /// Room for less than the warning time.
    Low {
        /// Seconds of space left at the rate.
        seconds_left: f64,
    },
    /// Room for less than the stop time: don't start, or stop now.
    Stop,
}

impl DiskGuard {
    /// Judges `free_bytes` at `bytes_per_second`: the seconds left are the free space above `min_free` over the rate.
    #[allow(clippy::cast_precision_loss)] // Bytes and rates far under 2^52.
    pub fn judge(&self, free_bytes: u64, bytes_per_second: u64) -> DiskState {
        let room = free_bytes.saturating_sub(self.min_free);
        let seconds_left = if bytes_per_second == 0 { f64::INFINITY } else { room as f64 / bytes_per_second as f64 };
        if room == 0 || seconds_left < self.stop_seconds {
            DiskState::Stop
        } else if seconds_left < self.warn_seconds {
            DiskState::Low { seconds_left }
        } else {
            DiskState::Ok { seconds_left }
        }
    }
}

/// The planning rate of `format` at `size` and `fps`, in bytes per second: a table at 1080p30 scaled by pixels times
/// frame rate. Estimates until the hardware measurement run measures them.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
pub fn estimate_rate(format: RecordFormat, size: UVec2, fps: (u32, u32)) -> u64 {
    let at_1080p30: f64 = match format {
        // About 1.5 MB a frame (estimate).
        RecordFormat::PngSequence => 45_000_000.0,
        // 16 Mbit/s of colour plus the alpha stream, about 25% more (estimate), plus a mono 48 kHz WAV.
        _ => 2_500_000.0 + 192_000.0,
    };
    let scale =
        f64::from(size.x) * f64::from(size.y) * f64::from(fps.0) / f64::from(fps.1.max(1)) / (1920.0 * 1080.0 * 30.0);
    (at_1080p30 * scale).ceil().max(1.0) as u64
}

/// Where the guard reads free space from: the real disk, or a script in tests.
pub type FreeSpace = Arc<dyn Fn(&Path) -> std::io::Result<u64> + Send + Sync>;

/// The free space an ordinary process may use on the disk holding `path`.
///
/// # Errors
/// The OS error when the disk can't be asked.
#[cfg(unix)]
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    // `f_bavail`, not `f_bfree`, which counts the superuser's reserve.
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// The free space an ordinary process may use on the disk holding `path`.
///
/// # Errors
/// The OS error when the disk can't be asked.
#[cfg(windows)]
#[allow(unsafe_code)] // The one FFI call: a local out value and a NUL-terminated wide string.
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0_u64;
    // SAFETY: `wide` ends in NUL and outlives the call; `available` is a valid out pointer; the two optional out
    // pointers are null, which the API allows.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::from_mut(&mut available),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { Err(std::io::Error::last_os_error()) } else { Ok(available) }
}

/// The real disk, as a [`FreeSpace`].
pub fn real() -> FreeSpace {
    Arc::new(free_space)
}

/// How often a take asks the disk for its free space.
pub const CHECK_EVERY: Duration = Duration::from_secs(1);

/// The guard a take runs with, and where it reads free space.
#[derive(Clone)]
pub struct GuardPlan {
    /// The limits.
    pub guard: DiskGuard,
    /// Where free space comes from.
    pub free: FreeSpace,
    /// How often a running take asks ([`CHECK_EVERY`]).
    pub every: Duration,
}

impl GuardPlan {
    /// The real disk with `guard`, asked every [`CHECK_EVERY`].
    pub fn real(guard: DiskGuard) -> Self {
        Self { guard, free: real(), every: CHECK_EVERY }
    }
}

impl GuardPlan {
    /// Judges the disk holding `dir` at `bytes_per_second`; `None` when the disk can't be asked, which never stops a
    /// take (the status then shows no disk state).
    pub fn check(&self, dir: &Path, bytes_per_second: u64) -> Option<DiskState> {
        (self.free)(dir).ok().map(|free| self.guard.judge(free, bytes_per_second))
    }
}

/// The disk state of the take in progress, written by the recorder and read by `record.status`.
pub type DiskSlot = Arc<std::sync::Mutex<Option<DiskState>>>;

impl std::fmt::Debug for GuardPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardPlan").field("guard", &self.guard).field("every", &self.every).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn seconds(state: DiskState) -> Option<f64> {
        match state {
            DiskState::Ok { seconds_left } | DiskState::Low { seconds_left } => Some(seconds_left),
            DiskState::Stop => None,
        }
    }

    #[test]
    fn the_guard_judges_the_room_above_the_minimum() {
        let guard = DiskGuard { min_free: 2 * GIB, warn_seconds: 600.0, stop_seconds: 10.0 };
        let rate = 33_000_000;
        let ok = guard.judge(100 * GIB, rate);
        assert!(matches!(ok, DiskState::Ok { .. }) && (seconds(ok).unwrap_or(0.0) - 3188.0).abs() < 1.0, "{ok:?}");
        let low = guard.judge(20 * GIB, rate);
        assert!(matches!(low, DiskState::Low { .. }) && (seconds(low).unwrap_or(0.0) - 585.6).abs() < 1.0, "{low:?}");
        assert_eq!(guard.judge(2 * GIB + GIB / 5, rate), DiskState::Stop);
        assert_eq!(guard.judge(GIB, rate), DiskState::Stop);
    }

    #[test]
    fn the_rate_scales_with_pixels_and_frames() {
        let full = estimate_rate(RecordFormat::WebmVp9Alpha, UVec2::new(1920, 1080), (30, 1));
        let half = estimate_rate(RecordFormat::WebmVp9Alpha, UVec2::new(960, 1080), (30, 1));
        assert_eq!(full, 2_692_000);
        assert_eq!(half, full / 2);
        assert_eq!(estimate_rate(RecordFormat::PngSequence, UVec2::new(1920, 1080), (60, 1)), 90_000_000);
    }

    #[test]
    fn this_disk_answers() {
        assert!(free_space(&std::env::temp_dir()).is_ok_and(|b| b > 0));
    }
}
