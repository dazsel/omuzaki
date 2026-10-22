//! The `spout` output read back the way OBS's Spout2 source reads it, with none of the sender's code: find the name in
//! `SpoutSenderNames`, read the 280-byte info block, open the legacy share handle on a second D3D11 device and copy the
//! texture out. Windows only (the nightly native run); on other OSes the kind's availability is checked instead.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_stream_api::output::find_output_kind;

#[test]
fn the_kinds_are_registered_and_say_where_they_work() {
    omuzaki_stream_share::link();
    let spout = find_output_kind("spout").expect("spout is registered");
    let syphon = find_output_kind("syphon").expect("syphon is registered");
    assert_eq!((spout.available)().is_ok(), cfg!(windows));
    if !cfg!(target_os = "macos") {
        assert!((syphon.available)().is_err());
    }
}

#[cfg(windows)]
mod windows_loopback {
    #![allow(unsafe_code)]

    use std::time::{Duration, Instant};

    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::{AlphaEncoding, Image};
    use omuzaki_stream_api::encoder::Frame;
    use omuzaki_stream_api::error::StreamError;
    use omuzaki_stream_api::output::{AlphaNeed, OutputConfig, find_output_kind};
    use omuzaki_stream_api::time::Ticks;
    use omuzaki_stream_share::spout::{ACTIVE_SENDER, FORMAT_BGRA8, INFO_BYTES, NAME_BYTES, SENDER_NAMES, read_names};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
        D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
        ID3D11DeviceContext, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
    use windows::Win32::System::Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile};
    use windows::core::HSTRING;

    /// Copies `len` bytes out of the named mapping, or `None` if it does not exist.
    fn read_mapping(name: &str, len: usize) -> Option<Vec<u8>> {
        // SAFETY: opens read-only, copies `len` bytes of the view (every mapping read here is at least that long),
        // and unmaps and closes everything.
        unsafe {
            let mapping = OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(name)).ok()?;
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len);
            let bytes =
                (!view.Value.is_null()).then(|| std::slice::from_raw_parts(view.Value.cast::<u8>(), len).to_vec());
            if !view.Value.is_null() {
                let _ = UnmapViewOfFile(view);
            }
            let _ = CloseHandle(mapping);
            bytes
        }
    }

    /// The senders listed now: the list at this sender's size, or Spout's older default of 10 if another app made it.
    fn listed() -> Vec<String> {
        read_mapping(SENDER_NAMES, NAME_BYTES * 64)
            .or_else(|| read_mapping(SENDER_NAMES, NAME_BYTES * 10))
            .map(|list| read_names(&list))
            .unwrap_or_default()
    }

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// A receiver's device, as OBS has its own.
    fn device() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        let (mut device, mut context) = (None, None);
        // SAFETY: out-pointers to live locals; the default adapter's hardware driver.
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
        .ok()?;
        device.zip(context)
    }

    /// Opens the share handle and copies the texture's rows out, tightly packed.
    fn read_texture(handle: u32, size: UVec2) -> Vec<u8> {
        let (device, context) = device().expect("the receiver's device");
        let mut shared: Option<ID3D11Texture2D> = None;
        // SAFETY: a legacy share handle as Spout stores it, 32 bits widened back to a handle.
        unsafe { device.OpenSharedResource(HANDLE(handle as usize as *mut _), &raw mut shared) }
            .expect("the handle opens on another device");
        let shared = shared.expect("a texture");
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.x,
            Height: size.y,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT(FORMAT_BGRA8.cast_signed()),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
            ..Default::default()
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: a valid descriptor and out-pointer.
        unsafe { device.CreateTexture2D(&raw const desc, None, Some(&raw mut staging)) }.expect("staging");
        let staging = staging.expect("a staging texture");
        let row = size.x as usize * 4;
        let mut pixels = vec![0u8; row * size.y as usize];
        // SAFETY: both textures are on this device; Map blocks until the copy is done, and the mapped rows are
        // `RowPitch` apart, of which the first `row` bytes are read.
        unsafe {
            context.CopyResource(&staging, &shared);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped)).expect("map");
            for (y, out) in pixels.chunks_exact_mut(row).enumerate() {
                let from = mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize);
                out.copy_from_slice(std::slice::from_raw_parts(from, row));
            }
            context.Unmap(&staging, 0);
        }
        pixels
    }

    #[test]
    fn a_receiver_written_from_the_protocol_reads_the_frame() {
        omuzaki_stream_share::link();
        if device().is_none() {
            eprintln!("no Direct3D 11 hardware device here; the loopback needs one");
            return;
        }
        let name = format!("Omuzaki loopback {}", std::process::id());
        let size = UVec2::new(64, 32);
        let mut out = (find_output_kind("spout").expect("registered").make)();
        assert_eq!(out.info().alpha, AlphaNeed::Premultiplied);
        out.start(&OutputConfig::new(name.clone(), size, (60, 1)).expect("config")).expect("the sender starts");

        // A premultiplied picture with a gradient in every channel, so a swapped channel or a flipped row shows.
        let mut rgba = Vec::with_capacity((size.x * size.y * 4) as usize);
        for y in 0..size.y {
            for x in 0..size.x {
                let a = u8::try_from(128 + y * 4).unwrap();
                rgba.extend([u8::try_from(x * 2).unwrap(), u8::try_from(y * 3).unwrap(), 7, a]);
            }
        }
        let image = Image { size, alpha: AlphaEncoding::Premultiplied, rgba8: rgba.clone() };
        out.submit(&Frame::new(size, 0, Ticks(0)).with_cpu(&image)).expect("submitted");
        let started = Instant::now();
        while out.status().frames == 0 {
            assert!(started.elapsed() < Duration::from_secs(10), "{:?}", out.status().last_error);
            std::thread::yield_now();
        }

        assert!(listed().contains(&name));
        assert_eq!(read_names(&read_mapping(ACTIVE_SENDER, NAME_BYTES).expect("active")), [name.as_str()]);
        let info = read_mapping(&name, INFO_BYTES).expect("the info block exists");
        assert_eq!((word(&info, 4), word(&info, 8), word(&info, 12)), (size.x, size.y, FORMAT_BGRA8));
        assert_ne!(word(&info, 0), 0, "a share handle");

        let bgra = read_texture(word(&info, 0), size);
        let expected: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        assert_eq!(bgra, expected, "the receiver sees the frame, BGRA, top row first");

        // A second sender may not take the same name while this one runs.
        let mut twin = (find_output_kind("spout").expect("registered").make)();
        let taken = twin.start(&OutputConfig::new(name.clone(), size, (60, 1)).expect("config"));
        assert!(matches!(taken, Err(StreamError::Config(_))), "{taken:?}");

        out.stop();
        assert!(!listed().contains(&name), "stopping takes the name off the list");
        assert!(read_mapping(&name, INFO_BYTES).is_none(), "and closes its info block");
    }
}
