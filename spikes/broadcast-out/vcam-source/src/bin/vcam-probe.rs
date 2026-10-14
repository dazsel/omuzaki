//! Tests the virtual camera source without installing anything, and prints exactly what installing it would take.
//!
//! - `env`: read-only facts about this machine (Windows build, Frame Server, Smart App Control, camera privacy).
//! - `selftest`: loads the DLL directly (no COM registration), makes the source through its class factory, and reads frames
//!   through Media Foundation's Source Reader as an app would: native NV12, YUY2, and RGB32 through the video processor.
//! - `api-check`: calls `MFCreateVirtualCamera` to see whether this machine accepts the request; it never calls `Start`.
//! - `print-registration`: prints the registry values and file permissions a real install needs. Does nothing itself.
//! - `start-camera`: the step that makes the camera appear in apps. Needs the DLL registered by the user first.

#![allow(linker_messages)] // old binutils warns about rustc's .drectve sections on every Windows link; nothing to act on

#[cfg(windows)]
mod probe {
    #![allow(unsafe_code)]

    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::time::Instant;

    use clap::{Parser, Subcommand, ValueEnum};
    use omuzaki_vcam_source::env;
    use omuzaki_vcam_source::format::{CLSID_TEXT, CLSID_U128, FPS, HEIGHT, PixelFormat, WIDTH};
    use omuzaki_vcam_source::pattern::{self, BARS};
    use windows::Win32::Foundation::{FreeLibrary, HMODULE, S_OK};
    use windows::Win32::Media::KernelStreaming::IKsControl;
    use windows::Win32::Media::MediaFoundation::{
        IMFAttributes, IMFGetService, IMFMediaSource, IMFMediaSourceEx, IMFMediaType, IMFSample, IMFSourceReader,
        MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
        MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MFCreateAttributes,
        MFCreateMediaType, MFCreateSourceReaderFromMediaSource, MFCreateVirtualCamera, MFGetSystemTime,
        MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup, MFVideoFormat_NV12, MFVideoFormat_RGB32,
        MFVideoFormat_YUY2, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
        MFVirtualCameraType_SoftwareCameraSource,
    };
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, IClassFactory};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{GUID, HRESULT, HSTRING, Interface, s};

    #[derive(Parser)]
    #[command(about = "Spike: test and describe the Media Foundation virtual camera source")]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Read-only facts about this machine.
        Env,
        /// Load the DLL without registering it and read frames through the Source Reader.
        Selftest {
            /// The camera DLL (default: next to this exe).
            #[arg(long)]
            dll: Option<PathBuf>,
            /// Frames to read per format.
            #[arg(long, default_value_t = 90)]
            frames: u32,
        },
        /// Ask Media Foundation to create the virtual camera object (no Start, so nothing is registered).
        ApiCheck,
        /// Print the install steps for a DLL at this path. Changes nothing.
        PrintRegistration {
            /// Where the DLL will be installed.
            #[arg(long, default_value = r"C:\Program Files\Omuzaki\omuzaki_vcam_source.dll")]
            dll: String,
        },
        /// Create and start the virtual camera. The DLL must already be registered (see print-registration).
        StartCamera {
            /// Required: confirms the user registered the DLL and wants the camera created.
            #[arg(long)]
            confirm_registered: bool,
            /// Seconds to keep it running (0 = until Enter is pressed).
            #[arg(long, default_value_t = 0)]
            seconds: u64,
        },
    }

    #[derive(Clone, Copy, ValueEnum)]
    enum Which {
        Nv12,
        Yuy2,
    }

    pub fn main() -> Result<(), String> {
        match Cli::parse().command {
            Command::Env => {
                for line in env::report().lines {
                    println!("{line}");
                }
                Ok(())
            }
            Command::Selftest { dll, frames } => selftest(dll, frames),
            Command::ApiCheck => api_check(),
            Command::PrintRegistration { dll } => {
                print_registration(&dll);
                Ok(())
            }
            Command::StartCamera { confirm_registered, seconds } => start_camera(confirm_registered, seconds),
        }
    }

    /// Keeps COM and Media Foundation up for the life of the value.
    struct Platform;

    impl Platform {
        fn start() -> Result<Self, String> {
            // SAFETY: balanced by `Drop`; called once per run on the main thread.
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| format!("CoInitializeEx: {e}"))?;
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("MFStartup: {e}"))?;
            }
            Ok(Self)
        }
    }

    impl Drop for Platform {
        fn drop(&mut self) {
            // SAFETY: balances the calls in `start`.
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    /// `MF_SOURCE_READER_FIRST_VIDEO_STREAM` as the unsigned stream index the Source Reader methods take.
    const FIRST_VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();

    type DllGetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
    type DllCanUnloadNow = unsafe extern "system" fn() -> HRESULT;

    /// The DLL loaded straight from a path, the way a test host (and Microsoft's own sample tests) load a media source.
    struct LoadedDll {
        module: HMODULE,
        get_class_object: DllGetClassObject,
        can_unload_now: DllCanUnloadNow,
    }

    impl LoadedDll {
        fn load(path: &std::path::Path) -> Result<Self, String> {
            // SAFETY: loads a DLL we built; the exports are looked up by name and cast to their documented signatures.
            unsafe {
                let module = LoadLibraryW(&HSTRING::from(path.as_os_str()))
                    .map_err(|e| format!("LoadLibrary {}: {e}", path.display()))?;
                let get = GetProcAddress(module, s!("DllGetClassObject")).ok_or("DllGetClassObject not exported")?;
                let can = GetProcAddress(module, s!("DllCanUnloadNow")).ok_or("DllCanUnloadNow not exported")?;
                Ok(Self {
                    module,
                    get_class_object: std::mem::transmute::<unsafe extern "system" fn() -> isize, DllGetClassObject>(
                        get,
                    ),
                    can_unload_now: std::mem::transmute::<unsafe extern "system" fn() -> isize, DllCanUnloadNow>(can),
                })
            }
        }

        fn factory(&self, clsid: GUID) -> Result<IClassFactory, String> {
            let mut out: Option<IClassFactory> = None;
            let iid = IClassFactory::IID;
            // SAFETY: valid GUID pointers and an out-pointer for an IClassFactory.
            unsafe { (self.get_class_object)(&raw const clsid, &raw const iid, (&raw mut out).cast()) }
                .ok()
                .map_err(|e| format!("DllGetClassObject: {e}"))?;
            out.ok_or_else(|| "no factory returned".to_owned())
        }

        fn unloadable(&self) -> bool {
            // SAFETY: the export takes no arguments.
            unsafe { (self.can_unload_now)() == S_OK }
        }
    }

    impl Drop for LoadedDll {
        fn drop(&mut self) {
            // SAFETY: the module handle came from LoadLibraryW and nothing from it is used afterwards.
            unsafe {
                let _ = FreeLibrary(self.module);
            }
        }
    }

    fn selftest(dll: Option<PathBuf>, frames: u32) -> Result<(), String> {
        let path = match dll {
            Some(path) => path,
            None => std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("omuzaki_vcam_source.dll"),
        };
        let _platform = Platform::start()?;
        let library = LoadedDll::load(&path)?;
        println!("loaded {} without registering it", path.display());

        // The wrong class id must be refused, the right one served.
        let refused = library.factory(GUID::from_u128(0xDEAD_BEEF_0000_0000_0000_0000_0000_0001));
        println!("unknown CLSID refused: {}", refused.is_err());
        let factory = library.factory(GUID::from_u128(CLSID_U128))?;

        let source = new_source(&factory)?;
        // The interfaces Frame Server's contract requires of the source.
        let required = (
            source.cast::<IMFMediaSourceEx>().is_ok(),
            source.cast::<IMFGetService>().is_ok(),
            source.cast::<IKsControl>().is_ok(),
        );
        println!("source implements IMFMediaSourceEx, IMFGetService, IKsControl: {required:?}");
        // SAFETY: no arguments.
        let characteristics = unsafe { source.GetCharacteristics() }.map_err(|e| e.to_string())?;
        println!("characteristics: {characteristics:#x} (1 = live, no seek or pause)");

        // A Source Reader shuts its source down when it is released, so each phase gets a fresh source.
        drop(source);
        for (which, label) in [(Which::Nv12, "NV12"), (Which::Yuy2, "YUY2")] {
            read_native(&new_source(&factory)?, which, label, frames)?;
        }
        read_rgb32_through_video_processor(&new_source(&factory)?)?;
        drop(factory);
        println!("DllCanUnloadNow after releasing everything: {} (S_OK means no leaked objects)", library.unloadable());
        Ok(())
    }

    fn new_source(factory: &IClassFactory) -> Result<IMFMediaSource, String> {
        // SAFETY: no aggregation, and the requested interface is one the source implements.
        unsafe { factory.CreateInstance(None) }.map_err(|e| format!("CreateInstance: {e}"))
    }

    fn format_of(which: Which) -> PixelFormat {
        match which {
            Which::Nv12 => PixelFormat::Nv12,
            Which::Yuy2 => PixelFormat::Yuy2,
        }
    }

    /// The native media type of the camera with `subtype`.
    fn native_type(reader: &IMFSourceReader, subtype: &GUID) -> Result<IMFMediaType, String> {
        for index in 0.. {
            // SAFETY: a plain query on a live reader; running off the end returns an error that ends the loop.
            let Ok(candidate) = (unsafe { reader.GetNativeMediaType(FIRST_VIDEO_STREAM, index) }) else {
                break;
            };
            // SAFETY: attribute read on a live media type.
            if unsafe { candidate.GetGUID(&MF_MT_SUBTYPE) }.is_ok_and(|g| g == *subtype) {
                return Ok(candidate);
            }
        }
        Err(format!("the camera offers no media type with subtype {subtype:?}"))
    }

    /// Reads one sample and returns it with its flags, waiting for the next frame.
    fn read(reader: &IMFSourceReader) -> Result<(IMFSample, u32), String> {
        let (mut flags, mut sample) = (0u32, None);
        // SAFETY: out-pointers reference live locals; the call is synchronous.
        unsafe {
            reader
                .ReadSample(FIRST_VIDEO_STREAM, 0, None, Some(&raw mut flags), None, Some(&raw mut sample))
                .map_err(|e| format!("ReadSample: {e}"))?;
        }
        sample.map(|s| (s, flags)).ok_or_else(|| format!("no sample (flags {flags:#x})"))
    }

    fn with_bytes<T>(sample: &IMFSample, f: impl FnOnce(&[u8]) -> T) -> Result<T, String> {
        // SAFETY: the buffer is locked for the duration of `f` and the slice covers exactly its current length.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(|e| e.to_string())?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&raw mut data, None, Some(&raw mut length)).map_err(|e| e.to_string())?;
            let result = f(std::slice::from_raw_parts(data, length as usize));
            buffer.Unlock().map_err(|e| e.to_string())?;
            Ok(result)
        }
    }

    fn read_native(source: &IMFMediaSource, which: Which, label: &str, frames: u32) -> Result<(), String> {
        // SAFETY: plain construction.
        let reader =
            unsafe { MFCreateSourceReaderFromMediaSource(source, None) }.map_err(|e| format!("source reader: {e}"))?;
        let format = format_of(which);
        let subtype = if matches!(which, Which::Nv12) { MFVideoFormat_NV12 } else { MFVideoFormat_YUY2 };
        let chosen = native_type(&reader, &subtype)?;
        // SAFETY: the type came from this reader.
        unsafe { reader.SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &chosen) }
            .map_err(|e| format!("SetCurrentMediaType {label}: {e}"))?;

        let started = Instant::now();
        let (mut wrong_size, mut counter_gaps, mut last_counter, mut tick_gaps) = (0u32, 0u32, None::<u64>, 0u32);
        let (mut lateness_ms, mut intervals_ms) = (Vec::new(), Vec::new());
        let (mut last_time, mut last_arrival) = (None::<i64>, None::<Instant>);
        for _ in 0..frames {
            let (sample, _flags) = read(&reader)?;
            let arrival = Instant::now();
            // SAFETY: reads from a live sample and the system clock.
            let (time, now) = unsafe { (sample.GetSampleTime().map_err(|e| e.to_string())?, MFGetSystemTime()) };
            #[allow(clippy::cast_precision_loss)] // microsecond-scale differences, far below f64's integer range
            lateness_ms.push((now - time) as f64 / 10_000.0);
            if let Some(previous) = last_arrival {
                intervals_ms.push(arrival.duration_since(previous).as_secs_f64() * 1000.0);
            }
            if let Some(previous) = last_time
                && !(omuzaki_vcam_source::format::FRAME_DURATION_100NS - 20_000
                    ..=2 * omuzaki_vcam_source::format::FRAME_DURATION_100NS)
                    .contains(&(time - previous))
            {
                tick_gaps += 1;
            }
            let (counter, size_ok) = with_bytes(&sample, |bytes| {
                let luma = |x: u32, y: u32| match format {
                    PixelFormat::Nv12 => bytes[(y * WIDTH + x) as usize],
                    PixelFormat::Yuy2 => bytes[((y * WIDTH + x) * 2) as usize],
                };
                (pattern::read_counter(WIDTH, HEIGHT, luma), bytes.len() == format.frame_bytes(WIDTH, HEIGHT))
            })?;
            wrong_size += u32::from(!size_ok);
            if let Some(previous) = last_counter
                && counter != previous + 1
            {
                counter_gaps += 1;
            }
            last_counter = Some(counter);
            last_time = Some(time);
            last_arrival = Some(arrival);
        }
        let seconds = started.elapsed().as_secs_f64();
        #[allow(clippy::cast_precision_loss)] // a few hundred samples
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let worst = |v: &[f64]| v.iter().copied().fold(f64::MIN, f64::max);
        println!(
            "{label}: {frames} frames in {seconds:.2} s = {:.1} fps (target {FPS}); wrong sized buffers {wrong_size}; frame-number gaps {counter_gaps}; timestamp irregularities {tick_gaps}",
            f64::from(frames) / seconds
        );
        println!(
            "  arrival interval mean {:.2} ms, worst {:.2} ms; sample timestamp to ReadSample return: mean {:.2} ms, worst {:.2} ms",
            mean(&intervals_ms),
            worst(&intervals_ms),
            mean(&lateness_ms),
            worst(&lateness_ms)
        );
        Ok(())
    }

    /// The Source Reader's video processor converts the camera's NV12 to RGB32 using the matrix and range the media type
    /// declares; if those declarations were wrong the bar colours would come out off.
    fn read_rgb32_through_video_processor(source: &IMFMediaSource) -> Result<(), String> {
        // SAFETY: plain construction and attribute setting on objects created here.
        let reader = unsafe {
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&raw mut attributes, 1).map_err(|e| e.to_string())?;
            let attributes = attributes.ok_or("no attributes")?;
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1).map_err(|e| e.to_string())?;
            let reader =
                MFCreateSourceReaderFromMediaSource(source, &attributes).map_err(|e| format!("source reader: {e}"))?;
            let rgb = MFCreateMediaType().map_err(|e| e.to_string())?;
            rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
            rgb.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(WIDTH) << 32) | u64::from(HEIGHT))
                .map_err(|e| e.to_string())?;
            rgb.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(FPS) << 32) | 1).map_err(|e| e.to_string())?;
            // A positive stride asks for top-down rows (RGB32's default is bottom-up).
            rgb.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH * 4).map_err(|e| e.to_string())?;
            reader
                .SetCurrentMediaType(FIRST_VIDEO_STREAM, None, &rgb)
                .map_err(|e| format!("SetCurrentMediaType RGB32: {e}"))?;
            reader
        };
        // Skip a few frames so the converter has settled, then compare one frame's bar colours with the intended ones.
        let mut worst = 0u32;
        let mut frame = None;
        for _ in 0..5 {
            frame = Some(read(&reader)?.0);
        }
        let sample = frame.ok_or("no frame")?;
        with_bytes(&sample, |bytes| {
            for (bar, expected) in BARS.iter().enumerate() {
                let x = (u32::try_from(bar).unwrap_or(0) * WIDTH / 8 + WIDTH / 16) as usize;
                let at = (100 * WIDTH as usize + x) * 4;
                let got = [bytes[at + 2], bytes[at + 1], bytes[at]];
                let error = got.iter().zip(expected).map(|(g, e)| u32::from(g.abs_diff(*e))).max().unwrap_or(0);
                worst = worst.max(error);
            }
        })?;
        println!(
            "RGB32 through the video processor: worst bar colour error {worst} levels of 255 (the BT.709 limited-range declaration is honoured when this is small)"
        );
        Ok(())
    }

    fn api_check() -> Result<(), String> {
        let _platform = Platform::start()?;
        // SAFETY: creates the configuration object only; `Start`, which would register the camera, is never called.
        let result = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from("Omuzaki spike camera"),
                &HSTRING::from(CLSID_TEXT),
                None,
            )
        };
        match result {
            Ok(_camera) => println!(
                "MFCreateVirtualCamera(SoftwareCameraSource, Session, CurrentUser, ...) succeeded; nothing was started or registered"
            ),
            Err(error) => println!("MFCreateVirtualCamera failed: {error}"),
        }
        Ok(())
    }

    fn print_registration(dll: &str) {
        println!("Installing the virtual camera needs these steps. This command changes nothing.\n");
        println!(
            "1. Put the DLL where the Frame Server services can read it. They run as Local Service and Local System, so a folder"
        );
        println!(
            "   under a user profile, a network share or the WSL file system will fail with access denied. Use, for example:"
        );
        println!(
            "     {dll}   (copied there by an installer running as administrator; inherited ACLs from Program Files are fine)\n"
        );
        println!(
            "2. Register the COM class machine-wide (administrator). MFVirtualCameraAccess_AllUsers and the registry write both need elevation;"
        );
        println!(
            "   Microsoft's own sample does this from an MSI, and its README says a per-user HKCU registration does not work:"
        );
        println!(r#"     reg add "HKLM\Software\Classes\CLSID\{CLSID_TEXT}" /ve /d "Omuzaki spike camera" /f"#);
        println!(r#"     reg add "HKLM\Software\Classes\CLSID\{CLSID_TEXT}\InprocServer32" /ve /d "{dll}" /f"#);
        println!(
            r#"     reg add "HKLM\Software\Classes\CLSID\{CLSID_TEXT}\InprocServer32" /v ThreadingModel /d Both /f"#
        );
        println!("   (or regsvr32 if the DLL exported DllRegisterServer; this spike's does not, on purpose.)\n");
        println!("3. Run `vcam-probe start-camera --confirm-registered` as the normal user. It calls");
        println!(
            "   MFCreateVirtualCamera(SoftwareCameraSource, Session, CurrentUser, name, {CLSID_TEXT}) and IMFVirtualCamera::Start."
        );
        println!(
            "   Session lifetime means the camera disappears when that process ends; System lifetime survives reboots.\n"
        );
        println!("To undo step 2: reg delete \"HKLM\\Software\\Classes\\CLSID\\{CLSID_TEXT}\" /f");
    }

    fn start_camera(confirmed: bool, seconds: u64) -> Result<(), String> {
        if !confirmed {
            return Err("refusing to start: pass --confirm-registered after you have registered the DLL (see print-registration)".to_owned());
        }
        let _platform = Platform::start()?;
        // SAFETY: standard virtual-camera lifecycle; Shutdown at the end releases it.
        unsafe {
            let camera = MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from("Omuzaki spike camera"),
                &HSTRING::from(CLSID_TEXT),
                None,
            )
            .map_err(|e| format!("MFCreateVirtualCamera: {e}"))?;
            camera.Start(None).map_err(|e| format!("Start (access denied usually means the DLL is unreadable by Frame Server or privacy blocks cameras): {e}"))?;
            println!(
                "virtual camera running: open the Windows Camera app and pick \"Omuzaki spike camera Windows Virtual Camera\""
            );
            if seconds == 0 {
                println!("press Enter to stop");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
            } else {
                std::thread::sleep(std::time::Duration::from_secs(seconds));
            }
            let _ = camera.Stop();
            camera.Shutdown().map_err(|e| format!("Shutdown: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match probe::main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vcam-probe is Windows-only; build with --target x86_64-pc-windows-gnu");
}
