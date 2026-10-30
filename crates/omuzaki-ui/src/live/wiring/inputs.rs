//! The studio's live inputs, built once at start-up: what the App hands to its input (`LiveInput`), so the webcam,
//! the phone and VMC out reach the person (PF-01 to PF-04).
//!
//! ```text
//!  <install>/models/mediapipe-face  ─▶ face_models ─▶ face_tracker ─┐
//!  <install>/models/mediapipe-body  ─▶ BodyModelDir (on demand) ────┤
//!  <install>/natives (the runtime's pinned archive)  ───────────────┤
//!  the platform's camera (Media Foundation on Windows) ─────────────┴▶ WebcamTracker
//!  the `input.phone` setting ─▶ NetTracker          (VMC out) ─▶ VmcOut
//! ```
//!
//! Everything the face tracker loads comes from the install, checked against its pins: the face models are fetched
//! when the release is built, never by the app (their licence record, `docs/licensing/deps/`), and the runtime's
//! native archive is read from the install's `natives` folder, never downloaded. When the models or the camera
//! backend are missing, the webcam still lists its cameras and starting it says what is missing, so the setup flow can
//! bind a camera and the person learns why the face doesn't move.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_ai::{CACHE_DIR, Registry, RegistrySettings, VerifiedCache};
use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, Sha256};
use omuzaki_anim_api::FrameSource;
use omuzaki_core::time::Clock;
use omuzaki_http::HttpFetcher;
use omuzaki_track_api::camera::{Camera, CameraId, CameraInfo};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::pose_out::PoseOutput;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};
use omuzaki_track_net::vmc_out::VmcOut;
use omuzaki_track_net::{NetTracker, PhoneSettings};
use sha2::Digest;

use omuzaki_track::body::BodyTrackerConfig;
use omuzaki_track::body::models::BodyModelDir;
use omuzaki_track::provision::{face_models, face_tracker};
use omuzaki_track::webcam::{BodyLoader, WebcamConfig, WebcamTracker};

/// The model registry over `data_dir` (the app's data folder): the shipped models' slots and the person's own models,
/// with the `model.*` ops over it. Files a model needs are downloaded through a verified cache in `<data_dir>/cache`,
/// checked against their pins; a user model runs in the sandboxed worker beside the executable.
///
/// # Errors
/// The registry's state folder can't be read or written.
pub fn model_registry(data_dir: &Path) -> Result<Registry, String> {
    let files = Arc::new(VerifiedCache::new(data_dir.join(CACHE_DIR), HttpFetcher::new()));
    let version = env!("CARGO_PKG_VERSION").parse().map_err(|e| format!("the app's version: {e}"))?;
    Registry::new(data_dir, files, version, RegistrySettings::default()).map_err(|e| e.to_string())
}

/// Where the face models sit under the install folder.
pub const FACE_MODELS_DIR: &str = "models/mediapipe-face";

/// Where the body models (the `MediaPipe` pose and hand landmarkers, `tools/models/mediapipe-body.sh`) sit under the
/// install folder.
pub const BODY_MODELS_DIR: &str = "models/mediapipe-body";

/// Where the runtimes' native archives sit under the install folder, under their release file names.
pub const NATIVES_DIR: &str = "natives";

/// The studio's inputs: the webcam, the phone link and the pose output.
pub struct StudioInputs {
    /// The webcam's tracker. With its models or camera backend missing, it lists what cameras it can and refuses to
    /// start with the reason in [`webcam_problem`](Self::webcam_problem).
    pub webcam: Box<dyn Tracker>,
    /// The phone link (a face-tracking app on the phone, or VMC in), on the `input.phone` setting.
    pub phone: Box<dyn Tracker>,
    /// VMC out.
    pub pose_out: Box<dyn PoseOutput>,
    /// Why the webcam cannot track, if it cannot: for the App's log and the setup flow.
    pub webcam_problem: Option<String>,
}

impl std::fmt::Debug for StudioInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StudioInputs").field("webcam_problem", &self.webcam_problem).finish_non_exhaustive()
    }
}

/// The inputs for an app installed in `install`, timing frames on `clock` (the perform loop's clock, so frames and
/// steps share one time line), with the phone link on `phone`. Cold: it loads the face models (about a second);
/// call it off the UI thread if that matters.
pub fn studio_inputs(install: &Path, clock: Arc<dyn Clock>, phone: PhoneSettings) -> StudioInputs {
    let (webcam, webcam_problem) = match platform_camera() {
        Ok(camera) => match webcam_tracker(camera, install, Arc::clone(&clock)) {
            Ok(tracker) => (Box::new(tracker) as Box<dyn Tracker>, None),
            Err((camera, problem)) => {
                (Box::new(Untracked { camera: Some(camera), problem: problem.clone() }) as _, Some(problem))
            }
        },
        Err(problem) => (Box::new(Untracked { camera: None, problem: problem.clone() }) as _, Some(problem)),
    };
    StudioInputs {
        webcam,
        phone: Box::new(NetTracker::new(phone, clock)),
        pose_out: Box::new(VmcOut::default()),
        webcam_problem,
    }
}

/// The webcam tracker over `camera` with the install's face models; on failure the camera comes back with the reason.
fn webcam_tracker(
    camera: Box<dyn Camera>,
    install: &Path,
    clock: Arc<dyn Clock>,
) -> Result<WebcamTracker, (Box<dyn Camera>, String)> {
    let dir = install.join(FACE_MODELS_DIR);
    let loaded = face_models(&dir).and_then(|models| face_tracker(&models, &InstallFiles(install.join(NATIVES_DIR))));
    match loaded {
        Ok(tracker) => Ok(WebcamTracker::new(camera, Box::new(tracker), WebcamConfig::default(), clock)
            .with_body(body_loader(install))),
        Err(e) => Err((camera, format!("the face tracker could not load from the install: {e}"))),
    }
}

/// How the webcam builds its body tracker, when `input.track_body` first asks: the install's body models, checked
/// against their manifest, through the same runtime archive as the face. A missing folder says so in `input.body`.
fn body_loader(install: &Path) -> BodyLoader {
    let (dir, natives) = (install.join(BODY_MODELS_DIR), InstallFiles(install.join(NATIVES_DIR)));
    Arc::new(move |size| {
        let models = BodyModelDir::open(&dir, size).map_err(|e| format!("the body models: {e}"))?;
        models.tracker(&natives, BodyTrackerConfig::default()).map_err(|e| format!("the body tracker: {e}"))
    })
}

/// The platform's camera: Media Foundation on Windows.
#[cfg(windows)]
fn platform_camera() -> Result<Box<dyn Camera>, String> {
    omuzaki_camera_mf::reader::MfCamera::new()
        .map(|camera| Box::new(camera) as Box<dyn Camera>)
        .map_err(|e| format!("the camera system could not start: {e}"))
}

/// The platform's camera: none yet off Windows.
#[cfg(not(windows))]
fn platform_camera() -> Result<Box<dyn Camera>, String> {
    Err("this build has no camera backend for this platform yet".to_owned())
}

/// A webcam that cannot track: it lists its cameras, so one can be bound, and refuses to start with the reason.
struct Untracked {
    camera: Option<Box<dyn Camera>>,
    problem: String,
}

impl Tracker for Untracked {
    fn devices(&self) -> Vec<CameraInfo> {
        self.camera.as_ref().map(|camera| camera.enumerate()).unwrap_or_default()
    }

    fn start(&mut self, _device: &CameraId, _config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        Err(TrackError::Backend(self.problem.clone()))
    }

    fn stop(&mut self) {}

    fn latency(&self) -> LatencyStats {
        LatencyStats::default()
    }
}

/// The install's native archives as a [`FileSource`]: each pin's file is looked up under its release file name and
/// checked (size, then SHA-256) like a download would be. Nothing is ever fetched.
#[derive(Debug)]
pub struct InstallFiles(pub PathBuf);

impl FileSource for InstallFiles {
    fn get(&self, pin: &PinnedFile, _progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        let name = pin.urls.first().and_then(|url| url.rsplit('/').next()).unwrap_or_default();
        let path = self.0.join(name);
        let io = |e: std::io::Error| FetchError::Io { path: path.display().to_string(), reason: e.to_string() };
        let actual = std::fs::metadata(&path).map_err(io)?.len();
        if actual != pin.size {
            return Err(FetchError::Size { url: name.to_owned(), expected: pin.size, actual });
        }
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            hasher.update(buffer.get(..read).unwrap_or_default());
        }
        let actual = Sha256(hasher.finalize().into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: name.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::SystemClock;

    use super::*;

    struct Quiet;

    impl Progress for Quiet {
        fn stage(&self, _name: &str, _fraction: f32) {}

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn pin(bytes: &[u8]) -> PinnedFile {
        let sha = Sha256(sha2::Sha256::digest(bytes).into());
        PinnedFile::new(["https://example.invalid/release/runtime-archive.zip"], sha, bytes.len() as u64)
    }

    #[test]
    fn an_install_without_models_still_gives_every_input_and_says_why_the_face_cannot_track() {
        let install = tempfile::tempdir().unwrap();
        let inputs = studio_inputs(install.path(), Arc::new(SystemClock::new()), PhoneSettings::default());
        let problem = inputs.webcam_problem.clone().unwrap();
        let mut webcam = inputs.webcam;
        let refused = webcam.start(&CameraId("any".to_owned()), &TrackerConfig::default()).err().unwrap();
        assert!(matches!(refused, TrackError::Backend(ref reason) if *reason == problem), "{refused:?}");
        assert_eq!(inputs.phone.devices().len(), 1, "the phone link's one device is the chosen protocol");
        assert_eq!(inputs.pose_out.status().target, None);
    }

    #[test]
    fn the_install_hands_over_a_native_archive_only_with_its_pinned_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("runtime-archive.zip"), b"the archive").unwrap();
        let files = InstallFiles(dir.path().to_owned());
        assert_eq!(files.get(&pin(b"the archive"), &Quiet).unwrap(), dir.path().join("runtime-archive.zip"));
        assert!(matches!(files.get(&pin(b"the archive!"), &Quiet), Err(FetchError::Size { .. })));
        assert!(matches!(files.get(&pin(b"another one"), &Quiet), Err(FetchError::Hash { .. })));
        assert!(matches!(InstallFiles(dir.path().join("none")).get(&pin(b"x"), &Quiet), Err(FetchError::Io { .. })));
    }
}
