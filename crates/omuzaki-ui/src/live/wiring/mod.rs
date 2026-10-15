//! The live slice's wiring (#312): the tracker, the perform thread, the newest-wins hand-off and the render side.
//!
//! ```text
//!  camera ─► Tracker (its own thread) ─► FrameSource ─► perform thread ─► FrameMailbox ─► render thread
//!                                                       Performer + PerformControl         LiveScene: Stage::tick
//!                                                            ▲                              ├─► main viewport
//!                     ops (perform.*) ─► Arc<dyn PerformHost> (the PerformHandle)          └─► output window (a copy)
//! ```
//!
//! # Thread ownership
//!
//! | Thread | Owns | Shares |
//! |---|---|---|
//! | tracker (the [`Tracker`](omuzaki_track_api::tracker::Tracker)'s own) | the camera stream | the [`FrameSource`](omuzaki_anim_api::FrameSource) it returns, through its mailbox |
//! | perform ([`PerformThread`], named [`THREAD_NAME`]) | the `Performer` (with its source), the `PerformControl`, the [`FrameWriter`] | nothing it locks: atomics and bounded queues only |
//! | render (the UI's) | the [`LiveScene`] (the `Stage`), the renderer, the output window | a [`FrameReader`] |
//! | op executor | nothing of the loop | `Arc<dyn PerformHost>` (a `PerformHandle` clone) in its services |
//!
//! The `Arc`s: the [`PerformThread`] and every `PerformHandle` clone share the control's record; the writer and
//! every reader share the mailbox; [`LiveRig`] owns the tracker and the [`PerformThread`] (one owner each, no `Arc`).
//!
//! # Quit order
//!
//! 1. The render thread stops drawing and releases its stage ([`LiveScene::release`]); its readers drop.
//! 2. [`LiveRig::shutdown`] stops the tracker, which releases the camera; the source it handed out goes quiet.
//! 3. It then stops the perform thread and joins it ([`PerformThread::stop`]). The performer and its source drop on
//!    that thread; the ops' handles answer `Disconnected` from then on.
//! 4. The runner drops its services, the last `PerformHandle` clones.
//!
//! Dropping the rig runs steps 2 and 3, so an early return can't leave a thread behind.
//!
//! # Lifecycle
//!
//! The performer starts on [`NoCamera`], so the life layer runs before any camera does. Picking a camera hands its
//! source to the loop ([`LiveRig::use_camera`]). A camera that is unplugged stops delivering frames: after the
//! performer's `STALE_SOURCE_S` it reports the face lost and the life layer takes the face over, with no jump. Retry
//! is `use_camera` again.

pub mod clock;
pub mod mailbox;
pub mod rig;
pub mod scene;
pub mod thread;

pub use clock::{PerformClock, RealClock};
pub use mailbox::{FrameReader, FrameWriter, LiveFrame, frame_mailbox};
pub use rig::{LIFE_SEED, LiveError, LiveRig, NoCamera, live_performer};
pub use scene::LiveScene;
pub use thread::{LoopReport, PerformThread, THREAD_NAME, TriggerPress, WiringError};
