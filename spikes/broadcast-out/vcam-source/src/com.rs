//! Small shared pieces of the COM server: the live-object count that `DllCanUnloadNow` reports, and the error codes the
//! Frame Server documentation asks for.

#![allow(unsafe_code)] // only for the Send/Sync promise on `Agile`

use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::ERROR_SET_NOT_FOUND;
use windows::core::{Error, HRESULT};

/// COM objects and class-factory locks alive in this DLL.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// A COM interface pointer that may cross threads.
///
/// windows-rs does not mark interface types `Send` or `Sync`, because in general a COM object may be apartment-bound. The
/// objects held this way are Media Foundation's event queues, stream descriptors, the source (this DLL's own free-threaded
/// object) and the opaque request tokens a consumer passes in: Media Foundation objects are free-threaded and thread-safe
/// by contract, and we only ever clone, release or hand them back, never call apartment-specific methods on them.
pub struct Agile<T>(pub T);

// SAFETY: see the type's documentation.
unsafe impl<T> Send for Agile<T> {}
// SAFETY: see the type's documentation.
unsafe impl<T> Sync for Agile<T> {}

/// Counts one more live object until the returned guard drops. Held inside every COM object so `DllCanUnloadNow` is true
/// only when none remain.
pub struct LiveGuard;

impl LiveGuard {
    pub fn new() -> Self {
        LIVE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether the DLL may be unloaded.
pub fn unloadable() -> bool {
    LIVE.load(Ordering::SeqCst) == 0
}

/// Adjusts the count for `IClassFactory::LockServer`.
pub fn lock_server(lock: bool) {
    if lock {
        LIVE.fetch_add(1, Ordering::SeqCst);
    } else {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a camera source returns for a control it does not implement: the code the AV Stream framework gives when a
/// miniport registers no handler (`HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND)`).
pub fn set_not_found() -> Error {
    Error::from(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0))
}

/// Packs two 32-bit values the way Media Foundation stores sizes and ratios in a `UINT64` attribute.
pub fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}
