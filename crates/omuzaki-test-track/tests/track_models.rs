//! The face tracker on its real graphs, through the `onnx` runtime: the model folder `tools/models/mediapipe-face.sh`
//! writes opens and loads, a frame with no face finds none, a run is repeatable bit for bit, the steady state allocates
//! nothing in Rust and stays under a measured ceiling in ONNX Runtime's own code, and a damaged file is refused. Gated
//! on `OMUZAKI_TRACK_MODELS` (that folder) and `OMUZAKI_ORT_CACHE` (the pinned ONNX Runtime archive, as in
//! `omuzaki-ai-ort`); each test prints a line and passes when one is unset.
//!
//! This file holds the only `GlobalAlloc` in its binary, and (through `native_alloc`) the only interposed C allocator;
//! both counters are per thread, so tests don't see each other.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a panic is a test failure

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use omuzaki_ai_api::{FetchError, FileSource, NoProgress, PinnedFile, SessionOptions, Sha256};
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::{MODEL_FILES, ModelDir, ModelError};
use omuzaki_track::{FaceSample, FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};
use sha2::Digest;
use track_support::DirSource;

mod track_support;

// ONNX Runtime allocates in C++, past Rust's `#[global_allocator]`; this counts the calling thread's C allocator calls
// (#1600). It is `omuzaki-ai-ort`'s test support, shared by path; `build.rs` exports its symbols to the runtime.
#[path = "../../omuzaki-ai-ort/tests/common/native_alloc.rs"]
mod native_alloc;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// Counts and forwards to `System`: no obligation beyond `GlobalAlloc`'s own, which is the one reason to allow `unsafe`.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The model folder and the runtime cache, or `None` (with a line on stderr) when either variable is unset.
fn env(test: &str) -> Option<(PathBuf, DirSource)> {
    let models = std::env::var_os("OMUZAKI_TRACK_MODELS").map(PathBuf::from);
    let cache = std::env::var_os("OMUZAKI_ORT_CACHE").map(|dir| DirSource(PathBuf::from(dir)));
    let (Some(models), Some(cache)) = (models, cache) else {
        eprintln!("skipping {test}: set OMUZAKI_TRACK_MODELS (the model folder) and OMUZAKI_ORT_CACHE");
        return None;
    };
    Some((models, cache))
}

fn tracker(models: &std::path::Path, natives: &DirSource, mode: Mode) -> FaceTracker {
    let dir = ModelDir::open(models).unwrap();
    let graphs = dir.load(&OrtRuntime, natives, &SessionOptions::default()).unwrap();
    FaceTracker::new(graphs, &dir.geometry().unwrap(), FaceTrackerConfig { mode, ..FaceTrackerConfig::default() })
        .unwrap()
}

const W: u32 = 640;
const H: u32 = 480;

/// A procedural 640x480 frame: smooth gradients with a diagonal texture, so every pixel differs and no face is in it.
fn gradient() -> Vec<u8> {
    let mut pixels = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            pixels.push(u8::try_from(x * 255 / (W - 1)).unwrap());
            pixels.push(u8::try_from(y * 255 / (H - 1)).unwrap());
            pixels.push(u8::try_from((x + y) % 256).unwrap());
        }
    }
    pixels
}

fn frame(pixels: &[u8]) -> RgbFrame<'_> {
    RgbFrame { data: pixels, width: W, height: H, stride: (W * 3) as usize }
}

/// A region in the middle of the frame, where a face would be.
const ROI: NormRect = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.667, rotation: 0.0 };

#[test]
fn the_folder_opens_and_loads() {
    let Some((models, natives)) = env("the_folder_opens_and_loads") else { return };
    let dir = ModelDir::open(&models).unwrap();
    for name in MODEL_FILES {
        assert!(models.join(name).is_file(), "{name}");
    }
    let graphs = dir.load(&OrtRuntime, &natives, &SessionOptions::default()).unwrap();
    FaceTracker::new(graphs, &dir.geometry().unwrap(), FaceTrackerConfig::default()).unwrap();
}

#[test]
fn a_flat_grey_frame_has_no_face() {
    let Some((models, natives)) = env("a_flat_grey_frame_has_no_face") else { return };
    for mode in [Mode::Image, Mode::Video] {
        let mut tracker = tracker(&models, &natives, mode);
        let grey = vec![128u8; (W * H * 3) as usize];
        for i in 0..3 {
            let (sample, _) = tracker.track(&frame(&grey), Seconds(f64::from(i) / 30.0)).unwrap();
            assert!(sample.is_none(), "{mode:?} frame {i}");
        }
    }
}

#[test]
fn track_roi_is_bit_identical_across_runs() {
    let Some((models, natives)) = env("track_roi_is_bit_identical_across_runs") else { return };
    let pixels = gradient();
    let run = || {
        let mut tracker = tracker(&models, &natives, Mode::Image);
        tracker.track_roi(&frame(&pixels), ROI, Seconds(0.0))
    };
    let (a, b) = (run().unwrap(), run().unwrap());
    // Bits, not `PartialEq`: that one counts +0.0 and -0.0 as equal.
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(a.present, b.present);
    assert!(a.landmarks.iter().flatten().all(|v| v.is_finite()));
}

/// Every float of a sample as its bit pattern, in field order.
fn bits(sample: &FaceSample) -> Vec<u32> {
    let roi = &sample.roi;
    let rect = [roi.x_center, roi.y_center, roi.width, roi.height, roi.rotation];
    sample
        .blendshapes
        .iter()
        .chain(&sample.pose)
        .chain(sample.landmarks.iter().flatten())
        .chain([&sample.presence])
        .chain(&rect)
        .map(|v| v.to_bits())
        .collect()
}

/// What the calling thread allocated inside a body.
#[derive(Debug)]
struct Counts {
    /// Allocations through Rust's allocator (`alloc` and `realloc`).
    rust: usize,
    /// Allocations made inside ONNX Runtime and the C library, past Rust's allocator; `None` where they can't be counted.
    native: Option<usize>,
}

/// Allocations the calling thread makes inside `body`, split into Rust's and the native side's. The C allocator sees
/// both (Rust's `System` allocator calls it), so the native count is what is left after Rust's own are taken off.
fn allocations(body: impl FnOnce()) -> Counts {
    ALLOCATIONS.with(|n| n.set(0));
    native_alloc::start();
    WATCHING.with(|w| w.set(true));
    body();
    WATCHING.with(|w| w.set(false));
    let c_calls = native_alloc::stop();
    let rust = ALLOCATIONS.with(Cell::get);
    Counts { rust, native: native_alloc::SUPPORTED.then(|| c_calls.saturating_sub(rust)) }
}

/// The most native allocations 300 steady-state calls may make. Measured with the pinned models and ONNX Runtime
/// 1.28.3, one intra-op thread: 86 651 or 86 652 over 300 `track` calls (about 289 a call: the detector, the landmark
/// graph on its region and the blendshape graph) and 577 804 over 300 `track_roi` calls (about 1 926 a call), the same
/// in five runs. So ONNX Runtime does allocate in the steady state, unlike the Rust side. The ceilings carry 1 % of
/// tolerance for a runtime build that differs in the last few allocations; a graph or runtime change that adds more to
/// the hot path trips them. Raise one only with the new measurement in the commit message.
const NATIVE_CEILING_TRACK: usize = 87_600;
const NATIVE_CEILING_TRACK_ROI: usize = 583_600;

#[test]
fn track_allocates_nothing_in_rust_after_warm_up() {
    let Some((models, natives)) = env("track_allocates_nothing_in_rust_after_warm_up") else { return };
    let pixels = gradient();
    let mut tracker = tracker(&models, &natives, Mode::Video);
    // Video mode wants strictly increasing timestamps, so every call, `track` or `track_roi`, takes the next tick.
    let mut tick = 0_u32;
    let mut next = || {
        tick += 1;
        Seconds(f64::from(tick) / 30.0)
    };
    for _ in 0..10 {
        let _ = tracker.track(&frame(&pixels), next()).unwrap();
        tracker.track_roi(&frame(&pixels), ROI, next()).unwrap();
    }
    // With the interposition not taking hold, a native count of zero would prove nothing.
    if native_alloc::SUPPORTED {
        assert!(native_alloc::interposed(), "the C allocator interposition did not take hold; see native_alloc.rs");
    } else {
        eprintln!("track_allocates_nothing_in_rust_after_warm_up: native allocations are not counted on this platform");
    }
    let counted = allocations(|| {
        for _ in 0..300 {
            let _ = tracker.track(&frame(&pixels), next()).unwrap();
        }
    });
    eprintln!("300 `track` calls: {counted:?}");
    assert_eq!(counted.rust, 0, "Rust allocations over 300 `track` calls");
    if let Some(native) = counted.native {
        assert!(native <= NATIVE_CEILING_TRACK, "native allocations over 300 `track` calls: {native}");
    }
    // `track` finds no face in the gradient, so the landmark, blendshape and pose stages run through `track_roi`.
    let counted = allocations(|| {
        for _ in 0..300 {
            tracker.track_roi(&frame(&pixels), ROI, next()).unwrap();
        }
    });
    eprintln!("300 `track_roi` calls: {counted:?}");
    assert_eq!(counted.rust, 0, "Rust allocations over 300 `track_roi` calls");
    if let Some(native) = counted.native {
        assert!(native <= NATIVE_CEILING_TRACK_ROI, "native allocations over 300 `track_roi` calls: {native}");
    }
}

#[test]
fn a_corrupted_file_fails_open() {
    let Some((models, _)) = env("a_corrupted_file_fails_open") else { return };
    for name in MODEL_FILES {
        let copy = tempfile::tempdir().unwrap();
        for file in MODEL_FILES.into_iter().chain(["manifest.json"]) {
            std::fs::copy(models.join(file), copy.path().join(file)).unwrap();
        }
        let mut bytes = std::fs::read(copy.path().join(name)).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        std::fs::write(copy.path().join(name), bytes).unwrap();
        match ModelDir::open(copy.path()) {
            Err(ModelError::HashMismatch { file }) => assert_eq!(file, name),
            other => panic!("{name}: {other:?}"),
        }
    }
}

/// The runtime source checks the pinned SHA-256, not only the length: a same-length file with one byte changed is
/// refused. Not gated: it runs on a file it writes itself.
#[test]
fn the_runtime_source_refuses_a_same_length_corruption() {
    let cache = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..1000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let sha256 = Sha256(sha2::Sha256::digest(&bytes).into());
    let pin = PinnedFile::new(["https://example.invalid/releases/runtime.tgz"], sha256, 1000);
    let source = DirSource(cache.path().to_path_buf());
    let path = cache.path().join("runtime.tgz");

    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(source.get(&pin, &NoProgress).unwrap(), path);

    let mut corrupted = bytes;
    corrupted[500] ^= 0x01;
    std::fs::write(&path, &corrupted).unwrap();
    match source.get(&pin, &NoProgress) {
        Err(FetchError::Hash { url, expected, actual }) => {
            assert_eq!(url, "runtime.tgz");
            assert_eq!(expected, sha256);
            assert_eq!(actual, Sha256(sha2::Sha256::digest(&corrupted).into()));
        }
        other => panic!("{other:?}"),
    }

    std::fs::write(&path, &corrupted[..999]).unwrap();
    assert!(matches!(source.get(&pin, &NoProgress), Err(FetchError::Size { expected: 1000, actual: 999, .. })));
}
