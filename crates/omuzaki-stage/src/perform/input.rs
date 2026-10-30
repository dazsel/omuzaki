//! The tracking input (PF-01, PF-02; `docs/design/first-run.md` §4, §5): which kind of source drives the face, which
//! camera it reads, whether the live mirror runs, and the camera permission. The `input.*` ops ask an [`InputHost`];
//! the App injects [`LiveInput`], which starts and stops a [`Tracker`] (the webcam's, or the phone link's from
//! [`LiveInput::with_phone`]) and hands each new frame source to the perform thread through a [`SourceSlot`].
//!
//! # Who may do what
//!
//! Which camera runs, and whether one runs at all, is a device choice (`omuzaki-track-api`, Permissions):
//! - choosing the kind of source and starting the mirror are the person's to confirm (an agent's request is queued);
//! - binding a physical camera and the OS permission prompt are the person's alone (an agent is refused, never
//!   queued).
//!
//! The ops enforce that; the host does what it is told.
//!
//! # Threads
//!
//! The host runs on the ops side, under its own lock: starting a tracker is cold work (a camera opens, a model loads).
//! The perform thread only calls [`SourceSlot::swap_into`] once per frame, which never blocks, allocates or frees: the
//! source it replaces goes back into the slot, and the ops side drops it on its next call.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_anim_api::{FrameSource, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Mode;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use omuzaki_track::gesture::Gesture;
use omuzaki_track_api::body::{BodyBoard, BodyConfig, BodyStatus, HandGesture};
use omuzaki_track_api::camera::CameraId;
use omuzaki_metrics_api::budget::{BudgetKey, BudgetUse, TRACK_FACE};
use omuzaki_track_api::doctor::{Diagnosis, Finding};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::pose_out::{PoseOutStatus, PoseOutput};
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::gesture::GestureWatch;
use super::guided::{FrameTap, GuidedRun, GuidedStatus};
use super::host::PerformHost;
use super::{Performer, Tracking};

/// The kind of source that drives the face.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A camera on this computer, through the face tracker.
    Webcam,
    /// An iPhone's face tracking over the network (PF-02).
    Iphone,
    /// No capture: the life layer (breathing, blinks, glances) moves the face on its own. What a fresh install and
    /// "Skip setup" leave.
    #[default]
    LifeLayer,
}

impl SourceKind {
    /// Whether this kind reads a camera on this computer, so it needs a bound camera to start.
    pub fn uses_camera(self) -> bool {
        self == Self::Webcam
    }
}

/// The kind of source, as the Machine setting `input.source`. Choosing it starts no capture, so an agent may ask, and
/// the person confirms (capture-adjacent keys are never `Free`, `docs/design/settings.md`). Machine scope: the next
/// computer may have no camera.
pub struct InputSource;

impl Setting for InputSource {
    type Value = SourceKind;
    const META: SettingMeta = SettingMeta::new(
        "input.source",
        "What drives the face: a webcam, an iPhone or the life layer alone",
        SettingScope::Machine,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> SourceKind {
        SourceKind::default()
    }
}

register_setting!(InputSource);

/// The camera the webcam source reads, as the Machine setting `input.camera`. A device binding: only the person sets
/// it (`AgentWrite::Never`, like `audio.bindings`), and the id may name the machine, so it never roams.
pub struct InputCamera;

/// The value of [`InputCamera`]: the bound camera, or none. A table with an optional `id`, because the settings files
/// are TOML and TOML has no null: an unbound camera is an empty table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BoundCamera {
    /// The camera's id, as `input.state` lists it; absent when no camera is bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CameraId>,
}

impl Setting for InputCamera {
    type Value = BoundCamera;
    const META: SettingMeta = SettingMeta::new(
        "input.camera",
        "The camera the webcam source reads",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> BoundCamera {
        BoundCamera::default()
    }
}

register_setting!(InputCamera);

/// What the camera tracks beside the face (PF-06 hands, PF-07 arms), as the Machine setting `input.body`. Switching it
/// on reads more of the person from the camera, so an agent may ask and the person confirms, as for the source. Machine
/// scope: the body models' cost depends on the computer.
pub struct InputBody;

impl Setting for InputBody {
    type Value = BodyConfig;
    const META: SettingMeta = SettingMeta::new(
        "input.body",
        "What the camera tracks beside the face: the arms, the hands and fingers, and whether on the heavier model",
        SettingScope::Machine,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> BodyConfig {
        BodyConfig::OFF
    }
}

register_setting!(InputBody);

/// Whether the app may use the camera, as the OS says.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Not asked yet, or the OS does not say until a camera opens.
    #[default]
    Unknown,
    /// The app may use the camera.
    Granted,
    /// The person refused; only they can change it, in the OS settings.
    Denied,
}

/// The OS camera permission. Platforms without a prompt use [`NoPrompt`].
pub trait CameraPermission: Send {
    /// The permission as it stands, without asking.
    fn status(&self) -> Permission;

    /// Shows the OS prompt (once; later calls return the answer) and returns the permission after it.
    fn request(&mut self) -> Permission;
}

/// A platform with no camera prompt: access is granted (desktop Linux and Windows).
#[derive(Copy, Clone, Debug, Default)]
pub struct NoPrompt;

impl CameraPermission for NoPrompt {
    fn status(&self) -> Permission {
        Permission::Granted
    }

    fn request(&mut self) -> Permission {
        Permission::Granted
    }
}

/// Whether the live mirror (the capture the blink check and Calibrate reuse) runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreviewState {
    /// No capture.
    #[default]
    Off,
    /// The tracker runs on the bound camera and feeds the performer.
    On {
        /// The camera it reads.
        camera: CameraId,
    },
}

impl PreviewState {
    /// Whether a capture runs.
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On { .. })
    }
}

/// A camera the tracker can read, for the picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CameraEntry {
    /// What `input.bind_camera` takes.
    pub id: CameraId,
    /// The name the OS shows.
    pub name: String,
    /// Whether it is the bound camera.
    pub bound: bool,
}

/// What the input is doing, read by `input.state`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InputState {
    /// The kind of source chosen.
    pub source: SourceKind,
    /// The bound camera, if any.
    pub camera: Option<CameraId>,
    /// Whether the bound camera is plugged in (in the device list now). `false` with no camera bound.
    pub camera_present: bool,
    /// Whether the live mirror runs.
    pub preview: PreviewState,
    /// The camera permission.
    pub permission: Permission,
    /// The cameras the tracker can read now. Listing them opens none.
    pub cameras: Vec<CameraEntry>,
    /// Whether the performer sees a face: `lost` once a running capture's frames stop (an unplugged camera, a face out
    /// of view), while the life layer keeps the face moving.
    pub tracking: Tracking,
}

/// How the running capture is doing (PF-26), read by `input.health`: its rate, latency and drops, its use of the
/// `track.face` budget, and what the doctor finds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct InputHealth {
    /// Whether a capture runs. With none, every other field is empty.
    pub running: bool,
    /// Tracker frames per second as the performer sees them.
    pub source_fps: f32,
    /// Latency from the frame's arrival to its publish, median, milliseconds.
    pub latency_ms_p50: f32,
    /// The same, 95th percentile.
    pub latency_ms_p95: f32,
    /// The same, worst seen.
    pub latency_ms_max: f32,
    /// Frames published this session.
    pub frames: u64,
    /// Frames lost or skipped this session.
    pub dropped: u64,
    /// `dropped` as a share of all the frames that came, 0 to 1.
    pub dropped_share: f32,
    /// The latency's 95th percentile against the spec §10 `track.face` budget (5 ms), for the webcam: the phone
    /// tracks on the phone, so its latency is the network's and has no budget here.
    pub budget: Option<BudgetUse>,
    /// What the tracking doctor finds now (`input.doctor` gives the fixes).
    pub findings: Vec<Finding>,
}

/// Why the input could not do what it was asked.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum InputError {
    /// The tracker lists no camera with this id (unplugged, or never there).
    #[error("no camera '{}' is connected", camera.0)]
    NoCamera {
        /// The id asked for.
        camera: CameraId,
    },
    /// The webcam source needs a bound camera to start.
    #[error("no camera is bound")]
    NoCameraBound,
    /// The chosen kind of source captures nothing, so there is no mirror to start.
    #[error("the life layer captures nothing, so there is no live mirror to start")]
    NothingToCapture,
    /// This build has no receiver for the kind of source: the App gave the input no phone tracker.
    #[error("this build has no {0:?} input yet")]
    Unsupported(SourceKind),
    /// The camera refused to start.
    #[error("the camera could not start: {0}")]
    Device(TrackError),
    /// The guided calibration reads the running capture, and none runs.
    #[error("no capture is running for the calibration to read")]
    CaptureOff,
    /// A guided calibration is already running.
    #[error("a guided calibration is already running")]
    CalibrationRunning,
    /// This input cannot run the guided calibration.
    #[error("this input has no guided calibration")]
    NoGuidedCalibration,
    /// This build has no pose output (VMC out).
    #[error("this build cannot send the pose to other apps")]
    NoPoseOutput,
    /// The pose output could not start.
    #[error("the pose output could not start: {0}")]
    PoseOutput(String),
    /// This build has no body tracking (its tracker has no body models).
    #[error("this build cannot track the arms and hands")]
    NoBodyTracking,
}

impl InputError {
    /// A [`InputError::NoCamera`] for `camera`.
    pub fn no_camera(camera: CameraId) -> Self {
        Self::NoCamera { camera }
    }
}

/// What the `input.*` ops ask of the tracking input. Every change takes a [`Mode`]: under [`Mode::DryRun`] it checks
/// what [`Mode::Apply`] would and returns the same answer, but starts, stops and records nothing. Each change returns
/// the value it replaced, so an op can undo it with the inverse call.
pub trait InputHost: Send + Sync {
    /// Chooses the kind of source. Starts no capture; a running mirror on another kind stops.
    ///
    /// # Errors
    /// None today; the result keeps the shape of the other calls.
    fn select(&self, source: SourceKind, mode: Mode) -> Result<SourceKind, InputError>;

    /// Binds the camera the webcam source reads (`None` unbinds it). A running mirror moves to the new camera, or
    /// stops when unbound.
    ///
    /// # Errors
    /// [`InputError::NoCamera`] for a camera the tracker does not list, or [`InputError::Device`] if the running mirror
    /// cannot restart on it: the change is then refused whole, the old binding stays and the mirror runs on the old
    /// camera again when that opens.
    fn bind_camera(&self, camera: Option<CameraId>, mode: Mode) -> Result<Option<CameraId>, InputError>;

    /// Starts (`on`) or stops the live mirror. Idempotent: starting a running mirror and stopping a stopped one change
    /// nothing. Returns whether it ran before.
    ///
    /// # Errors
    /// Starting: [`InputError::NothingToCapture`] for the life layer, [`InputError::Unsupported`] for a kind with no
    /// receiver, [`InputError::NoCameraBound`], [`InputError::NoCamera`] when the bound camera is unplugged, or
    /// [`InputError::Device`] when it refuses to open. Stopping never fails.
    fn preview(&self, on: bool, mode: Mode) -> Result<bool, InputError>;

    /// Shows the OS camera prompt and returns the permission after it.
    fn request_camera(&self) -> Permission;

    /// What the input is doing.
    fn state(&self) -> InputState;

    /// What the running tracker's doctor finds wrong with the input now (PF-28); empty with no capture running, or for
    /// a host without one.
    fn diagnoses(&self) -> Vec<Diagnosis> {
        Vec::new()
    }

    /// Makes the running tracker's doctor measure again from now. A no-op with no capture running.
    fn recheck(&self) {}

    /// Starts the guided calibration (PF-20) over the running capture, and answers its status.
    ///
    /// # Errors
    /// [`InputError::CaptureOff`] with no capture running, [`InputError::CalibrationRunning`] while one runs, and
    /// [`InputError::NoGuidedCalibration`] for a host without one.
    fn calibrate_guided(&self, mode: Mode) -> Result<GuidedStatus, InputError> {
        let _ = mode;
        Err(InputError::NoGuidedCalibration)
    }

    /// Stops a running guided calibration; the earlier calibration stays. Returns whether one was running.
    fn cancel_guided(&self) -> bool {
        false
    }

    /// Where the guided calibration stands.
    fn guided(&self) -> GuidedStatus {
        GuidedStatus::Idle
    }

    /// How the running capture is doing (PF-26).
    fn health(&self) -> InputHealth {
        InputHealth::default()
    }

    /// Sends the performed pose and face to `target` over VMC (PF-04's "out"), or stops with `None`, and answers what
    /// the output did before. Sending again to another target moves it.
    ///
    /// # Errors
    /// [`InputError::NoPoseOutput`] for a host without one, and [`InputError::PoseOutput`] when it cannot start.
    fn set_pose_out(&self, target: Option<SocketAddr>, mode: Mode) -> Result<PoseOutStatus, InputError> {
        let _ = (target, mode);
        Err(InputError::NoPoseOutput)
    }

    /// What the pose output is doing.
    fn pose_out(&self) -> PoseOutStatus {
        PoseOutStatus::default()
    }

    /// The face gestures showing now (PF-25): each presses its trigger key while it shows.
    fn gestures(&self) -> Vec<Gesture> {
        Vec::new()
    }

    /// The hand gestures showing now (PF-06), while body tracking runs: each presses its trigger key while it shows.
    fn hand_gestures(&self) -> Vec<HandGesture> {
        Vec::new()
    }

    /// Tracks the arms and hands beside the face (PF-06, PF-07) as `config` says, or stops with
    /// [`BodyConfig::OFF`]; returns the config before. It follows the capture: on while the camera runs, idle while it
    /// is off.
    ///
    /// # Errors
    /// [`InputError::NoBodyTracking`] for a host without it, [`InputError::Device`] when its thread cannot start.
    fn track_body(&self, config: BodyConfig, mode: Mode) -> Result<BodyConfig, InputError> {
        let _ = (config, mode);
        Err(InputError::NoBodyTracking)
    }

    /// How body tracking is doing.
    fn body(&self) -> BodyStatus {
        BodyStatus::default()
    }
}

/// A source that never delivers a frame: what the performer reads with no capture running (the life layer moves the
/// face). A zero-sized type, so boxing it allocates nothing.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoSource;

impl FrameSource for NoSource {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        None
    }
}

/// A source in its hand-off cell: the cell is allocated by the ops side and only moves between the two sides, so the
/// perform thread never allocates or frees one.
struct SourceCell {
    source: Box<dyn FrameSource>,
    /// The next cell of the retired list; null at its end. Only the perform thread writes it, before the cell is
    /// published.
    next: *mut SourceCell,
}

/// The hand-off of frame sources from the ops side to the perform thread: newest wins, and the source a swap replaces
/// comes back here, so the perform thread never frees one. Lock-free: two atomic pointers, so the live frame loop
/// takes no lock even to find the slot empty.
///
/// The ops side posts a cell with a source in it; the perform thread takes it, puts the source into the performer and
/// the performer's old source into the same cell (exchanging them through a source that is zero-sized, so nothing is
/// allocated), and pushes the cell on a list of retired cells for the ops side to drop. The list has no bound and the
/// push never waits, so a swap never depends on the ops side collecting first: the newest posted source is always
/// taken at the next swap.
pub struct SourceSlot {
    /// A posted cell not taken yet, or null.
    posted: AtomicPtr<SourceCell>,
    /// The head of the retired cells (a stack the perform thread pushes onto and the ops side empties whole), or null.
    retired: AtomicPtr<SourceCell>,
}

impl Default for SourceSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceSlot {
    /// An empty slot.
    pub fn new() -> Self {
        Self { posted: AtomicPtr::new(std::ptr::null_mut()), retired: AtomicPtr::new(std::ptr::null_mut()) }
    }

    /// Posts `source` for the next [`swap_into`](Self::swap_into), replacing one not taken yet. Ops side: allocates, and
    /// drops the sources the perform thread retired and the one it replaces.
    #[allow(unsafe_code, reason = "the lock-free hand-off passes cells as raw pointers; each use states why it is sound")]
    pub fn post(&self, source: Box<dyn FrameSource>) {
        self.collect();
        let cell = Box::into_raw(Box::new(SourceCell { source, next: std::ptr::null_mut() }));
        let replaced = self.posted.swap(cell, Ordering::AcqRel);
        // SAFETY: a non-null pointer in `posted` came from `Box::into_raw` in this function, and the swap made this
        // call its only holder: the perform thread takes cells by the same atomic swap.
        drop(unsafe { Self::reclaim(replaced) });
    }

    /// Drops the sources the perform thread retired. Ops side.
    #[allow(unsafe_code, reason = "the lock-free hand-off passes cells as raw pointers; each use states why it is sound")]
    pub fn collect(&self) {
        let mut cell = self.retired.swap(std::ptr::null_mut(), Ordering::AcqRel);
        while !cell.is_null() {
            // SAFETY: the swap took the whole list, so this call is the only holder of every cell on it, and each came
            // from `Box::into_raw`; `next` was written before the cell was pushed.
            let Some(boxed) = (unsafe { Self::reclaim(cell) }) else { break };
            cell = boxed.next;
        }
    }

    /// Whether a source is posted and not taken yet.
    pub fn has_posted(&self) -> bool {
        !self.posted.load(Ordering::Acquire).is_null()
    }

    /// The box behind `cell`, or `None` for null.
    ///
    /// # Safety
    /// `cell` is null or came from `Box::into_raw(Box<SourceCell>)` and is owned by the caller alone.
    #[allow(unsafe_code, reason = "the lock-free hand-off passes cells as raw pointers; each use states why it is sound")]
    unsafe fn reclaim(cell: *mut SourceCell) -> Option<Box<SourceCell>> {
        // SAFETY: the caller's contract.
        (!cell.is_null()).then(|| unsafe { Box::from_raw(cell) })
    }

    /// Puts a posted source into `performer` and returns true; false when nothing is posted. Perform thread: never
    /// blocks, locks, allocates or frees.
    #[allow(unsafe_code, reason = "the lock-free hand-off passes cells as raw pointers; each use states why it is sound")]
    pub fn swap_into(&self, performer: &mut Performer) -> bool {
        if !self.has_posted() {
            return false;
        }
        let cell = self.posted.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if cell.is_null() {
            return false;
        }
        // SAFETY: a non-null `posted` pointer is a leaked box from `post`, and the swap made this call its only holder.
        let cell_ref = unsafe { &mut *cell };
        // `NoSource` is zero-sized: boxing it allocates nothing, so the exchange below allocates and frees nothing.
        let incoming = std::mem::replace(&mut cell_ref.source, Box::new(NoSource));
        cell_ref.source = performer.set_source(incoming);
        // Push the cell: only this thread pushes, and the ops side only takes the whole list, so the head can change
        // under this loop only to null, and a failed exchange retries with the new head.
        let mut head = self.retired.load(Ordering::Acquire);
        loop {
            cell_ref.next = head;
            match self.retired.compare_exchange_weak(head, cell, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(current) => head = current,
            }
        }
        true
    }
}

impl Drop for SourceSlot {
    #[allow(unsafe_code, reason = "the lock-free hand-off passes cells as raw pointers; each use states why it is sound")]
    fn drop(&mut self) {
        let posted = *self.posted.get_mut();
        // SAFETY: `&mut self`: no other holder, and the pointer is a leaked box from `post`.
        drop(unsafe { Self::reclaim(posted) });
        self.collect();
    }
}

impl std::fmt::Debug for SourceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSlot").finish_non_exhaustive()
    }
}

/// Which of the input's trackers a capture runs on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Device {
    Webcam,
    Phone,
}

/// The input's own state, behind the host's lock.
struct Inner {
    tracker: Box<dyn Tracker>,
    /// The phone link (PF-02 to PF-04), when the App has one: its one device is the protocol the person chose.
    phone: Option<Box<dyn Tracker>>,
    permission: Box<dyn CameraPermission>,
    config: TrackerConfig,
    source: SourceKind,
    camera: Option<CameraId>,
    preview: PreviewState,
    /// A camera refused access since the last prompt, whatever the OS reported before opening it.
    refused: bool,
    /// Where each posted source copies its frames, for the guided calibration.
    tap: Arc<FrameTap>,
    /// The App's pose output (VMC out), when it has one.
    pose_out: Option<Box<dyn PoseOutput>>,
    /// Where body tracking publishes, when the App gave the performer a body reader.
    body: Option<Arc<BodyBoard>>,
}

impl Inner {
    /// Whether the tracker lists `camera` now.
    fn has_camera(&self, camera: &CameraId) -> bool {
        self.tracker.devices().iter().any(|info| info.id == *camera)
    }

    /// What starting the mirror on the current choice would need, checked without starting anything: the tracker
    /// and the device to open.
    fn startable(&self) -> Result<(Device, CameraId), InputError> {
        match self.source {
            SourceKind::LifeLayer => return Err(InputError::NothingToCapture),
            SourceKind::Iphone => {
                let unsupported = || InputError::Unsupported(SourceKind::Iphone);
                let phone = self.phone.as_ref().ok_or_else(unsupported)?;
                let link = phone.devices().into_iter().next().ok_or_else(unsupported)?;
                return Ok((Device::Phone, link.id));
            }
            SourceKind::Webcam => {}
        }
        let camera = self.camera.clone().ok_or(InputError::NoCameraBound)?;
        if !self.has_camera(&camera) {
            return Err(InputError::no_camera(camera));
        }
        Ok((Device::Webcam, camera))
    }

    /// Starts the webcam's tracker on `camera` and posts its source. On a refusal the mirror is off and the performer
    /// reads nothing.
    fn start(&mut self, camera: CameraId, slot: &SourceSlot) -> Result<(), InputError> {
        self.start_on(Device::Webcam, camera, slot)
    }

    /// Starts `device`'s tracker on `camera` (the phone link's protocol, for the phone) and posts its source, after
    /// stopping the other tracker. On a refusal the mirror is off and the performer reads nothing.
    fn start_on(&mut self, device: Device, camera: CameraId, slot: &SourceSlot) -> Result<(), InputError> {
        let started = match (device, self.phone.as_mut()) {
            (Device::Webcam, phone) => {
                if let Some(phone) = phone {
                    phone.stop();
                }
                self.tracker.start(&camera, &self.config)
            }
            (Device::Phone, Some(phone)) => {
                self.tracker.stop();
                phone.start(&camera, &self.config)
            }
            (Device::Phone, None) => return Err(InputError::Unsupported(SourceKind::Iphone)),
        };
        match started {
            Ok(source) => {
                slot.post(self.tap.tee(source));
                self.preview = PreviewState::On { camera };
                Ok(())
            }
            Err(error) => {
                self.refused |= error == TrackError::PermissionDenied;
                self.stop(slot);
                Err(InputError::Device(error))
            }
        }
    }

    /// Stops the trackers and hands the performer a source that delivers nothing. Idempotent.
    fn stop(&mut self, slot: &SourceSlot) {
        self.tracker.stop();
        if let Some(phone) = self.phone.as_mut() {
            phone.stop();
        }
        if self.preview.is_on() {
            slot.post(Box::new(NoSource));
        }
        self.preview = PreviewState::Off;
    }

    /// The permission: the OS's answer, or `Denied` once a camera refused access since the last prompt.
    fn permission(&self) -> Permission {
        if self.refused { Permission::Denied } else { self.permission.status() }
    }
}

/// The App's [`InputHost`]: a [`Tracker`] for the webcam source, the OS permission, and the [`SourceSlot`] the live
/// loop swaps sources from. The tracking state comes from the performer's [`PerformHost`].
pub struct LiveInput {
    inner: Mutex<Inner>,
    slot: Arc<SourceSlot>,
    perform: Arc<dyn PerformHost>,
    guided: GuidedRun,
    /// The gesture watch, from the first capture on (PF-25).
    gestures: Mutex<Option<GestureWatch>>,
}

impl LiveInput {
    /// An input over `tracker` that posts sources into `slot` and reads the tracking state from `perform`. It starts
    /// on the life layer with no camera bound and the mirror off; the App then replays the saved settings with
    /// [`InputHost::select`] and [`InputHost::bind_camera`], which start nothing.
    pub fn new(
        tracker: Box<dyn Tracker>,
        permission: Box<dyn CameraPermission>,
        config: TrackerConfig,
        slot: Arc<SourceSlot>,
        perform: Arc<dyn PerformHost>,
    ) -> Self {
        let inner = Inner {
            tracker,
            phone: None,
            permission,
            config,
            source: SourceKind::default(),
            camera: None,
            preview: PreviewState::Off,
            refused: false,
            tap: Arc::default(),
            pose_out: None,
            body: None,
        };
        Self { inner: Mutex::new(inner), slot, perform, guided: GuidedRun::default(), gestures: Mutex::default() }
    }

    /// With `phone` as the tracker the iPhone source starts (`omuzaki-track-net`'s `NetTracker`: its one device is
    /// the protocol the person chose). Without one, that source is [`InputError::Unsupported`].
    #[must_use]
    pub fn with_phone(self, phone: Box<dyn Tracker>) -> Self {
        let mut inner = self.inner.into_inner().unwrap_or_else(PoisonError::into_inner);
        inner.phone = Some(phone);
        Self { inner: Mutex::new(inner), slot: self.slot, perform: self.perform, guided: self.guided, gestures: self.gestures }
    }

    /// With `output` as the pose output `input.vmc_out` starts (`omuzaki-track-net`'s `VmcOut`). Without one, that
    /// op is [`InputError::NoPoseOutput`].
    #[must_use]
    pub fn with_pose_out(self, output: Box<dyn PoseOutput>) -> Self {
        let mut inner = self.inner.into_inner().unwrap_or_else(PoisonError::into_inner);
        inner.pose_out = Some(output);
        Self { inner: Mutex::new(inner), slot: self.slot, perform: self.perform, guided: self.guided, gestures: self.gestures }
    }

    /// With `board` as where `input.track_body` publishes: the board whose reader the performer was made with
    /// (`Performer::with_body`). Without one, that op is [`InputError::NoBodyTracking`].
    #[must_use]
    pub fn with_body(self, board: Arc<BodyBoard>) -> Self {
        let mut inner = self.inner.into_inner().unwrap_or_else(PoisonError::into_inner);
        inner.body = Some(board);
        Self { inner: Mutex::new(inner), slot: self.slot, perform: self.perform, guided: self.guided, gestures: self.gestures }
    }

    /// Starts the gesture watch over `tap` (and the hand gestures on `board`) if it isn't running. A watch that can't
    /// start leaves gestures off and the capture running.
    fn watch_gestures(&self, tap: &Arc<FrameTap>, board: Option<&Arc<BodyBoard>>) {
        let mut watch = self.gestures.lock().unwrap_or_else(PoisonError::into_inner);
        if watch.is_none() {
            *watch = GestureWatch::start(Arc::clone(tap), board.cloned(), Arc::clone(&self.perform)).ok();
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl InputHost for LiveInput {
    fn select(&self, source: SourceKind, mode: Mode) -> Result<SourceKind, InputError> {
        let mut inner = self.inner();
        let before = inner.source;
        if mode == Mode::Apply && source != before {
            inner.source = source;
            if inner.preview.is_on() {
                inner.stop(&self.slot);
            }
        }
        Ok(before)
    }

    fn bind_camera(&self, camera: Option<CameraId>, mode: Mode) -> Result<Option<CameraId>, InputError> {
        let mut inner = self.inner();
        if let Some(id) = camera.as_ref().filter(|id| !inner.has_camera(id)) {
            return Err(InputError::no_camera(id.clone()));
        }
        let before = inner.camera.clone();
        if mode == Mode::DryRun || camera == before {
            return Ok(before);
        }
        inner.camera.clone_from(&camera);
        // A running phone capture does not read the camera: rebinding it changes nothing until the webcam runs.
        if inner.preview.is_on() && inner.source.uses_camera() {
            match camera {
                Some(id) => {
                    if let Err(error) = inner.start(id, &self.slot) {
                        // The new camera would not open: keep the old binding, and the old camera running if it
                        // still opens, so a refused change changes nothing.
                        inner.camera.clone_from(&before);
                        if let Some(old) = before {
                            let _ = inner.start(old, &self.slot);
                        }
                        return Err(error);
                    }
                }
                None => inner.stop(&self.slot),
            }
        }
        Ok(before)
    }

    fn preview(&self, on: bool, mode: Mode) -> Result<bool, InputError> {
        let mut inner = self.inner();
        let before = inner.preview.is_on();
        self.slot.collect();
        if on == before {
            return Ok(before);
        }
        if !on {
            if mode == Mode::Apply {
                inner.stop(&self.slot);
            }
            return Ok(before);
        }
        let (device, id) = inner.startable()?;
        if mode == Mode::Apply {
            inner.start_on(device, id, &self.slot)?;
            self.watch_gestures(&inner.tap, inner.body.as_ref());
        }
        Ok(before)
    }

    fn request_camera(&self) -> Permission {
        let mut inner = self.inner();
        let answer = inner.permission.request();
        inner.refused = answer == Permission::Denied;
        answer
    }

    fn state(&self) -> InputState {
        let inner = self.inner();
        self.slot.collect();
        let cameras: Vec<CameraEntry> = inner
            .tracker
            .devices()
            .into_iter()
            .map(|info| CameraEntry { bound: inner.camera.as_ref() == Some(&info.id), id: info.id, name: info.name })
            .collect();
        InputState {
            source: inner.source,
            camera: inner.camera.clone(),
            camera_present: cameras.iter().any(|entry| entry.bound),
            preview: inner.preview.clone(),
            permission: inner.permission(),
            cameras,
            tracking: self.perform.status().tracking,
        }
    }

    fn diagnoses(&self) -> Vec<Diagnosis> {
        // A stopped tracker finds nothing, so the running one, if any, is the one that answers.
        let inner = self.inner();
        let mut found = inner.tracker.diagnoses();
        if let Some(phone) = &inner.phone {
            found.extend(phone.diagnoses());
        }
        found
    }

    fn recheck(&self) {
        let inner = self.inner();
        inner.tracker.recheck();
        if let Some(phone) = &inner.phone {
            phone.recheck();
        }
    }

    fn calibrate_guided(&self, mode: Mode) -> Result<GuidedStatus, InputError> {
        let tap = {
            let inner = self.inner();
            if !inner.preview.is_on() {
                return Err(InputError::CaptureOff);
            }
            Arc::clone(&inner.tap)
        };
        if self.guided.status().is_running() {
            return Err(InputError::CalibrationRunning);
        }
        if mode == Mode::DryRun {
            return Ok(self.guided.status());
        }
        let base = self.perform.requested_calibration();
        match self.guided.start(tap, base, Arc::clone(&self.perform)) {
            Ok(true) => Ok(self.guided.status()),
            Ok(false) => Err(InputError::CalibrationRunning),
            Err(e) => Err(InputError::Device(TrackError::Backend(format!("the calibration thread: {e}")))),
        }
    }

    fn cancel_guided(&self) -> bool {
        self.guided.cancel()
    }

    fn guided(&self) -> GuidedStatus {
        self.guided.status()
    }

    #[allow(clippy::cast_precision_loss)] // frame counts as a share
    fn health(&self) -> InputHealth {
        let findings = self.diagnoses().into_iter().map(|d| d.finding).collect();
        let inner = self.inner();
        if !inner.preview.is_on() {
            return InputHealth::default();
        }
        let phone = inner.phone.as_ref().filter(|_| inner.source == SourceKind::Iphone);
        let (stats, on_phone) = (phone.map_or_else(|| inner.tracker.latency(), |phone| phone.latency()), phone.is_some());
        drop(inner);
        let ms = |d: std::time::Duration| d.as_secs_f32() * 1e3;
        let came = stats.frames + stats.dropped;
        InputHealth {
            running: true,
            source_fps: self.perform.status().source_fps,
            latency_ms_p50: ms(stats.p50),
            latency_ms_p95: ms(stats.p95),
            latency_ms_max: ms(stats.max),
            frames: stats.frames,
            dropped: stats.dropped,
            dropped_share: if came > 0 { stats.dropped as f32 / came as f32 } else { 0.0 },
            budget: (!on_phone).then(|| {
                BudgetUse::new(BudgetKey::TrackFace, f64::from(ms(stats.p95)), TRACK_FACE.0 * 1e3, false)
            }),
            findings,
        }
    }

    fn set_pose_out(&self, target: Option<SocketAddr>, mode: Mode) -> Result<PoseOutStatus, InputError> {
        let mut inner = self.inner();
        let output = inner.pose_out.as_mut().ok_or(InputError::NoPoseOutput)?;
        let before = output.status();
        if mode == Mode::DryRun || before.target == target {
            return Ok(before);
        }
        if let Some(target) = target {
            let tap = output.start(target).map_err(|e| InputError::PoseOutput(e.to_string()))?;
            if let Err(e) = self.perform.set_pose_tap(Some(tap)) {
                output.stop();
                return Err(InputError::PoseOutput(e.to_string()));
            }
        } else {
            // A tap left in place after a refused request only fills a slot nobody sends from.
            self.perform.set_pose_tap(None).ok();
            output.stop();
        }
        Ok(before)
    }

    fn pose_out(&self) -> PoseOutStatus {
        self.inner().pose_out.as_ref().map(|output| output.status()).unwrap_or_default()
    }

    fn gestures(&self) -> Vec<Gesture> {
        self.gestures.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(GestureWatch::active).unwrap_or_default()
    }

    fn hand_gestures(&self) -> Vec<HandGesture> {
        self.gestures.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(GestureWatch::hands).unwrap_or_default()
    }

    fn track_body(&self, config: BodyConfig, mode: Mode) -> Result<BodyConfig, InputError> {
        let mut inner = self.inner();
        let board = inner.body.clone().ok_or(InputError::NoBodyTracking)?;
        let before = inner.tracker.body_status().config;
        if mode == Mode::DryRun || config == before {
            return Ok(before);
        }
        // Every source that can track a body is told, so the body follows whichever runs: the webcam's own tracker,
        // or a mocap app over VMC (PF-04). Each publishes only while its frames come.
        if let Some(phone) = inner.phone.as_mut() {
            match phone.track_body(config, &board) {
                Ok(_) | Err(TrackError::Unsupported(_)) => {}
                Err(other) => return Err(InputError::Device(other)),
            }
        }
        inner.tracker.track_body(config, &board).map_err(|e| match e {
            TrackError::Unsupported(_) => InputError::NoBodyTracking,
            other => InputError::Device(other),
        })
    }

    fn body(&self) -> BodyStatus {
        let inner = self.inner();
        if inner.body.is_none() {
            return BodyStatus::default();
        }
        match (inner.source, inner.phone.as_ref()) {
            (SourceKind::Iphone, Some(phone)) => phone.body_status(),
            _ => inner.tracker.body_status(),
        }
    }
}

impl std::fmt::Debug for LiveInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveInput").finish_non_exhaustive()
    }
}
