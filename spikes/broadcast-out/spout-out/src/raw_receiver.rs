//! A receiver that does what OBS's Spout2 source does, with none of the SDK's receiver code: read the sender's
//! `SharedTextureInfo` out of Spout's shared memory, open the legacy share handle on a plain D3D11 device, and read the
//! texture on its own schedule, with no sync event and no access mutex.
//!
//! That makes it the honest test of the tearing risk OBS faces (OBS renders on its own clock and samples whatever the
//! shared surface holds), and an independent check that the sender follows the protocol rather than just the SDK's own
//! conventions.

#![allow(unsafe_code)]

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
};
use windows::core::{HSTRING, Interface};

use crate::clock::Clock;
use crate::receiver::{ReceiveConfig, ReceiveError};
use crate::report::ReceiverReport;
use crate::tally::{Observation, Tally};

/// Spout's `SharedTextureInfo` (SpoutSenderNames.h): 280 bytes in a file mapping named after the sender.
const INFO_BYTES: usize = 280;
/// `DXGI_FORMAT_B8G8R8A8_UNORM`, the only layout the tag decoder and this spike's sender use.
const FORMAT_BGRA8: u32 = 87;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);

fn d3d(what: &'static str) -> impl FnOnce(windows::core::Error) -> ReceiveError {
    move |e| ReceiveError::Direct3D(format!("{what}: {e}"))
}

/// The sender's description as published in shared memory.
struct SharedInfo {
    handle: u32,
    width: u32,
    height: u32,
    format: u32,
}

/// Reads the sender's info block, if its mapping exists.
fn read_info(name: &str) -> Option<SharedInfo> {
    // SAFETY: opens a named mapping read-only, copies 280 bytes out of the view and unmaps and closes everything.
    unsafe {
        let mapping = OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(name)).ok()?;
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, INFO_BYTES);
        let info = (!view.Value.is_null()).then(|| {
            let bytes = std::slice::from_raw_parts(view.Value.cast::<u8>(), INFO_BYTES);
            let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            SharedInfo { handle: word(0), width: word(4), height: word(8), format: word(12) }
        });
        if !view.Value.is_null() {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view.Value });
        }
        let _ = CloseHandle(mapping);
        info
    }
}

/// Waits for the sender's info block to describe a texture.
fn wait_for_sender(name: &str) -> Result<SharedInfo, ReceiveError> {
    let started = Instant::now();
    loop {
        if let Some(info) = read_info(name).filter(|i| i.width > 0 && i.handle != 0) {
            return Ok(info);
        }
        if started.elapsed() > CONNECT_TIMEOUT {
            return Err(ReceiveError::NoSender(name.to_owned(), CONNECT_TIMEOUT));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The receiver's own device, the sender's texture opened on it, and a CPU-readable copy target.
struct SharedTexture {
    context: ID3D11DeviceContext,
    source: ID3D11Resource,
    staging: ID3D11Resource,
    size: (u32, u32),
}

impl SharedTexture {
    fn open(info: &SharedInfo) -> Result<Self, ReceiveError> {
        if info.format != FORMAT_BGRA8 {
            return Err(ReceiveError::Direct3D(format!("sender format {} is not BGRA8", info.format)));
        }
        let (mut device, mut context): (Option<ID3D11Device>, Option<ID3D11DeviceContext>) = (None, None);
        // SAFETY: out-pointers reference live locals; default adapter, hardware driver, no software module.
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
        }
        .map_err(d3d("D3D11CreateDevice"))?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(ReceiveError::Direct3D("no device".to_owned()));
        };

        let mut shared: Option<ID3D11Texture2D> = None;
        // SAFETY: the handle is the sender's legacy shared handle (a 32-bit value the SDK stores in the info block).
        unsafe { device.OpenSharedResource(HANDLE(info.handle as usize as *mut _), &raw mut shared) }
            .map_err(d3d("OpenSharedResource"))?;
        let source: ID3D11Resource = shared
            .ok_or_else(|| ReceiveError::Direct3D("no shared texture".to_owned()))?
            .cast()
            .map_err(d3d("ID3D11Resource"))?;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: info.width,
            Height: info.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT(FORMAT_BGRA8.cast_signed()),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
            ..Default::default()
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: valid descriptor and out-pointer.
        unsafe { device.CreateTexture2D(&raw const desc, None, Some(&raw mut staging)) }
            .map_err(d3d("CreateTexture2D"))?;
        let staging: ID3D11Resource = staging
            .ok_or_else(|| ReceiveError::Direct3D("no staging texture".to_owned()))?
            .cast()
            .map_err(d3d("staging texture"))?;
        Ok(Self { context, source, staging, size: (info.width, info.height) })
    }

    /// Copies the shared texture into `pixels` (tightly packed rows). Blocks until the GPU copy is done.
    fn read(&self, pixels: &mut [u8]) -> Result<(), ReceiveError> {
        let row_bytes = self.size.0 as usize * 4;
        // SAFETY: both resources belong to `self.context`'s device; Map blocks until the copy has finished, and while
        // mapped the source holds `size.1` rows of `RowPitch` bytes, of which the first `row_bytes` of each are read.
        unsafe {
            self.context.CopyResource(&self.staging, &self.source);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)).map_err(d3d("Map"))?;
            for (row, out) in pixels.chunks_exact_mut(row_bytes).enumerate() {
                let from = mapped.pData.cast::<u8>().add(row * mapped.RowPitch as usize);
                out.copy_from_slice(std::slice::from_raw_parts(from, row_bytes));
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(())
    }
}

/// Reads the sender's texture every `1 / poll_hz` seconds, whatever the sender is doing, until the last frame or silence.
///
/// # Errors
/// Fails when the sender never appears, Direct3D refuses, or a frame cannot be decoded.
pub fn run(config: &ReceiveConfig, poll_hz: f64) -> Result<ReceiverReport, ReceiveError> {
    let clock = Clock::new();
    let texture = SharedTexture::open(&wait_for_sender(&config.name)?)?;
    let mut pixels = vec![0u8; texture.size.0 as usize * texture.size.1 as usize * 4];
    let period = clock.ticks_in_ms(1000.0 / poll_hz);
    let mut tally = Tally::new(config.hash_every);
    let (mut deadline, mut last_new) = (clock.now(), Instant::now());

    loop {
        clock.sleep_until(deadline);
        deadline += period;
        let seen = clock.now();
        texture.read(&mut pixels)?;
        let read = clock.now();
        if tally.observe(&clock, &pixels, texture.size, (seen, read))? == Observation::New {
            last_new = Instant::now();
        }
        let finished = tally.received() > 0 && tally.last_frame() + 1 >= config.frames;
        if finished || (tally.received() > 0 && last_new.elapsed() > IDLE_TIMEOUT) {
            break;
        }
    }
    Ok(tally.finish(0.0))
}
