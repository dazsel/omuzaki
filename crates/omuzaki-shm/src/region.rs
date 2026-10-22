//! One mapped shared object: made with no name to leak (a memfd on Linux, a name unlinked at once on macOS, a
//! pagefile section on Windows, `docs/design/supervisor.md` §4.3), and the only place this crate uses `unsafe`.
//!
//! Every access goes through bounds-checked offsets. The other process may write any byte of the region at any time,
//! so whatever is read from it (a length, an index) is a claim to check, never a fact; the geometry each type relies
//! on is kept in its own fields, read once when the object is made or opened.
#![allow(unsafe_code, reason = "mapping shared memory and viewing it as atomics has no safe API; every use is here")]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::{Share, ShmError};

/// A mapped shared object.
pub(crate) struct Region {
    ptr: NonNull<u8>,
    len: usize,
    _map: Mapping,
}

// SAFETY: the region is plain shared memory; every access is through atomics or through copies whose exclusivity the
// channel's protocol gives (a slot's owner, a ring's unread span), so it may move between and be shared by threads.
unsafe impl Send for Region {}
// SAFETY: as above.
unsafe impl Sync for Region {}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Region").field("len", &self.len).finish_non_exhaustive()
    }
}

impl Region {
    /// A new zeroed object of `len` bytes, mapped, and the [`Share`] that hands it to the other end.
    pub(crate) fn create(len: usize) -> Result<(Self, Share), ShmError> {
        if len == 0 {
            return Err(ShmError::new("create", "a shared object needs at least one byte"));
        }
        let (map, share) = sys::create(len)?;
        Ok((Self::from_map(map)?, share))
    }

    /// Maps the object `share` hands over.
    pub(crate) fn open(share: Share) -> Result<Self, ShmError> {
        Self::from_map(sys::open(share)?)
    }

    fn from_map(map: Mapping) -> Result<Self, ShmError> {
        let (ptr, len) = map.raw();
        let ptr = NonNull::new(ptr).ok_or_else(|| ShmError::new("map", "the mapping has no address"))?;
        if ptr.as_ptr().align_offset(8) != 0 {
            return Err(ShmError::new("map", "the mapping is not 8-byte aligned"));
        }
        Ok(Self { ptr, len, _map: map })
    }

    /// Its length in bytes.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Whether `len` bytes at `offset` lie inside it.
    fn fits(&self, offset: usize, len: usize) -> bool {
        offset.checked_add(len).is_some_and(|end| end <= self.len)
    }

    /// The `u64` at `offset` as an atomic, or `None` past the end or off its alignment.
    pub(crate) fn atomic(&self, offset: usize) -> Option<&AtomicU64> {
        if !offset.is_multiple_of(8) || !self.fits(offset, 8) {
            return None;
        }
        // SAFETY: in bounds and 8-aligned (the base is 8-aligned, checked in `from_map`); an `AtomicU64` has the size
        // and alignment of a `u64`, and the mapping lives as long as `self`. Another process may change it at any
        // time, which is what an atomic is for.
        #[allow(clippy::cast_ptr_alignment, reason = "the offset and the base are both checked to be 8-aligned")]
        Some(unsafe { &*self.ptr.as_ptr().add(offset).cast::<AtomicU64>() })
    }

    /// The byte at `offset` as an atomic.
    fn atomic_u8(&self, offset: usize) -> Option<&AtomicU8> {
        if !self.fits(offset, 1) {
            return None;
        }
        // SAFETY: in bounds; any address is aligned for a byte; the mapping lives as long as `self`.
        Some(unsafe { &*self.ptr.as_ptr().add(offset).cast::<AtomicU8>() })
    }

    /// Copies `bytes` in at `offset` with relaxed atomic stores: a reader racing the write (a seqlock's) sees some
    /// mix of old and new bytes, never undefined behaviour, and its sequence check discards the mix.
    pub(crate) fn store(&self, offset: usize, bytes: &[u8]) -> Result<(), ShmError> {
        if !self.fits(offset, bytes.len()) {
            return Err(ShmError::new("write", "past the end of the shared object"));
        }
        let mut at = offset;
        let mut rest = bytes;
        while !at.is_multiple_of(8) && !rest.is_empty() {
            self.store_byte(at, rest[0]);
            (at, rest) = (at + 1, &rest[1..]);
        }
        let (words, tail) = rest.as_chunks::<8>();
        for word in words {
            if let Some(cell) = self.atomic(at) {
                cell.store(u64::from_ne_bytes(*word), Ordering::Relaxed);
            }
            at += 8;
        }
        for &byte in tail {
            self.store_byte(at, byte);
            at += 1;
        }
        Ok(())
    }

    /// Copies `out.len()` bytes out from `offset` with relaxed atomic loads (see [`store`](Self::store)).
    pub(crate) fn load(&self, offset: usize, out: &mut [u8]) -> Result<(), ShmError> {
        if !self.fits(offset, out.len()) {
            return Err(ShmError::new("read", "past the end of the shared object"));
        }
        let mut at = offset;
        let mut rest = &mut out[..];
        while !at.is_multiple_of(8) && !rest.is_empty() {
            rest[0] = self.load_byte(at);
            at += 1;
            rest = &mut rest[1..];
        }
        let (words, tail) = rest.as_chunks_mut::<8>();
        for word in words {
            *word = self.atomic(at).map_or(0, |cell| cell.load(Ordering::Relaxed)).to_ne_bytes();
            at += 8;
        }
        for byte in tail {
            *byte = self.load_byte(at);
            at += 1;
        }
        Ok(())
    }

    /// Copies `bytes` in at `offset` with a plain copy, for spans the channel's protocol gives this side alone (a slot
    /// it owns, a ring's free space): bulk data at memory speed.
    pub(crate) fn copy_in(&self, offset: usize, bytes: &[u8]) -> Result<(), ShmError> {
        if !self.fits(offset, bytes.len()) {
            return Err(ShmError::new("write", "past the end of the shared object"));
        }
        // SAFETY: in bounds; the source is a separate allocation (the region is never handed out as a slice). The
        // protocol gives this span to this side alone; a peer that writes it anyway (a bug or a hostile worker) can
        // only make the bytes wrong, which every reader checks, since this side never forms a reference to them.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.as_ptr().add(offset), bytes.len()) };
        Ok(())
    }

    /// Copies `out.len()` bytes out from `offset` with a plain copy, for spans the protocol gives this side alone.
    pub(crate) fn copy_out(&self, offset: usize, out: &mut [u8]) -> Result<(), ShmError> {
        if !self.fits(offset, out.len()) {
            return Err(ShmError::new("read", "past the end of the shared object"));
        }
        // SAFETY: as `copy_in`, in the other direction.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr.as_ptr().add(offset), out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    fn store_byte(&self, offset: usize, byte: u8) {
        if let Some(cell) = self.atomic_u8(offset) {
            cell.store(byte, Ordering::Relaxed);
        }
    }

    fn load_byte(&self, offset: usize) -> u8 {
        self.atomic_u8(offset).map_or(0, |cell| cell.load(Ordering::Relaxed))
    }

    /// The whole region's bytes, read relaxed (the privacy test scans it for a marker).
    #[cfg(feature = "testing")]
    pub(crate) fn snapshot(&self) -> Vec<u8> {
        let mut out = vec![0; self.len];
        // In bounds by construction.
        let _ = self.load(0, &mut out);
        out
    }
}

/// The OS mapping that keeps the region alive.
#[cfg(unix)]
pub(crate) struct Mapping(memmap2::MmapRaw);

#[cfg(unix)]
impl Mapping {
    fn raw(&self) -> (*mut u8, usize) {
        (self.0.as_mut_ptr(), self.0.len())
    }
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::os::fd::OwnedFd;

    use super::Mapping;
    use crate::{Share, ShmError};

    fn os(what: &'static str) -> impl Fn(rustix::io::Errno) -> ShmError {
        move |e| ShmError::new(what, std::io::Error::from(e).to_string())
    }

    /// An anonymous object of `len` bytes.
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    fn anonymous(len: u64) -> Result<OwnedFd, ShmError> {
        use rustix::fs::{MemfdFlags, SealFlags};
        let fd = rustix::fs::memfd_create("omuzaki-shm", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
            .map_err(os("memfd_create"))?;
        rustix::fs::ftruncate(&fd, len).map_err(os("ftruncate"))?;
        // Sealed at its size: a worker that shrank the object would make the host's next access fault.
        rustix::fs::fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL).map_err(os("seal"))?;
        Ok(fd)
    }

    /// An object under a random name, unlinked at once: only a crash between the two calls can leave the name.
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
    fn anonymous(len: u64) -> Result<OwnedFd, ShmError> {
        use rustix::fs::Mode;
        use rustix::shm::OFlags;
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|e| ShmError::new("random name", e.to_string()))?;
        // Short enough for macOS's 31-byte limit on shared-memory names.
        let name: String =
            std::iter::once('/').chain(random.iter().take(12).map(|b| char::from(b'a' + b % 26))).collect();
        let fd =
            rustix::shm::open(name.as_str(), OFlags::CREATE | OFlags::EXCL | OFlags::RDWR, Mode::RUSR | Mode::WUSR)
                .map_err(os("shm_open"))?;
        rustix::shm::unlink(name.as_str()).map_err(os("shm_unlink"))?;
        rustix::fs::ftruncate(&fd, len).map_err(os("ftruncate"))?;
        Ok(fd)
    }

    fn map(fd: &OwnedFd, len: usize) -> Result<Mapping, ShmError> {
        let file = File::from(fd.try_clone().map_err(|e| ShmError::new("dup", e.to_string()))?);
        let map =
            memmap2::MmapOptions::new().len(len).map_raw(&file).map_err(|e| ShmError::new("mmap", e.to_string()))?;
        Ok(Mapping(map))
    }

    pub(super) fn create(len: usize) -> Result<(Mapping, Share), ShmError> {
        let fd = anonymous(len as u64)?;
        let map = map(&fd, len)?;
        Ok((map, Share::Fd(fd)))
    }

    pub(super) fn open(share: Share) -> Result<Mapping, ShmError> {
        let Share::Fd(fd) = share;
        let stat = rustix::fs::fstat(&fd).map_err(os("fstat"))?;
        let len = usize::try_from(stat.st_size).map_err(|_| ShmError::new("open", "the object has no size"))?;
        if len == 0 {
            return Err(ShmError::new("open", "the object is empty"));
        }
        map(&fd, len)
    }
}

/// The OS mapping that keeps the region alive: the view and the section handle.
#[cfg(windows)]
pub(crate) struct Mapping {
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    _section: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl Mapping {
    fn raw(&self) -> (*mut u8, usize) {
        (self.view.Value.cast::<u8>(), self.len)
    }
}

#[cfg(windows)]
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `view` came from `MapViewOfFile` and is unmapped once, here.
        unsafe {
            windows_sys::Win32::System::Memory::UnmapViewOfFile(self.view);
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};

    use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW,
        PAGE_READWRITE, VirtualQuery,
    };

    use super::Mapping;
    use crate::{Share, ShmError};

    fn last(what: &'static str) -> ShmError {
        // SAFETY: reads the calling thread's last error; no preconditions.
        let code = unsafe { GetLastError() };
        ShmError::new(what, format!("Windows error {code}"))
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn view(section: OwnedHandle, len: Option<usize>) -> Result<Mapping, ShmError> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: maps a section handle we own; a zero length maps the whole section.
        let view = unsafe { MapViewOfFile(section.as_raw_handle(), FILE_MAP_ALL_ACCESS, 0, 0, len.unwrap_or(0)) };
        if view.Value.is_null() {
            return Err(last("MapViewOfFile"));
        }
        let len = if let Some(len) = len {
            len
        } else {
            let mut info = MEMORY_BASIC_INFORMATION {
                BaseAddress: std::ptr::null_mut(),
                AllocationBase: std::ptr::null_mut(),
                AllocationProtect: 0,
                PartitionId: 0,
                RegionSize: 0,
                State: 0,
                Protect: 0,
                Type: 0,
            };
            // SAFETY: queries the view just mapped into a correctly sized struct.
            let got = unsafe { VirtualQuery(view.Value, &raw mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
            if got == 0 {
                return Err(last("VirtualQuery"));
            }
            info.RegionSize
        };
        Ok(Mapping { view, len, _section: section })
    }

    pub(super) fn create(len: usize) -> Result<(Mapping, Share), ShmError> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|e| ShmError::new("random name", e.to_string()))?;
        let hex: String = random.iter().fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        });
        let name = format!("Local\\omuzaki-shm-{hex}");
        let size = len as u64;
        // SAFETY: a pagefile-backed section (INVALID_HANDLE_VALUE) with the default security, sized `len`, named by a
        // NUL-terminated wide string that lives across the call.
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                u32::try_from(size >> 32).unwrap_or(u32::MAX),
                u32::try_from(size & 0xFFFF_FFFF).unwrap_or(u32::MAX),
                wide(&name).as_ptr(),
            )
        };
        if raw.is_null() {
            return Err(last("CreateFileMappingW"));
        }
        // SAFETY: a handle CreateFileMappingW just returned, owned from here on.
        let section = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok((view(section, Some(len))?, Share::Name(name)))
    }

    pub(super) fn open(share: Share) -> Result<Mapping, ShmError> {
        let Share::Name(name) = share;
        // SAFETY: opens a section by a NUL-terminated wide name that lives across the call.
        let raw = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide(&name).as_ptr()) };
        if raw.is_null() {
            return Err(last("OpenFileMappingW"));
        }
        // SAFETY: a handle OpenFileMappingW just returned, owned from here on.
        let section = unsafe { OwnedHandle::from_raw_handle(raw) };
        view(section, None)
    }
}

/// No shared memory on this platform.
#[cfg(not(any(unix, windows)))]
pub(crate) struct Mapping;

#[cfg(not(any(unix, windows)))]
impl Mapping {
    fn raw(&self) -> (*mut u8, usize) {
        (std::ptr::null_mut(), 0)
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    use super::Mapping;
    use crate::{Share, ShmError};

    pub(super) fn create(_: usize) -> Result<(Mapping, Share), ShmError> {
        Err(ShmError::new("create", "this platform has no shared memory"))
    }

    pub(super) fn open(share: Share) -> Result<Mapping, ShmError> {
        match share {}
    }
}
