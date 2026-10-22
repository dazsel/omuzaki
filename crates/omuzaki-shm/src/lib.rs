//! The live show's shared-memory channels between the output host and its workers (`docs/design/supervisor.md` §4.3,
//! §15.3).
//!
//! | Type | Producer → consumer | Semantics |
//! |---|---|---|
//! | [`FrameSlots`] | stage → host; host → encoder | slots owned by one side at a time; ownership moves with the protocol's `Frame` and `SlotFree` messages |
//! | [`Latest`] | tracker → stage; audio → stage; studio → stage | newest wins: a seqlock record whose reads are bounded and never spin |
//! | [`SampleRing`] | audio → host | runs of interleaved samples with their positions, gaps and withheld flags, and the clock mapping |
//! | [`PacketChannel`] | encoder → host | a byte ring of encoded packets, side data included; only complete records are read |
//!
//! **No object outlives the show.** Every object is anonymous (a memfd on Linux; on macOS a random name unlinked at
//! once; on Windows a pagefile section, freed with its last handle) and travels as a [`Share`]: a descriptor sent over
//! the control socket on Unix, a section name on Windows. Every worker generation gets fresh objects, so a producer
//! killed mid-write never corrupts its successor's.
//!
//! **The other process is not trusted.** Each type keeps its geometry in its own fields; every length and index read
//! from shared memory is checked before it is used, and a value out of bounds is an error, never a fault.

mod latest;
mod packets;
mod region;
mod ring;
mod slots;

pub use latest::{LATEST_READ_TRIES, Latest};
pub use packets::PacketChannel;
pub use ring::{ClockMapping, RunInfo, SampleRing};
pub use slots::FrameSlots;

/// What went wrong with a shared object.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("shared memory: {what}: {detail}")]
pub struct ShmError {
    /// The step that failed.
    pub what: &'static str,
    /// What the OS or the check said.
    pub detail: String,
}

impl ShmError {
    pub(crate) fn new(what: &'static str, detail: impl Into<String>) -> Self {
        Self { what, detail: detail.into() }
    }
}

impl From<ShmError> for omuzaki_stream_api::error::StreamError {
    fn from(e: ShmError) -> Self {
        Self::Io(e.to_string())
    }
}

/// One end of a shared object as it is handed to another process: the descriptor on Unix (sent over the control
/// socket with `SCM_RIGHTS`), the section name on Windows. Made by `create`, consumed by `open`.
#[derive(Debug)]
pub enum Share {
    /// The object's descriptor.
    #[cfg(unix)]
    Fd(std::os::fd::OwnedFd),
    /// The section's name.
    #[cfg(windows)]
    Name(String),
}

impl Share {
    /// The handle the host keeps for the object (and sends a clone of).
    #[cfg(unix)]
    pub fn into_handle(self) -> omuzaki_stream_api::worker::ChannelHandle {
        let Self::Fd(fd) = self;
        omuzaki_stream_api::worker::ChannelHandle::from_fd(fd)
    }
}

/// The OS monotonic clock in nanoseconds, the same in every process of the show: `CLOCK_MONOTONIC` on Unix,
/// `QueryPerformanceCounter` on Windows. The only time base that crosses a process boundary.
pub fn host_now() -> u64 {
    #[cfg(unix)]
    {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let secs = u64::try_from(t.tv_sec).unwrap_or(0);
        let nanos = u64::try_from(t.tv_nsec).unwrap_or(0);
        secs.saturating_mul(1_000_000_000).saturating_add(nanos)
    }
    #[cfg(windows)]
    {
        windows_now()
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "the performance counter has no safe binding")]
fn windows_now() -> u64 {
    use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let (mut count, mut freq) = (0_i64, 0_i64);
    // SAFETY: both write one i64 through a valid pointer; they cannot fail on Windows XP and later.
    unsafe {
        QueryPerformanceCounter(&raw mut count);
        QueryPerformanceFrequency(&raw mut freq);
    }
    let (count, freq) = (u128::from(count.unsigned_abs()), u128::from(freq.unsigned_abs().max(1)));
    u64::try_from(count * 1_000_000_000 / freq).unwrap_or(u64::MAX)
}

/// Header fields every object starts with: a magic naming its type and the layout version.
pub(crate) mod header {
    use std::sync::atomic::Ordering;

    use crate::ShmError;
    use crate::region::Region;

    /// The first word: `OMZS`, the type's tag, and the layout version.
    pub(crate) fn tag(kind: u8) -> u64 {
        u64::from_le_bytes([b'O', b'M', b'Z', b'S', kind, 1, 0, 0])
    }

    /// The size of the common header; type headers follow at word offsets after it, and data starts at
    /// [`DATA_ALIGN`]-aligned offsets.
    pub(crate) const WORD: usize = 8;

    /// Where data starts aligning to (a cache line).
    pub(crate) const DATA_ALIGN: usize = 64;

    pub(crate) fn align(n: usize) -> Option<usize> {
        n.checked_add(DATA_ALIGN - 1).map(|n| n / DATA_ALIGN * DATA_ALIGN)
    }

    /// Writes the tag and the geometry words (each at `WORD * (1 + i)`).
    pub(crate) fn write(region: &Region, kind: u8, geometry: &[u64]) -> Result<(), ShmError> {
        for (i, value) in geometry.iter().enumerate() {
            word(region, 1 + i)?.store(*value, Ordering::Relaxed);
        }
        word(region, 0)?.store(tag(kind), Ordering::Release);
        Ok(())
    }

    /// Checks the tag and reads `n` geometry words, as the opening end does once.
    pub(crate) fn read(region: &Region, kind: u8, n: usize) -> Result<Vec<u64>, ShmError> {
        if word(region, 0)?.load(Ordering::Acquire) != tag(kind) {
            return Err(ShmError::new("open", "the object is not the expected kind"));
        }
        (0..n).map(|i| word(region, 1 + i).map(|w| w.load(Ordering::Relaxed))).collect()
    }

    /// Header word `i`.
    pub(crate) fn word(region: &Region, i: usize) -> Result<&std::sync::atomic::AtomicU64, ShmError> {
        region.atomic(i * WORD).ok_or_else(|| ShmError::new("header", "the object is too small for its header"))
    }

    /// `n` as a `usize`, or an error naming `what`.
    pub(crate) fn size(n: u64, what: &'static str) -> Result<usize, ShmError> {
        usize::try_from(n).map_err(|_| ShmError::new("open", format!("{what} does not fit this machine")))
    }
}
