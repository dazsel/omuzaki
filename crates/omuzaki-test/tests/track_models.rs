//! The face tracker on its real graphs, through the `onnx` runtime: the model folder `tools/models/mediapipe-face.sh`
//! writes opens and loads, a frame with no face finds none, a run is repeatable bit for bit, the steady state allocates
//! nothing and a damaged file is refused. Gated on `OMUZAKI_TRACK_MODELS` (that folder) and `OMUZAKI_ORT_CACHE` (the
//! pinned ONNX Runtime archive, as in `omuzaki-ai-ort`); each test prints a line and passes when one is unset.
//!
//! This file holds the only `GlobalAlloc` in its binary; the counter is per thread, so tests don't see each other.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a panic is a test failure

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, SessionOptions};
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::{MODEL_FILES, ModelDir, ModelError};
use omuzaki_track::{FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};

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

/// Archives from the `OMUZAKI_ORT_CACHE` folder under their release file names; never downloads. Checks the pinned
/// size (the runtime itself refuses a library that does not load).
struct DirSource(PathBuf);

impl FileSource for DirSource {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let asset = pin.urls[0].rsplit('/').next().unwrap();
        let path = self.0.join(asset);
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let actual = std::fs::metadata(&path).map_err(io)?.len();
        if actual != pin.size {
            return Err(FetchError::Size { url: asset.to_owned(), expected: pin.size, actual });
        }
        Ok(path)
    }
}

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
    assert_eq!(a, b);
    assert!(a.landmarks.iter().flatten().all(|v| v.is_finite()));
}

/// Allocations the calling thread makes inside `body`.
fn allocations(body: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    body();
    WATCHING.with(|w| w.set(false));
    ALLOCATIONS.with(Cell::get)
}

#[test]
fn track_allocates_nothing_after_warm_up() {
    let Some((models, natives)) = env("track_allocates_nothing_after_warm_up") else { return };
    let pixels = gradient();
    let mut tracker = tracker(&models, &natives, Mode::Video);
    for i in 0..10 {
        let _ = tracker.track(&frame(&pixels), Seconds(f64::from(i) / 30.0)).unwrap();
        tracker.track_roi(&frame(&pixels), ROI, Seconds(f64::from(i) / 30.0 + 1.0)).unwrap();
    }
    let counted = allocations(|| {
        for i in 10..310 {
            let _ = tracker.track(&frame(&pixels), Seconds(f64::from(i) / 30.0)).unwrap();
        }
    });
    assert_eq!(counted, 0, "allocations over 300 `track` calls");
    // `track` finds no face in the gradient, so the landmark, blendshape and pose stages run through `track_roi`.
    let counted = allocations(|| {
        for i in 310..610 {
            tracker.track_roi(&frame(&pixels), ROI, Seconds(f64::from(i) / 30.0)).unwrap();
        }
    });
    assert_eq!(counted, 0, "allocations over 300 `track_roi` calls");
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
