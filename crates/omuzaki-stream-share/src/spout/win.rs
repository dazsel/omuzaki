//! The Windows half of the Spout sender: the named kernel objects and the D3D11 texture (see the module above).

// Spout is shared kernel objects and a D3D11 texture: file mappings read and written through raw views, named mutexes
// and a semaphore, and COM calls. Every unsafe block says what it relies on.
#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::error::StreamError;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HMODULE, INVALID_HANDLE_VALUE, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_QUERY_DESC,
    D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIResource};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateSemaphoreW, ReleaseMutex, ReleaseSemaphore, WaitForSingleObject,
};
use windows::core::{HSTRING, Interface};

use super::{ACTIVE_SENDER, FORMAT_BGRA8, INFO_BYTES, MAX_SENDERS, NAME_BYTES, SENDER_NAMES};
use crate::share::Publisher;

/// How long a write waits for a mapping's mutex before giving up.
const MAP_TIMEOUT_MS: u32 = 1000;
/// How long a frame waits for the access mutex (a receiver mid-read) before it is skipped: Spout's own figure.
const ACCESS_TIMEOUT_MS: u32 = 67;
/// How long a frame waits for its GPU copy before the device counts as lost.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

fn win(what: &str) -> impl FnOnce(windows::core::Error) -> StreamError + '_ {
    move |e| StreamError::Io(format!("Spout: {what}: {e}"))
}

/// A kernel handle, closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a Create/Open call and is closed once, here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A named mutex.
struct NamedMutex(Handle);

impl NamedMutex {
    fn create(name: &str) -> Result<Self, StreamError> {
        // SAFETY: default security, not initially owned, a valid name.
        let handle = unsafe { CreateMutexW(None, false, &HSTRING::from(name)) }.map_err(win("CreateMutex"))?;
        Ok(Self(Handle(handle)))
    }

    /// Takes the mutex, or `None` after `ms` milliseconds. A mutex its last owner abandoned (it crashed holding it)
    /// is taken too: what it guards is rewritten whole by every writer.
    fn lock(&self, ms: u32) -> Option<MutexGuard<'_>> {
        // SAFETY: a live mutex handle.
        let waited = unsafe { WaitForSingleObject(self.0.0, ms) };
        (waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED).then_some(MutexGuard(self))
    }
}

struct MutexGuard<'a>(&'a NamedMutex);

impl Drop for MutexGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex (the guard exists only after a successful wait).
        let _ = unsafe { ReleaseMutex(self.0.0.0) };
    }
}

/// A named file mapping with a read-write view of all of it, and the mutex Spout pairs with it (`<name>_mutex`).
struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    lock: NamedMutex,
    _handle: Handle,
}

impl Mapping {
    /// Creates the mapping, or opens it if it exists (then at the size its creator chose). Returns whether it existed.
    fn create(name: &str, size: usize) -> Result<(Self, bool), StreamError> {
        let size32 = u32::try_from(size).map_err(|_| StreamError::Config(format!("a {size}-byte mapping")))?;
        // SAFETY: a pagefile-backed mapping (INVALID_HANDLE_VALUE), default security, a valid name; GetLastError is
        // read right after the call that set it.
        let (handle, existed) = unsafe {
            let handle =
                CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, size32, &HSTRING::from(name))
                    .map_err(win("CreateFileMapping"))?;
            (Handle(handle), GetLastError() == ERROR_ALREADY_EXISTS)
        };
        // SAFETY: maps the whole of a live mapping read-write; VirtualQuery reads the view's region into a local.
        let (view, len) = unsafe {
            let view = MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if view.Value.is_null() {
                return Err(StreamError::Io(format!("Spout: the {name} mapping could not be mapped")));
            }
            let mut region = MEMORY_BASIC_INFORMATION::default();
            let filled =
                VirtualQuery(Some(view.Value.cast_const()), &raw mut region, size_of::<MEMORY_BASIC_INFORMATION>());
            // The region is the mapping rounded up to a page; an existing mapping may be smaller than `size`.
            (view, if filled == 0 { size } else { region.RegionSize })
        };
        let lock = NamedMutex::create(&format!("{name}_mutex"))?;
        Ok((Self { view, len, lock, _handle: handle }, existed))
    }

    /// Whether a mapping called `name` exists: a live sender holds its info block open.
    fn exists(name: &str) -> bool {
        // SAFETY: opens read-only and closes at once.
        unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(name)) }.is_ok_and(|h| {
            drop(Handle(h));
            true
        })
    }

    /// Runs `f` on the mapping's bytes (at most `cap`) with its mutex held.
    fn with<T>(&self, cap: usize, f: impl FnOnce(&mut [u8]) -> T) -> Result<T, StreamError> {
        let _guard = self
            .lock
            .lock(MAP_TIMEOUT_MS)
            .ok_or_else(|| StreamError::Io("Spout: another app holds Spout's shared memory; try again".into()))?;
        // SAFETY: the view is `len` bytes of a live read-write mapping; the mutex every Spout program takes before
        // writing it is held, and nothing else in this process aliases it.
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.view.Value.cast::<u8>(), self.len.min(cap)) };
        Ok(f(bytes))
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the view came from MapViewOfFile and is unmapped once; the handle closes after (field order).
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}

/// A running Spout sender. Dropping it withdraws it: its name leaves the list, and its objects close.
pub(super) struct Sender {
    name: String,
    size: UVec2,
    context: ID3D11DeviceContext,
    upload: ID3D11Texture2D,
    shared: ID3D11Texture2D,
    copied: ID3D11Query,
    access: NamedMutex,
    count: Handle,
    active: Mapping,
    names: Mapping,
    _info: Mapping,
    _device: ID3D11Device,
}

fn texture(device: &ID3D11Device, size: UVec2, shared: bool) -> Result<ID3D11Texture2D, StreamError> {
    let mut bind = D3D11_BIND_SHADER_RESOURCE.0;
    let mut misc = 0;
    if shared {
        bind |= D3D11_BIND_RENDER_TARGET.0;
        misc = D3D11_RESOURCE_MISC_SHARED.0;
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.x,
        Height: size.y,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT(FORMAT_BGRA8.cast_signed()),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.cast_unsigned(),
        CPUAccessFlags: 0,
        MiscFlags: misc.cast_unsigned(),
    };
    let mut made: Option<ID3D11Texture2D> = None;
    // SAFETY: a valid descriptor, no initial data, an out-pointer to a live local.
    unsafe { device.CreateTexture2D(&raw const desc, None, Some(&raw mut made)) }.map_err(win("CreateTexture2D"))?;
    made.ok_or_else(|| StreamError::Io("Spout: no texture".into()))
}

impl Sender {
    pub(super) fn open(name: &str, size: UVec2) -> Result<Self, StreamError> {
        let (mut device, mut context): (Option<ID3D11Device>, Option<ID3D11DeviceContext>) = (None, None);
        // SAFETY: the default adapter's hardware driver, BGRA support (Spout's format), out-pointers to live locals.
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
        }
        .map_err(|e| StreamError::Unavailable {
            what: "Direct3D 11 for Spout".into(),
            hint: format!("update the graphics driver ({e})"),
        })?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(StreamError::Io("Spout: Direct3D 11 made no device".into()));
        };
        let upload = texture(&device, size, false)?;
        let shared = texture(&device, size, true)?;
        // SAFETY: the texture was created shared; GetSharedHandle returns its legacy (not NT) handle.
        let share = unsafe { shared.cast::<IDXGIResource>().and_then(|r| r.GetSharedHandle()) }
            .map_err(win("GetSharedHandle"))?;
        // A legacy shared handle is 32 bits by design (Spout stores it so, and 32-bit receivers open it).
        let share =
            u32::try_from(share.0.addr()).map_err(|_| StreamError::Io("Spout: a share handle over 32 bits".into()))?;
        let mut copied: Option<ID3D11Query> = None;
        let desc = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
        // SAFETY: a valid descriptor and an out-pointer to a live local.
        unsafe { device.CreateQuery(&raw const desc, Some(&raw mut copied)) }.map_err(win("CreateQuery"))?;
        let copied = copied.ok_or_else(|| StreamError::Io("Spout: no query".into()))?;

        // The list first: a name listed whose info block is gone is a sender that crashed, and is reused; one whose
        // block is open is another sender's, and refused.
        let (names, _) = Mapping::create(SENDER_NAMES, MAX_SENDERS * NAME_BYTES)?;
        let taken = names.with(MAX_SENDERS * NAME_BYTES, |list| super::read_names(list))?;
        if taken.iter().any(|n| n == name) && Mapping::exists(name) {
            return Err(taken_error(name));
        }
        let (info, existed) = Mapping::create(name, INFO_BYTES)?;
        if existed {
            return Err(taken_error(name));
        }
        let host = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        info.with(INFO_BYTES, |bytes| bytes.copy_from_slice(&super::info_block(share, size, &host)))?;
        let access = NamedMutex::create(&format!("{name}_SpoutAccessMutex"))?;
        // SAFETY: default security, a count from 1 up to i32::MAX as Spout makes it, a valid name.
        let count = unsafe { CreateSemaphoreW(None, 1, i32::MAX, &HSTRING::from(format!("{name}_Count_Semaphore"))) }
            .map_err(win("CreateSemaphore"))?;
        names.with(MAX_SENDERS * NAME_BYTES, |list| {
            let mut all = super::read_names(list);
            if !all.iter().any(|n| n == name) {
                all.push(name.to_owned());
            }
            super::write_names(list, &all)
        })??;
        // The newest sender is the active one, as with Spout's own senders: a receiver given no name picks it.
        let (active, _) = Mapping::create(ACTIVE_SENDER, NAME_BYTES)?;
        active.with(NAME_BYTES, |slot| super::write_active(slot, name))?;
        Ok(Self {
            name: name.to_owned(),
            size,
            context,
            upload,
            shared,
            copied,
            access,
            count: Handle(count),
            active,
            names,
            _info: info,
            _device: device,
        })
    }
}

fn taken_error(name: &str) -> StreamError {
    StreamError::Config(format!("another app already sends to Spout as {name:?}; pick another name"))
}

impl Publisher for Sender {
    fn publish(&mut self, bgra: &[u8]) -> Result<(), StreamError> {
        // Upload into the private texture while receivers read the shared one; only the GPU copy below touches it.
        // SAFETY: `bgra` holds `size.y` rows of `size.x * 4` bytes (the share module sizes it from the config), and
        // the upload texture is that size.
        unsafe { self.context.UpdateSubresource(&self.upload, 0, None, bgra.as_ptr().cast(), self.size.x * 4, 0) };
        let Some(guard) = self.access.lock(ACCESS_TIMEOUT_MS) else {
            // A receiver is holding the texture: skip this frame rather than hold up the next.
            return Ok(());
        };
        // SAFETY: both textures belong to this context's device and have the same size and format.
        unsafe {
            self.context.CopyResource(&self.shared, &self.upload);
            self.context.End(&self.copied);
            self.context.Flush();
        }
        let started = Instant::now();
        loop {
            let mut done = 0i32;
            // SAFETY: an event query's data is a 4-byte BOOL, written only once the GPU has passed the copy.
            unsafe { self.context.GetData(&self.copied, Some((&raw mut done).cast()), 4, 0) }
                .map_err(win("GetData"))?;
            if done != 0 {
                break;
            }
            if started.elapsed() > COPY_TIMEOUT {
                return Err(StreamError::Io("Spout: the GPU stopped answering (device lost)".into()));
            }
            std::thread::yield_now();
        }
        drop(guard);
        // SAFETY: a live semaphore; a count at its maximum (years of frames) just fails, which changes nothing.
        let _ = unsafe { ReleaseSemaphore(self.count.0, 1, None) };
        Ok(())
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let name = self.name.clone();
        let mut rest = Vec::new();
        let _ = self.names.with(MAX_SENDERS * NAME_BYTES, |list| {
            rest = super::read_names(list);
            rest.retain(|n| *n != name);
            super::write_names(list, &rest)
        });
        let _ = self.active.with(NAME_BYTES, |slot| {
            if super::read_names(slot).first() == Some(&name) {
                // Hand the active role to the next sender still listed, as Spout does, or leave it empty.
                super::write_active(slot, rest.first().map_or("", String::as_str));
            }
        });
    }
}
