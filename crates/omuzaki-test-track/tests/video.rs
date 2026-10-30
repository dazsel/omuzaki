//! A recorded video as a tracking input (#2585): a file added to the webcam tracker plays through its camera thread at
//! the file's own rate, every frame's pixels reaching the face tracker, as a camera's would. Offline, the kit's posed
//! frames (`omuzaki-stage`'s `body_frames`), encoded to MP4 by ffmpeg, are decoded and tracked frame by frame as fast as
//! they go, by the face tracker (with `OMUZAKI_TRACK_MODELS`) and the body tracker (with `OMUZAKI_BODY_MODELS`), each
//! timed against the clip's own length. That one is gated on `OMUZAKI_ORT_CACHE`, `OMUZAKI_BODY_FRAMES_DIR` and an
//! ffmpeg, and prints a line and passes when one is missing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use omuzaki_ai_api::SessionOptions;
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask};
use omuzaki_core::time::{Seconds, SystemClock};
use omuzaki_track::body::models::{BodyModelDir, PoseSize};
use omuzaki_track::body::{BodyTracker, BodyTrackerConfig};
use omuzaki_track::models::ModelDir;
use omuzaki_track::webcam::{FrameConverter, FrameTracker, WebcamConfig, WebcamTracker};
use omuzaki_track::{FaceSample, FaceTrackError, FaceTracker, FaceTrackerConfig, NormRect, RgbFrame, StageTimings};
use omuzaki_track_api::body::{BodyConfig, BodyFrame};
use omuzaki_track_api::camera::{CameraFrame, DeviceTime, PixelFormat};
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use omuzaki_track_video::camera::VideoCamera;
use omuzaki_track_video::ffmpeg::Ffmpeg;
use omuzaki_track_video::y4m::write_frame;
use omuzaki_track_video::{Picture, VideoInfo, VideoReader};

/// Frames in the synthetic clip.
const FRAMES: u8 = 12;

/// A face tracker that reports each frame it is handed: its time and the red of its centre pixel.
struct Probe(Sender<(f64, u8)>);

impl FrameTracker for Probe {
    fn track(
        &mut self,
        frame: &RgbFrame<'_>,
        time: Seconds,
    ) -> Result<(Option<FaceSample>, StageTimings), FaceTrackError> {
        let centre = (frame.height as usize / 2) * frame.stride + (frame.width as usize / 2) * 3;
        let _ = self.0.send((time.0, frame.data[centre]));
        let mut pose = [0.0; 16];
        for i in 0..4 {
            pose[i * 5] = 1.0;
        }
        let sample = FaceSample {
            blendshapes: [0.0; ARKIT_COUNT],
            present: ChannelMask::EMPTY,
            pose,
            landmarks: [[0.5; 3]; 478],
            presence: 1.0,
            roi: NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.5, rotation: 0.0 },
        };
        Ok((Some(sample), StageTimings::default()))
    }

    fn reset(&mut self) {}
}

/// A grey `.y4m` clip at `fps` whose frame `i` has luma `16 + 16 i`, so each frame can be told from its pixels.
fn clip(dir: &Path, fps: u32) -> PathBuf {
    let path = dir.join("clip.y4m");
    let mut out = BufWriter::new(File::create(&path).unwrap());
    let info = VideoInfo::new(64, 48, fps, 1);
    let mut picture = Picture::default();
    picture.resize(64, 48);
    for i in 0..FRAMES {
        let luma_end = picture.stride * 48;
        picture.data[..luma_end].fill(16 + 16 * i);
        picture.data[luma_end..].fill(128);
        write_frame(&mut out, &picture, (i == 0).then_some(info)).unwrap();
    }
    drop(out);
    path
}

#[test]
fn a_video_file_plays_through_the_webcam_tracker_at_its_own_rate() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(dir.path(), 30);
    let (sent, seen) = channel();
    let camera = VideoCamera::new(None, Ffmpeg::default());
    let mut tracker = WebcamTracker::new(
        Box::new(camera),
        Box::new(Probe(sent)),
        WebcamConfig::default(),
        Arc::new(SystemClock::new()),
    );
    let info = tracker.add_video(&path).unwrap();
    assert_eq!(tracker.devices(), vec![info.clone()], "the file is listed as a camera");
    let begun = Instant::now();
    let _source = tracker.start(&info.id, &TrackerConfig::default()).unwrap();
    // Two passes of the clip, each frame handed over as the tracker takes it.
    let frames: Vec<(f64, u8)> =
        (0..2 * usize::from(FRAMES)).map(|_| seen.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
    let elapsed = begun.elapsed();
    tracker.stop();

    // Paced: 23 frame periods at 30 fps is 767 ms; a file read as fast as it decodes would take a few.
    assert!(elapsed >= Duration::from_millis(700), "{elapsed:?}");
    // Every frame's own pixels, in order, and the file starts again at its end. A busy machine may drop a frame
    // (the newest wins), so each red must be the next frame's or a later one's.
    let index = |red: u8| ((f64::from(red) * 219.0 / 255.0) / 16.0).round() as i64;
    let indices: Vec<i64> = frames.iter().map(|&(_, red)| index(red)).collect();
    let passes = indices.windows(2).filter(|w| w[1] < w[0]).count();
    assert_eq!(passes, 1, "one loop back to the start: {indices:?}");
    assert!(indices.iter().all(|i| (0..i64::from(FRAMES)).contains(i)), "{indices:?}");
    // Times step at the file's rate: each gap a whole number of periods.
    for pair in frames.windows(2) {
        let periods = (pair[1].0 - pair[0].0) * 30.0;
        assert!(periods > 0.5 && (periods - periods.round()).abs() < 0.35, "a gap of {periods} periods: {frames:?}");
    }
}

/// The ffmpeg on this machine, if any.
fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// The kit's posed frames from `frames` (PPM), each held for 10 frames at 30 fps, as H.264 in MP4 the way a phone
/// records, in `dir`.
fn kit_mp4(frames: &Path, dir: &Path) -> PathBuf {
    let mp4 = dir.join("kit.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-framerate", "3", "-pattern_type", "glob", "-i"])
        .arg(format!("{}/*.ppm", frames.display()))
        .args(["-r", "30", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&mp4)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg encoded the clip");
    mp4
}

/// Reads `mp4` from the start and hands each frame, as RGB, with its time, to `track`, which says whether it found
/// what it tracks. Prints the frame count, the share found and the speed against real time, decoding included.
fn time_clip(what: &str, mp4: &Path, mut track: impl FnMut(&RgbFrame<'_>, f64) -> bool) -> u32 {
    let begun = Instant::now();
    let mut reader = VideoReader::open(mp4, &Ffmpeg::default()).unwrap();
    let info = reader.info();
    let (mut picture, mut converter) = (Picture::default(), FrameConverter::new());
    let (mut count, mut found) = (0_u32, 0_u32);
    while reader.read(&mut picture).unwrap() {
        let frame = CameraFrame::new(
            &picture.data,
            picture.width,
            picture.height,
            picture.stride,
            PixelFormat::Nv12,
            DeviceTime(0),
            u64::from(count),
        );
        let rgb = converter.convert(&frame).unwrap();
        found += u32::from(track(&rgb, f64::from(count) / info.fps()));
        count += 1;
    }
    let wall = begun.elapsed().as_secs_f64();
    let length = f64::from(count) / info.fps();
    println!(
        "{what}: {count} frames {}x{} at {:.0} fps ({length:.2} s of video) in {wall:.2} s, {:.1} ms a frame: {:.2}x \
         real time; found in {found}",
        info.width,
        info.height,
        info.fps(),
        wall * 1000.0 / f64::from(count.max(1)),
        length / wall,
    );
    count
}

#[test]
fn a_kit_clip_tracks_from_an_mp4_offline() {
    let test = "a_kit_clip_tracks_from_an_mp4_offline";
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let (Some(cache), Some(frames)) = (env("OMUZAKI_ORT_CACHE"), env("OMUZAKI_BODY_FRAMES_DIR")) else {
        eprintln!("skipping {test}: set OMUZAKI_ORT_CACHE and OMUZAKI_BODY_FRAMES_DIR, and a model folder");
        return;
    };
    if !have_ffmpeg() {
        eprintln!("skipping {test}: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mp4 = kit_mp4(&frames, dir.path());
    let natives = track_support::DirSource(cache);
    let decoded = time_clip("decoding alone", &mp4, |_, _| true);
    assert!(decoded >= 30, "{decoded} frames");

    if let Some(models) = env("OMUZAKI_TRACK_MODELS") {
        let dir = ModelDir::open(&models).unwrap();
        let graphs = dir.load(&OrtRuntime, &natives, &SessionOptions::default()).unwrap();
        let mut face = FaceTracker::new(graphs, &dir.geometry().unwrap(), FaceTrackerConfig::default()).unwrap();
        let count = time_clip("face", &mp4, |rgb, t| face.track(rgb, Seconds(t)).unwrap().0.is_some());
        assert_eq!(count, decoded);
    }
    if let Some(models) = env("OMUZAKI_BODY_MODELS") {
        let graphs = BodyModelDir::open(&models, PoseSize::Lite)
            .unwrap()
            .load(&OrtRuntime, &natives, &SessionOptions::default())
            .unwrap();
        let mut body = BodyTracker::new(graphs, BodyTrackerConfig::default()).unwrap();
        let mut out = BodyFrame::empty(0, Seconds(0.0));
        let count = time_clip("body (pose and both hands)", &mp4, |rgb, t| {
            body.track(rgb, t, &BodyConfig::default(), &mut out).unwrap();
            out.sees_anything()
        });
        assert_eq!(count, decoded);
    }
}

mod track_support;
