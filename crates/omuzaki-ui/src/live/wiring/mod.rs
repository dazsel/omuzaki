//! The live slice's wiring (#312): the tracker, the perform thread, the newest-wins hand-off and the render side.
//!
//! ```text
//!  camera ─► Tracker (its own thread) ─► FrameSource ─► SourceSlot ─► perform thread ─► FrameMailbox ─► render thread
//!              ▲ owned by LiveInput                                   Performer + PerformControl     LiveScene: Stage::tick
//!          ops (input.*) ─► Arc<dyn InputHost>                             ▲                          ├─► main viewport
//!          ops (perform.*, trigger.*) ─► Arc<dyn PerformHost> (the PerformHandle)                    └─► output window
//! ```
//!
//! # Thread ownership
//!
//! | Thread | Owns | Shares |
//! |---|---|---|
//! | tracker (the [`Tracker`](omuzaki_track_api::tracker::Tracker)'s own) | the camera stream | the [`FrameSource`](omuzaki_anim_api::FrameSource) it returns, through its mailbox |
//! | perform ([`PerformThread`], named [`THREAD_NAME`]) | the `Performer` (with its source), the `PerformControl`, the [`FrameWriter`] | nothing it locks: atomics and bounded queues only |
//! | render (the UI's) | the [`LiveScene`] (the `Stage`), the renderer, the output window | a [`FrameReader`] |
//! | op executor | nothing of the loop | `Arc<dyn PerformHost>` (a `PerformHandle` clone) and `Arc<dyn InputHost>` (the `LiveInput`) in its services |
//!
//! The `Arc`s: the [`PerformThread`] and every `PerformHandle` clone share the control's record; the writer and
//! every reader share the mailbox; the thread and the `LiveInput` share the `SourceSlot`; [`LiveRig`] owns the
//! [`PerformThread`] and shares the `LiveInput` (which owns the tracker, behind its own lock) with the runner.
//!
//! # Quit order
//!
//! 1. The render thread stops drawing and releases its stage ([`LiveScene::release`]); its readers drop.
//! 2. [`LiveRig::shutdown`] stops the mirror, so the tracker releases the camera; the source it handed out goes quiet.
//! 3. It then stops the perform thread and joins it ([`PerformThread::stop`]). The performer and its source drop on
//!    that thread; the ops' handles answer `Disconnected` from then on.
//! 4. The runner drops its services, the last `PerformHandle` clones and the `LiveInput`.
//!
//! Dropping the rig runs steps 2 and 3, so an early return can't leave a thread behind.
//!
//! # Lifecycle
//!
//! The performer starts on the stage's `NoSource`, so the life layer runs before any camera does. The `input.*` ops
//! drive the stage's `LiveInput` ([`LiveRig::input`]), which owns the tracker: binding a camera and starting the
//! mirror posts its source into the perform thread's slot ([`PerformThread::sources`]), which the loop swaps in at its
//! next pass without locking or allocating. A camera that is unplugged stops delivering frames: after the performer's
//! `STALE_SOURCE_S` it reports the face lost and the life layer takes the face over, with no jump. Retry is
//! `input.preview` on again.

pub mod app;
pub mod clock;
pub mod mailbox;
pub mod rig;
pub mod scene;
pub mod thread;

pub use app::{LiveApp, NoTracker, StartError};
pub use clock::{PerformClock, RealClock};
pub use mailbox::{FrameReader, FrameWriter, LiveFrame, frame_mailbox};
pub use rig::{LIFE_SEED, LiveRig, live_performer};
pub use scene::LiveScene;
pub use thread::{LoopReport, PerformThread, THREAD_NAME, WiringError};
