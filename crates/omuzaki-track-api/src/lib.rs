//! Contract for live trackers (spec v3 §8, §22 PF-01): the camera seam, the [`Tracker`] device trait, the mapping
//! from a device clock onto the stage clock, and the newest-wins [`mailbox`] that a tracker publishes into and the
//! pipeline polls through [`FrameSource`](omuzaki_anim_api::FrameSource).
//!
//! The frame types live in `omuzaki-anim-api` ([`TrackFrame`](omuzaki_anim_api::TrackFrame),
//! [`FrameSource`](omuzaki_anim_api::FrameSource), [`DeviceProfile`](omuzaki_anim_api::DeviceProfile)); this crate
//! defines no twins of them. Design: `docs/design/track-api.md`.
//!
//! ## Threads
//!
//! ```text
//!  tracker thread (one per running Tracker)                     stage thread
//!  CameraStream::next_frame ─▶ convert ─▶ infer ─▶ ClockMap::map ─▶ MailboxWriter::publish ┄┄▶ MailboxReader::poll
//!  (blocks on the device)      (pixfmt)   (ai sessions)                (wait-free)                 (wait-free)
//! ```
//!
//! - **The tracker owns its thread.** Capture, pixel conversion and inference run one after another on it; the spike
//!   measured one thread as enough, and a second only adds hand-offs. [`CameraStream::next_frame`] is the one call
//!   that may block, and only there.
//! - **The stage thread only polls.** [`MailboxReader::poll`] never blocks, never locks and never allocates.
//!
//! ## Real-time rules
//!
//! Calls marked **hot path** run once per frame and may not allocate, lock or block: [`MailboxWriter::publish`],
//! [`MailboxReader::poll`], [`ClockMap::map`], and the [`CameraFrame`] borrow. [`Camera::open`], [`Tracker::start`]
//! and the device lists are cold and may allocate.
//!
//! ## Permissions
//!
//! Which camera runs, and whether one runs at all, is a device choice: ops that call [`Camera::open`] or
//! [`Tracker::start`] are person-only (`docs/design/settings.md`, devices are `Never` for agents). The OS camera prompt
//! surfaces as [`TrackError::PermissionDenied`].
#![deny(missing_docs)]

pub mod camera;
pub mod clock;
pub mod error;
pub mod mailbox;
pub mod tracker;

pub use camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
pub use clock::{CLOCK_WINDOW, ClockError, ClockMap, MAX_DRIFT, RESYNC_GAP};
pub use error::TrackError;
pub use mailbox::{MailboxReader, MailboxWriter, mailbox};
pub use tracker::{LatencyStats, QualityTier, Tracker, TrackerConfig};
