//! The tracking input (PF-01; `docs/design/first-run.md` §4, §5): which kind of source drives the face, which camera
//! it reads, whether the live mirror runs, and the camera permission. The `input.*` ops ask an [`InputHost`]; the App
//! injects [`LiveInput`], which starts and stops a [`Tracker`] and hands each new frame source to the perform thread
//! through a [`SourceSlot`].
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

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_anim_api::{FrameSource, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Mode;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

impl Setting for InputCamera {
    type Value = Option<CameraId>;
    const META: SettingMeta = SettingMeta::new(
        "input.camera",
        "The camera the webcam source reads",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Option<CameraId> {
        None
    }
}

register_setting!(InputCamera);

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
    /// This build has no receiver for the kind of source (the iPhone link is PF-02).
    #[error("this build has no {0:?} input yet")]
    Unsupported(SourceKind),
    /// The camera refused to start.
    #[error("the camera could not start: {0}")]
    Device(TrackError),
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

/// The input's own state, behind the host's lock.
struct Inner {
    tracker: Box<dyn Tracker>,
    permission: Box<dyn CameraPermission>,
    config: TrackerConfig,
    source: SourceKind,
    camera: Option<CameraId>,
    preview: PreviewState,
    /// A camera refused access since the last prompt, whatever the OS reported before opening it.
    refused: bool,
}

impl Inner {
    /// Whether the tracker lists `camera` now.
    fn has_camera(&self, camera: &CameraId) -> bool {
        self.tracker.devices().iter().any(|info| info.id == *camera)
    }

    /// What starting the mirror on the current choice would need, checked without starting anything: the camera to
    /// open.
    fn startable(&self) -> Result<CameraId, InputError> {
        match self.source {
            SourceKind::LifeLayer => return Err(InputError::NothingToCapture),
            SourceKind::Iphone => return Err(InputError::Unsupported(SourceKind::Iphone)),
            SourceKind::Webcam => {}
        }
        let camera = self.camera.clone().ok_or(InputError::NoCameraBound)?;
        if !self.has_camera(&camera) {
            return Err(InputError::no_camera(camera));
        }
        Ok(camera)
    }

    /// Starts the tracker on `camera` and posts its source. On a refusal the mirror is off and the performer reads
    /// nothing.
    fn start(&mut self, camera: CameraId, slot: &SourceSlot) -> Result<(), InputError> {
        match self.tracker.start(&camera, &self.config) {
            Ok(source) => {
                slot.post(source);
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

    /// Stops the tracker and hands the performer a source that delivers nothing. Idempotent.
    fn stop(&mut self, slot: &SourceSlot) {
        self.tracker.stop();
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
            permission,
            config,
            source: SourceKind::default(),
            camera: None,
            preview: PreviewState::Off,
            refused: false,
        };
        Self { inner: Mutex::new(inner), slot, perform }
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
        if inner.preview.is_on() {
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
        let camera = inner.startable()?;
        if mode == Mode::Apply {
            inner.start(camera, &self.slot)?;
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
}

impl std::fmt::Debug for LiveInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveInput").finish_non_exhaustive()
    }
}
