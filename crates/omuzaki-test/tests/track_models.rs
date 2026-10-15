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

use omuzaki_ai_api::{FetchError, FileSource, GraphOptimisation, NoProgress, PinnedFile, SessionOptions, Sha256};
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_core::time::Seconds;
use omuzaki_track::models::{MODEL_FILES, ModelDir, ModelError};
use omuzaki_track::{FaceSample, FaceTracker, FaceTrackerConfig, Mode, NormRect, RgbFrame};
use sha2::Digest;
use track_support::DirSource;

mod track_support;

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
    tracker_with(models, natives, mode, &SessionOptions::default())
}

fn tracker_with(models: &std::path::Path, natives: &DirSource, mode: Mode, options: &SessionOptions) -> FaceTracker {
    let dir = ModelDir::open(models).unwrap();
    let graphs = dir.load(&OrtRuntime, natives, options).unwrap();
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

/// A level above the default is allowed on a graph only while its runs repeat bit for bit; this holds for every graph
/// at both levels (the numeric check against `TFLite` is what keeps the face slots on the default: #1730).
#[test]
fn track_roi_is_bit_identical_across_runs_at_every_graph_optimisation_level() {
    let Some((models, natives)) = env("track_roi_is_bit_identical_across_runs_at_every_graph_optimisation_level")
    else {
        return;
    };
    let pixels = gradient();
    for level in [GraphOptimisation::Basic, GraphOptimisation::Extended] {
        let options = SessionOptions::default().with_graph_optimisation(level);
        let run = || {
            let mut tracker = tracker_with(&models, &natives, Mode::Video, &options);
            (0..3)
                .flat_map(|i| bits(&tracker.track_roi(&frame(&pixels), ROI, Seconds(f64::from(i) / 30.0)).unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run(), "{level:?}");
    }
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
    let counted = allocations(|| {
        for _ in 0..300 {
            let _ = tracker.track(&frame(&pixels), next()).unwrap();
        }
    });
    assert_eq!(counted, 0, "allocations over 300 `track` calls");
    // `track` finds no face in the gradient, so the landmark, blendshape and pose stages run through `track_roi`.
    let counted = allocations(|| {
        for _ in 0..300 {
            tracker.track_roi(&frame(&pixels), ROI, next()).unwrap();
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
