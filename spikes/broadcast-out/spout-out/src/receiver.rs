//! The receiving half: what an app using the Spout SDK's receiver does. Connects to a sender by name, waits for its sync
//! event, reads each new frame back to the CPU, and checks it: which frame, how old, whole or torn, and are the known
//! patches right.

use std::time::{Duration, Instant};

use crate::clock::Clock;
use crate::frame_tag;
use crate::report::ReceiverReport;
use crate::spout::{SpoutError, SpoutReceiver};
use crate::tally::{Observation, Tally};

/// How long to wait for the sender to appear.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long without a new frame, once frames have flowed, before concluding the sender is gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest single wait on the sync event.
const SYNC_WAIT_MS: u32 = 20;

/// What to receive.
#[derive(Clone, Debug)]
pub struct ReceiveConfig {
    /// Sender name to connect to.
    pub name: String,
    /// Stop after seeing frame `frames - 1`.
    pub frames: u32,
    /// Record the hash of every frame whose number is a multiple of this (0 = never).
    pub hash_every: u32,
}

/// Why a receive run failed.
#[derive(Debug, thiserror::Error)]
pub enum ReceiveError {
    /// Spout failed.
    #[error(transparent)]
    Spout(#[from] SpoutError),
    /// A frame could not be decoded.
    #[error(transparent)]
    Tag(#[from] frame_tag::TagError),
    /// Direct3D refused (the raw receiver).
    #[error("{0}")]
    Direct3D(String),
    /// The sender never appeared.
    #[error("no sender called {0:?} appeared within {1:?}")]
    NoSender(String, Duration),
}

/// Receives until the last expected frame arrives or the sender goes quiet.
///
/// # Errors
/// Fails when Spout fails, a frame cannot be decoded, or the sender never appears.
pub fn run(config: &ReceiveConfig) -> Result<ReceiverReport, ReceiveError> {
    let clock = Clock::new();
    let mut receiver = SpoutReceiver::new(&config.name)?;
    let mut pixels: Vec<u8> = Vec::new();
    let mut tally = Tally::new(config.hash_every);
    let (started, mut last_new) = (Instant::now(), Instant::now());
    let mut sender_fps = 0.0;

    loop {
        // Spout's own "is this frame new" flag is unusable without a registry setting, so pace on the sync event and tell
        // frames apart by the number drawn into them. The SDK reports "signalled" when no event exists yet.
        let signalled = receiver.wait_sync(SYNC_WAIT_MS);
        let info = receiver.poll();
        let quiet = tally.received() > 0 && last_new.elapsed() > IDLE_TIMEOUT;
        if !info.connected {
            if tally.received() == 0 && started.elapsed() > CONNECT_TIMEOUT {
                return Err(ReceiveError::NoSender(config.name.clone(), CONNECT_TIMEOUT));
            }
            if quiet {
                break;
            }
            continue;
        }
        if info.updated {
            pixels = vec![0; info.width as usize * info.height as usize * 4];
            continue;
        }
        if !signalled {
            if quiet {
                break;
            }
            continue;
        }
        let seen = clock.now();
        receiver.read_pixels(&mut pixels)?;
        let read = clock.now();
        sender_fps = info.sender_fps;
        if tally.observe(&clock, &pixels, (info.width, info.height), (seen, read))? == Observation::New {
            last_new = Instant::now();
            if tally.last_frame() + 1 >= config.frames {
                break;
            }
        }
    }
    Ok(tally.finish(sender_fps))
}
