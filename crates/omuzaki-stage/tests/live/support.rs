//! What the live tests share: a tracker over several scripted cameras that can be unplugged, an in-memory settings
//! service, and a performer with the real retarget, life layer and animator. Included through a `#[path]` attribute.

// Every test file that includes this module uses a different part of it.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim::{BusAnimator, BusAnimatorParams};
use omuzaki_anim_api::{ChannelBuf, DeviceProfile, FrameSource, HumanoidPose, LifeConfig, TrackFrame};
use omuzaki_anim_mock::track::{SyntheticSpec, synthetic_track};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::event::{Change, SettingChange, SettingsEvent, Subscription, SwitchReport};
use omuzaki_settings_api::meta::Apply;
use omuzaki_settings_api::profile::{ProfileId, ProfileInfo, ProfileSource};
use omuzaki_settings_api::registry;
use omuzaki_settings_api::service::{Layer, ListFilter, Resolved, Settings, Snapshot, Source};
use omuzaki_settings_api::session::SessionStore;
use omuzaki_stage::perform::{Performer, PerformerParts};
use omuzaki_track_api::camera::{CameraFormat, CameraId, CameraInfo, PixelFormat};
use omuzaki_track_api::doctor::Diagnosis;
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};
use omuzaki_track_mock::{FakeCamera, FakeTracker};

/// The scripted frames of a camera that sees a face for `seconds` from time zero, then nothing (as an unplugged camera
/// or a face out of view would leave it).
pub fn face_for(seconds: f32, seed: u64) -> Vec<(Duration, TrackFrame)> {
    let track = synthetic_track(&SyntheticSpec { seconds, seed, ..SyntheticSpec::default() });
    // Truncated to whole nanoseconds, so a frame on a step boundary is not released a step late.
    track.frames.iter().map(|f| (Duration::from_nanos((f.time.0 * 1e9) as u64), *f)).collect()
}

/// Calls the tracker saw, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `start` on this camera.
    Start(String),
    /// `stop`.
    Stop,
}

/// What the test keeps of a [`Cameras`] tracker after handing it over.
#[derive(Clone, Default)]
pub struct CameraLog {
    calls: Arc<Mutex<Vec<Call>>>,
    unplugged: Arc<Mutex<Vec<String>>>,
    refused: Arc<Mutex<Vec<String>>>,
    /// What the tracker's doctor finds while a capture runs, and how many rechecks it was asked for.
    findings: Arc<Mutex<Vec<Diagnosis>>>,
    rechecks: Arc<Mutex<u32>>,
}

impl CameraLog {
    /// Every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Whether a capture was ever started.
    pub fn started(&self) -> bool {
        self.calls().iter().any(|call| matches!(call, Call::Start(_)))
    }

    /// Makes opening `camera` fail while it stays in the device list (the OS refuses it).
    pub fn refuse(&self, camera: &str) {
        self.refused.lock().unwrap().push(camera.to_owned());
    }

    /// Makes the tracker's doctor find `findings` while a capture runs, until a recheck clears them.
    pub fn find(&self, findings: Vec<Diagnosis>) {
        *self.findings.lock().unwrap() = findings;
    }

    /// How many rechecks the tracker was asked for.
    pub fn rechecks(&self) -> u32 {
        *self.rechecks.lock().unwrap()
    }

    /// Takes `camera` out of the device list (its running session keeps going until its frames stop).
    pub fn unplug(&self, camera: &str) {
        self.unplugged.lock().unwrap().push(camera.to_owned());
    }
}

/// A tracker over several scripted cameras, each a [`FakeTracker`].
pub struct Cameras {
    cameras: Vec<(String, FakeTracker)>,
    log: CameraLog,
}

impl Cameras {
    /// Cameras named by `ids`, each replaying its script.
    pub fn new(cameras: Vec<(&str, Vec<(Duration, TrackFrame)>)>) -> (Self, CameraLog) {
        let log = CameraLog::default();
        let cameras = cameras.into_iter().map(|(id, script)| (id.to_owned(), FakeTracker::scripted(script))).collect();
        (Self { cameras, log: log.clone() }, log)
    }
}

impl Tracker for Cameras {
    fn devices(&self) -> Vec<CameraInfo> {
        let unplugged = self.log.unplugged.lock().unwrap();
        let format = CameraFormat::new(640, 480, 30, PixelFormat::Rgb8);
        self.cameras
            .iter()
            .filter(|(id, _)| !unplugged.contains(id))
            .map(|(id, _)| CameraInfo::new(CameraId(id.clone()), format!("Camera {id}"), vec![format]))
            .collect()
    }

    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        self.log.calls.lock().unwrap().push(Call::Start(device.0.clone()));
        if self.log.refused.lock().unwrap().contains(&device.0) {
            return Err(TrackError::PermissionDenied);
        }
        for (_, camera) in &mut self.cameras {
            camera.stop();
        }
        let config = config.with_camera(omuzaki_track_api::camera::CameraFormatRequest::new(640, 480, 30));
        let present = self.devices().iter().any(|info| info.id == *device);
        match self.cameras.iter_mut().find(|(id, _)| *id == device.0).filter(|_| present) {
            Some((_, camera)) => camera.start(&FakeCamera::id(), &config),
            None => Err(TrackError::NoDevice(device.clone())),
        }
    }

    fn stop(&mut self) {
        self.log.calls.lock().unwrap().push(Call::Stop);
        for (_, camera) in &mut self.cameras {
            camera.stop();
        }
    }

    fn latency(&self) -> LatencyStats {
        LatencyStats::default()
    }

    fn diagnoses(&self) -> Vec<Diagnosis> {
        let running = matches!(self.log.calls().last(), Some(Call::Start(_)));
        if running { self.log.findings.lock().unwrap().clone() } else { Vec::new() }
    }

    fn recheck(&self) {
        *self.log.rechecks.lock().unwrap() += 1;
        self.log.findings.lock().unwrap().clear();
    }

    /// A file that exists becomes a camera `video:<path>` replaying a face of its own, as `omuzaki-video` lists one.
    fn add_video(&mut self, path: &std::path::Path) -> Result<CameraInfo, TrackError> {
        let id = format!("video:{}", path.display());
        if !path.is_file() {
            return Err(TrackError::NoDevice(CameraId(id)));
        }
        if !self.cameras.iter().any(|(known, _)| *known == id) {
            self.cameras.push((id.clone(), FakeTracker::scripted(face_for(5.0, 9))));
        }
        let format = CameraFormat::new(640, 480, 30, PixelFormat::Rgb8);
        Ok(CameraInfo::new(CameraId(id), "clip.mp4 (video)", vec![format]))
    }
}

/// A performer with the real `ArkitRetarget`, `Life` (seed 7) and `BusAnimator`, reading nothing until a source is
/// swapped in, on `base`.
pub fn performer(base: &HumanoidPose) -> Performer {
    let parts = PerformerParts {
        source: Box::new(omuzaki_stage::perform::input::NoSource),
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(BusAnimator::new(BusAnimatorParams::default())),
    };
    Performer::new(parts, ChannelBuf::NEUTRAL, *base)
}

/// The middle of step `i`, so each `advance` runs exactly one step.
pub fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(omuzaki_stage::perform::STEP_HZ))
}

/// An in-memory settings service with only what the input ops call: values are checked against the registry and kept
/// by key. Everything else is out of these tests' reach and panics.
#[derive(Default)]
pub struct MemSettings {
    values: Mutex<BTreeMap<String, Value>>,
    /// While set, every write is refused as a full disk would.
    refusing: std::sync::atomic::AtomicBool,
}

impl MemSettings {
    /// Makes every write fail (`true`) or work again (`false`).
    pub fn refuse_writes(&self, refuse: bool) {
        self.refusing.store(refuse, std::sync::atomic::Ordering::SeqCst);
    }

    /// The stored value of `key`.
    pub fn value(&self, key: &str) -> Option<Value> {
        self.values.lock().unwrap().get(key).cloned()
    }
}

impl Settings for MemSettings {
    fn snapshot(&self) -> Arc<Snapshot> {
        let values = self.values.lock().unwrap();
        Arc::new(Snapshot::new(values.iter().map(|(k, v)| (k.clone(), (v.clone(), Source::Base))).collect()))
    }

    fn resolve(&self, _key: &str) -> Result<Resolved, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn list(&self, _filter: &ListFilter) -> Vec<Resolved> {
        unimplemented!("not used by the input ops")
    }

    fn set_raw(&self, key: &str, value: Value, _layer: Layer, mode: Mode, _by: &Actor) -> Result<Change, SettingError> {
        let registration = registry::find(key).ok_or_else(|| SettingError::unknown_key(key))?;
        registration.validate(&value)?;
        if mode == Mode::Apply && self.refusing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SettingError::invalid(key, "the disk is full"));
        }
        let mut values = self.values.lock().unwrap();
        let before = values.get(key).cloned().unwrap_or_else(|| registration.default_json());
        if mode == Mode::Apply {
            values.insert(key.to_owned(), value.clone());
        }
        Ok(Change::new(key, before, value, Apply::Live))
    }

    fn reset(&self, _key: &str, _layer: Layer, _mode: Mode, _by: &Actor) -> Result<Option<Change>, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn profiles(&self) -> Vec<ProfileInfo> {
        unimplemented!("not used by the input ops")
    }

    fn save_profile(
        &self,
        _name: &str,
        _from: ProfileSource,
        _overwrite: bool,
        _mode: Mode,
        _by: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn rename_profile(
        &self,
        _id: &ProfileId,
        _name: &str,
        _mode: Mode,
        _by: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn delete_profile(&self, _id: &ProfileId, _mode: Mode, _by: &Actor) -> Result<SwitchReport, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn switch_profile(&self, _to: Option<&ProfileId>, _mode: Mode, _by: &Actor) -> Result<SwitchReport, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn reload(&self, _mode: Mode, _by: &Actor) -> Result<SwitchReport, SettingError> {
        unimplemented!("not used by the input ops")
    }

    fn changes(&self, _limit: usize) -> Vec<SettingChange> {
        unimplemented!("not used by the input ops")
    }

    fn subscribe(&self) -> Subscription<SettingsEvent> {
        unimplemented!("not used by the input ops")
    }

    fn session(&self) -> &dyn SessionStore {
        unimplemented!("not used by the input ops")
    }
}
