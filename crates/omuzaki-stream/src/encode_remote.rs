//! The remote encoder (#1224; `docs/design/supervisor.md` §15.5, `docs/design/record.md` D11): an [`Encoder`] whose
//! encoding happens in a supervised encoder worker, so a crashing codec or driver takes down that worker and nothing
//! else. The ring and the take writers stay in the host and outlive it.
//!
//! - **[`EncoderPeer`]** is the host side of every generation: it makes each generation's fresh [`FrameSlots`] and
//!   [`PacketChannel`] (§4.3), keeps the host's ends, and takes the worker's `Opened` (its [`StreamSpec`]) and
//!   `SlotFree` messages. Give it to the worker's `ProcessLauncher`.
//! - **[`RemoteEncoders`]** is the [`EncoderFactory`] over the supervised worker: `create` waits for a generation newer
//!   than the last one it handed out to be up and opened, and returns a [`RemoteEncoder`] over that generation's
//!   objects. A recorder or the replay buffer recreates its encoder from it after a failure (D11) and nothing else
//!   changes.
//! - **[`RemoteEncoder`]**: `send` copies the frame into a slot the worker has handed back and announces it
//!   (`Frame { slot, index, pts }`), [`StreamError::Full`] when none is free (D2); `receive` drains the packet
//!   channel, side data included; `request_keyframe` goes out before the next frame. Commands go to its own generation
//!   only, so a frame never reaches a successor that doesn't have its slot. A dead or hung worker (the supervisor took
//!   it down) surfaces as [`StreamError::Encode`].
//! - **Nothing is lost by a crash:** every packet carries its frame's index as `pts`, so after a failure the frames
//!   announced whose packets never arrived are [`unfinished`](Encoder::unfinished), copied back out of the dead
//!   generation's slots (the host still maps them), oldest first, for the next encoder.
//!
//! Unix only for now: on Windows the worker's control pipe is not built yet, and the peer refuses to start.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_shm::{FrameSlots, PacketChannel, Share};
use omuzaki_stream_api::encoder::{EncodeConfig, Encoder, EncoderFactory, EncoderInfo, Frame, StreamSpec};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::Packet;
use omuzaki_stream_api::worker::{
    ChannelHandle, ChannelNames, ChannelRef, ChannelRole, HostMsg, StartParams, WorkerMsg,
};

use crate::watchdog::launch::{LaunchError, Peer, Start};
use crate::watchdog::{SendError, Supervisor, WorkerId};

/// The frames in flight to an encoder worker: its slot count.
pub const DEFAULT_DEPTH: u32 = 4;

/// How long `create` waits for a generation to be up and opened: a restart is a spawn, `Hello`, `Start` and the
/// codec's open.
pub const OPEN_WAIT: Duration = Duration::from_secs(5);

/// How long `flush` waits for the packets of the frames still in the worker.
const FLUSH_WAIT: Duration = Duration::from_secs(2);

/// Generations the peer keeps after they end (an encoder may still hold an older one).
const KEPT: usize = 4;

/// The packet channel's room past the largest packet a frame can make (headers and side data).
const PACKET_SLACK: usize = 64 * 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the worker of one generation has said.
#[derive(Debug, Default)]
struct Said {
    spec: Option<StreamSpec>,
    /// Slots the host may write: all of them at first, then each as the worker hands it back.
    free: VecDeque<u32>,
    gone: Option<String>,
}

/// One generation's host side.
#[derive(Debug)]
struct Generation {
    number: u32,
    slots: FrameSlots,
    packets: PacketChannel,
    said: Mutex<Said>,
}

/// The host side of an encoder worker's generations (give it to the worker's launcher as its [`Peer`]).
#[derive(Debug)]
pub struct EncoderPeer {
    config: EncodeConfig,
    depth: u32,
    slot_bytes: usize,
    /// The lowest frame index whose packet has not reached the host: the next generation's `first_index`.
    next_index: AtomicU64,
    generations: Mutex<VecDeque<Arc<Generation>>>,
    opened: Condvar,
}

impl EncoderPeer {
    /// The peer of an encoder worker for `config`, with `depth` frames in flight (at least 1).
    ///
    /// # Errors
    /// [`StreamError::Config`] for a frame size whose RGBA bytes don't fit a slot.
    pub fn new(config: EncodeConfig, depth: u32) -> Result<Self, StreamError> {
        let size = config.video.size;
        let slot_bytes = usize::try_from(u64::from(size.x) * u64::from(size.y) * 4)
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| StreamError::Config(format!("a {}x{} frame has no slot size", size.x, size.y)))?;
        Ok(Self {
            config,
            depth: depth.max(1),
            slot_bytes,
            next_index: AtomicU64::new(0),
            generations: Mutex::new(VecDeque::with_capacity(KEPT + 1)),
            opened: Condvar::new(),
        })
    }

    /// The configuration its workers are started with.
    pub fn config(&self) -> &EncodeConfig {
        &self.config
    }

    fn generation(&self, number: u32) -> Option<Arc<Generation>> {
        lock(&self.generations).iter().find(|g| g.number == number).cloned()
    }

    /// Waits until `deadline` for the newest generation after `after` that has opened and is the live one.
    fn wait_open(
        &self,
        after: u32,
        sup: &Supervisor,
        worker: WorkerId,
        deadline: Instant,
    ) -> Result<(Arc<Generation>, StreamSpec), StreamError> {
        let mut generations = lock(&self.generations);
        loop {
            let live = sup.live_generation(worker).map_err(|e| StreamError::Config(e.to_string()))?;
            let ready = generations.iter().rev().filter(|g| g.number > after && Some(g.number) == live).find_map(|g| {
                let spec = lock(&g.said).spec.clone()?;
                Some((Arc::clone(g), spec))
            });
            if let Some(ready) = ready {
                return Ok(ready);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(StreamError::Encode(format!(
                    "the encoder worker did not open within {:.1} s",
                    OPEN_WAIT.as_secs_f64()
                )));
            }
            // Wake on `Opened`, and look again at least every 10 ms for the supervisor's `Start`.
            let wait = (deadline - now).min(Duration::from_millis(10));
            generations = self.opened.wait_timeout(generations, wait).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// The host's handle for a share, sent to the worker with `Start`.
// Fallible on the platforms without the control pipe yet.
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))]
#[cfg_attr(not(unix), allow(clippy::needless_pass_by_value))]
fn handle(share: Share) -> Result<ChannelHandle, LaunchError> {
    #[cfg(unix)]
    {
        Ok(share.into_handle())
    }
    #[cfg(not(unix))]
    {
        let _ = share;
        Err(LaunchError::permanent("the remote encoder needs Unix here (the Windows control pipe is not built yet)"))
    }
}

impl Peer for EncoderPeer {
    fn prepare(&self, generation: u32) -> Result<Start, LaunchError> {
        let objects = |e: omuzaki_shm::ShmError| LaunchError::spawn(e.to_string());
        let (slots, slots_share) = FrameSlots::create(self.depth, self.slot_bytes).map_err(objects)?;
        let capacity = self.slot_bytes.saturating_mul(2).saturating_add(PACKET_SLACK);
        let (packets, packets_share) = PacketChannel::create(capacity).map_err(objects)?;
        let handles = vec![handle(slots_share)?, handle(packets_share)?];
        let said = Said { free: (0..self.depth).collect(), ..Said::default() };
        let mut generations = lock(&self.generations);
        generations.push_back(Arc::new(Generation { number: generation, slots, packets, said: Mutex::new(said) }));
        while generations.len() > KEPT {
            generations.pop_front();
        }
        drop(generations);
        let channels = ChannelNames::new(
            generation,
            vec![(ChannelRole::Frames, ChannelRef::fd(0)), (ChannelRole::Packets, ChannelRef::fd(1))],
        );
        let params = StartParams::encoder(self.config.clone(), self.next_index.load(Ordering::Acquire));
        Ok(Start { channels, handles, params })
    }

    fn message(&self, generation: u32, msg: WorkerMsg) {
        let Some(g) = self.generation(generation) else { return };
        match msg {
            WorkerMsg::Opened { spec, .. } => {
                lock(&g.said).spec = Some(spec);
                // Taking the lock orders the wake-up after a waiter's check.
                let _generations = lock(&self.generations);
                self.opened.notify_all();
            }
            WorkerMsg::SlotFree { slot, .. } => {
                let mut said = lock(&g.said);
                // A slot it doesn't have, or one already free, is a broken worker's: ignored.
                if slot < self.depth && !said.free.contains(&slot) {
                    said.free.push_back(slot);
                }
            }
            WorkerMsg::Gone { reason, .. } => lock(&g.said).gone = Some(reason),
            _ => {}
        }
    }
}

/// The [`EncoderFactory`] over a supervised encoder worker.
#[derive(Debug)]
pub struct RemoteEncoders {
    sup: Arc<Supervisor>,
    worker: WorkerId,
    peer: Arc<EncoderPeer>,
    open_wait: Duration,
    /// The newest generation handed out.
    last: Mutex<u32>,
}

impl RemoteEncoders {
    /// Encoders from worker `worker` of `sup`, whose launcher was given `peer`.
    pub fn new(sup: Arc<Supervisor>, worker: WorkerId, peer: Arc<EncoderPeer>) -> Self {
        Self { sup, worker, peer, open_wait: OPEN_WAIT, last: Mutex::new(0) }
    }
}

impl EncoderFactory for RemoteEncoders {
    fn probe(&self) -> Vec<EncoderInfo> {
        // The worker probes its own encoders; the host knows only the one it was started for.
        Vec::new()
    }

    fn create(&self, cfg: &EncodeConfig) -> Result<Box<dyn Encoder>, StreamError> {
        if *cfg != self.peer.config {
            return Err(StreamError::Config("the encoder worker was started for another configuration".into()));
        }
        let mut last = lock(&self.last);
        let deadline = Instant::now() + self.open_wait;
        let (generation, spec) = self.peer.wait_open(*last, &self.sup, self.worker, deadline)?;
        *last = generation.number;
        Ok(Box::new(RemoteEncoder {
            sup: Arc::clone(&self.sup),
            worker: self.worker,
            peer: Arc::clone(&self.peer),
            generation,
            spec,
            announced: VecDeque::with_capacity(self.peer.depth as usize),
            keyframe: false,
            stopped: false,
            buf: Vec::with_capacity(8),
        }))
    }
}

/// A frame the worker was given and has not answered.
#[derive(Debug)]
struct Announced {
    index: u64,
    slot: u32,
    size: UVec2,
    alpha: AlphaEncoding,
}

/// An encoder over one generation of a supervised encoder worker.
#[derive(Debug)]
pub struct RemoteEncoder {
    sup: Arc<Supervisor>,
    worker: WorkerId,
    peer: Arc<EncoderPeer>,
    generation: Arc<Generation>,
    spec: StreamSpec,
    /// Announced frames without a packet yet, oldest first.
    announced: VecDeque<Announced>,
    /// A keyframe was asked for and goes out before the next frame.
    keyframe: bool,
    stopped: bool,
    buf: Vec<Packet>,
}

impl RemoteEncoder {
    /// Its generation of the worker.
    pub fn generation(&self) -> u32 {
        self.generation.number
    }

    fn live(&self) -> bool {
        matches!(self.sup.live_generation(self.worker), Ok(Some(n)) if n == self.generation.number)
    }

    fn dead(&self) -> StreamError {
        let gone = lock(&self.generation.said).gone.clone();
        let why = gone.map_or_else(|| "has stopped".to_owned(), |reason| format!("said: {reason}"));
        StreamError::Encode(format!("the encoder worker (generation {}) {why}", self.generation.number))
    }

    fn command(&self, msg: &HostMsg) -> Result<(), StreamError> {
        self.sup.send_to(self.worker, self.generation.number, msg).map_err(|e| match e {
            SendError::Down => self.dead(),
            other => StreamError::Encode(other.to_string()),
        })
    }
}

/// Whether a take continues in the same file after an encoder restart: the new encoder's codec parameters are
/// byte-identical to the old one's (`docs/design/record.md` D6); otherwise it rolls to the next part.
pub fn same_part(old: &StreamSpec, new: &StreamSpec) -> bool {
    old.codec == new.codec && old.video == new.video && old.audio == new.audio && old.extradata == new.extradata
}

impl Encoder for RemoteEncoder {
    fn spec(&self) -> StreamSpec {
        self.spec.clone()
    }

    fn send(&mut self, frame: &Frame<'_>) -> Result<(), StreamError> {
        if self.stopped {
            return Err(StreamError::Stopped);
        }
        let image =
            frame.cpu.ok_or_else(|| StreamError::Config("the remote encoder takes frames on the CPU".into()))?;
        if image.rgba8.len() != self.generation.slots.slot_bytes() {
            return Err(StreamError::Config(format!(
                "a {}x{} frame for a {} byte slot",
                image.size.x,
                image.size.y,
                self.generation.slots.slot_bytes()
            )));
        }
        if !self.live() {
            return Err(self.dead());
        }
        let slot = lock(&self.generation.said).free.pop_front().ok_or(StreamError::Full)?;
        let announced = self.generation.slots.write(slot, &image.rgba8).map_err(StreamError::from).and_then(|()| {
            if self.keyframe {
                self.command(&HostMsg::Keyframe)?;
            }
            let pts = i64::try_from(frame.index).unwrap_or(i64::MAX);
            self.command(&HostMsg::frame(slot, frame.index, pts))
        });
        if let Err(e) = announced {
            // Not announced: the slot is still the host's.
            lock(&self.generation.said).free.push_front(slot);
            return Err(e);
        }
        self.keyframe = false;
        self.announced.push_back(Announced { index: frame.index, slot, size: image.size, alpha: image.alpha });
        Ok(())
    }

    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        self.buf.clear();
        // A channel that fails its checks is a broken worker: an encoder failure, which restarts it.
        let n = self.generation.packets.recv(&mut self.buf).map_err(|e| StreamError::Encode(e.to_string()))?;
        for packet in self.buf.drain(..) {
            let answered =
                u64::try_from(packet.pts).ok().and_then(|index| self.announced.iter().position(|a| a.index == index));
            let Some(at) = answered else {
                return Err(StreamError::Encode(format!(
                    "the encoder worker sent a packet for frame {}, which it was not given",
                    packet.pts
                )));
            };
            let done = self.announced.remove(at).map_or(0, |a| a.index + 1);
            let next = self.announced.front().map_or(done, |a| a.index);
            self.peer.next_index.fetch_max(next, Ordering::AcqRel);
            out.push(packet);
        }
        if n == 0 && !self.announced.is_empty() && !self.live() {
            return Err(self.dead());
        }
        Ok(())
    }

    fn request_keyframe(&mut self) {
        self.keyframe = true;
    }

    fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        let deadline = Instant::now() + FLUSH_WAIT;
        loop {
            self.receive(out)?;
            if self.announced.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(StreamError::Encode(format!(
                    "the encoder worker left {} frames unanswered",
                    self.announced.len()
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.stopped = true;
        Ok(())
    }

    fn unfinished(&mut self) -> Vec<(u64, Image)> {
        let slots = &self.generation.slots;
        self.announced
            .drain(..)
            .filter_map(|a| {
                // The host wrote the slot and the worker only read it, so its bytes are the frame's.
                let bytes = slots.read(a.slot).ok()?;
                Image::new(a.size, a.alpha, bytes).ok().map(|image| (a.index, image))
            })
            .collect()
    }
}
