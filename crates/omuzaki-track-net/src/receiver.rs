//! [`NetTracker`]: the receiver thread and the [`Tracker`] around it.
//!
//! ```text
//!  receiver thread (one per running session)
//!  recv_from (≤ TICK) ─▶ Decoder::decode ─▶ ClockMap::map ─▶ MailboxWriter::publish ┄┄▶ the stage's poll
//!        └─ every TICK: renew the phone's request when due; publish one lost frame after `lost_after` of silence
//! ```
//!
//! The thread owns the socket, the decoder, the clock map and the mailbox's writer, so nothing on it is shared but a
//! few atomics. It allocates its packet buffer once at start; decoding and publishing allocate nothing per packet
//! (`tests/no_alloc.rs` counts the decoders; track-api's own tests count publish and the clock map).

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_anim_api::{FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_track_api::camera::{CameraId, CameraInfo, DeviceTime};
use omuzaki_track_api::clock::{ClockError, ClockMap};
use omuzaki_track_api::error::TrackError;
use omuzaki_track_api::mailbox::{MailboxReader, MailboxWriter, mailbox};
use omuzaki_track_api::tracker::{LatencyStats, Tracker, TrackerConfig};

use crate::protocol::{Decoder, Protocol, Stamp, blank, ifacialmocap, vtube_studio};
use crate::settings::PhoneSettings;

/// How long one `recv_from` waits: the bound on how late a lost frame, a renewal or `stop` is seen.
pub const TICK: Duration = Duration::from_millis(10);

/// The largest datagram read whole. A VMC bundle of a full body is about 3 KB; a UDP datagram cannot exceed 64 KiB.
pub const MAX_DATAGRAM: usize = 65_536;

/// Latencies kept for the percentiles.
const WINDOW: usize = 128;

/// Frames between percentile updates: the sort runs on the receiver thread, off the stage's path.
const PERCENTILE_EVERY: u64 = 16;

/// Seconds without a datagram after which iFacialMocap's handshake is sent again (the phone may have restarted).
const IFACIALMOCAP_RETRY_S: f64 = 2.0;

/// What the session publishes for the health meter and `latency`, as atomics the thread writes and anyone reads.
#[derive(Debug, Default)]
struct Shared {
    frames: AtomicU64,
    dropped: AtomicU64,
    refused: AtomicU64,
    p50_ns: AtomicU64,
    p95_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl Shared {
    fn latency(&self) -> LatencyStats {
        let ns = |a: &AtomicU64| Duration::from_nanos(a.load(Ordering::Relaxed));
        LatencyStats::new(
            self.frames.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            ns(&self.p50_ns),
            ns(&self.p95_ns),
            ns(&self.max_ns),
        )
    }
}

/// The source `start` returns: the mailbox, until the session stops, then `None` for ever.
struct Source {
    reader: MailboxReader,
    live: Arc<AtomicBool>,
}

impl FrameSource for Source {
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        if self.live.load(Ordering::Relaxed) { self.reader.poll(now) } else { None }
    }
}

struct Session {
    thread: JoinHandle<()>,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
    local: SocketAddr,
}

/// The network [`Tracker`] (PF-02 to PF-04): one device per [`Protocol`] (`net:ifacialmocap`, `net:vtube_studio`,
/// `net:live_link_face`, `net:vmc`). `start` binds the protocol's UDP port on every interface and runs a receiver
/// thread until `stop`; frames are timed on the `clock` given to [`new`](Self::new), the stage's own.
///
/// Opening a port on the local network is a device choice: like a camera, only the person starts it (the
/// `input.*` ops enforce that). Once `input.phone`'s address is set, datagrams from any other address are dropped.
pub struct NetTracker {
    settings: PhoneSettings,
    clock: Arc<dyn Clock>,
    session: Option<Session>,
    shared: Arc<Shared>,
}

impl NetTracker {
    /// A stopped tracker with `settings`, timing frames on `clock`.
    pub fn new(settings: PhoneSettings, clock: Arc<dyn Clock>) -> Self {
        Self { settings, clock, session: None, shared: Arc::new(Shared::default()) }
    }

    /// Replaces the settings; a running session keeps the old ones until it is started again.
    pub fn set_settings(&mut self, settings: PhoneSettings) {
        self.settings = settings;
    }

    /// The settings in use for the next `start`.
    pub fn settings(&self) -> &PhoneSettings {
        &self.settings
    }

    /// The address the running session listens on (its port is the OS's pick when the setting says 0).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.session.as_ref().map(|s| s.local)
    }

    /// Datagrams the running (or last) session refused as not its protocol's: the tracking doctor's hint that the
    /// phone app and the chosen protocol do not match (PF-28).
    pub fn refused(&self) -> u64 {
        self.shared.refused.load(Ordering::Relaxed)
    }
}

impl Tracker for NetTracker {
    fn devices(&self) -> Vec<CameraInfo> {
        Protocol::ALL
            .into_iter()
            .map(|p| CameraInfo::new(CameraId(p.device_id().to_owned()), p.label(), Vec::new()))
            .collect()
    }

    fn start(&mut self, device: &CameraId, config: &TrackerConfig) -> Result<Box<dyn FrameSource>, TrackError> {
        self.stop();
        let protocol = Protocol::from_device_id(&device.0).ok_or_else(|| TrackError::NoDevice(device.clone()))?;
        let phone = self.settings.address;
        let handshake = match (protocol, phone) {
            (Protocol::Ifacialmocap | Protocol::VtubeStudio, None) => {
                return Err(TrackError::Backend(format!(
                    "{} needs the phone's address (input.phone)",
                    protocol.label()
                )));
            }
            (Protocol::Ifacialmocap, Some(ip)) => {
                Some((SocketAddr::new(ip, ifacialmocap::PORT), ifacialmocap::HANDSHAKE.to_vec()))
            }
            _ => None,
        };
        let socket =
            UdpSocket::bind((Ipv4Addr::UNSPECIFIED, self.settings.port(protocol))).map_err(|e| match e.kind() {
                ErrorKind::AddrInUse => TrackError::Busy(device.clone()),
                ErrorKind::PermissionDenied => TrackError::PermissionDenied,
                _ => TrackError::Backend(e.to_string()),
            })?;
        socket.set_read_timeout(Some(TICK)).map_err(|e| TrackError::Backend(e.to_string()))?;
        let local = socket.local_addr().map_err(|e| TrackError::Backend(e.to_string()))?;
        // VTube Studio's request names the port to send to: this session's own.
        let handshake = match (protocol, phone) {
            (Protocol::VtubeStudio, Some(ip)) => {
                Some((SocketAddr::new(ip, vtube_studio::PORT), vtube_studio::request(local.port())))
            }
            _ => handshake,
        };
        let (writer, reader) = mailbox();
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(AtomicBool::new(true));
        self.shared = Arc::new(Shared::default());
        let receiver = Receiver {
            socket,
            protocol,
            decoder: protocol.decoder(),
            phone,
            handshake,
            clock: Arc::clone(&self.clock),
            lost_after: config.lost_after.as_secs_f64(),
            writer,
            shared: Arc::clone(&self.shared),
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name(format!("omuzaki-track-net-{}", protocol.device_id()))
            .spawn(move || receiver.run())
            .map_err(|e| TrackError::Backend(e.to_string()))?;
        self.session = Some(Session { thread, stop, live: Arc::clone(&live), local });
        Ok(Box::new(Source { reader, live }))
    }

    fn stop(&mut self) {
        if let Some(session) = self.session.take() {
            session.live.store(false, Ordering::Relaxed);
            session.stop.store(true, Ordering::Relaxed);
            // The thread checks `stop` every TICK; a panic on it has already ended it, and is not ours to rethrow.
            let _ = session.thread.join();
        }
    }

    fn latency(&self) -> LatencyStats {
        self.shared.latency()
    }
}

impl Drop for NetTracker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Everything the receiver thread owns.
struct Receiver {
    socket: UdpSocket,
    protocol: Protocol,
    decoder: Box<dyn Decoder>,
    phone: Option<std::net::IpAddr>,
    /// Where and what to send to start the phone's stream, for the protocols that ask.
    handshake: Option<(SocketAddr, Vec<u8>)>,
    clock: Arc<dyn Clock>,
    lost_after: f64,
    writer: MailboxWriter,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
}

/// The receiver thread's running state.
struct Run {
    map: ClockMap,
    /// The newest frame published, for the lost frame's `seq` and the stale check.
    last: Option<TrackFrame>,
    /// When a tracked frame last went out (stage seconds), and whether its silence was already reported.
    last_tracked: f64,
    lost_sent: bool,
    /// When the last datagram arrived and the handshake was last sent (stage seconds).
    last_datagram: f64,
    last_handshake: f64,
    /// The sender's last frame counter, for the drop count.
    last_sender_seq: Option<u64>,
    seq: u64,
    latencies: [u32; WINDOW],
    latency_count: u64,
}

impl Receiver {
    fn run(mut self) {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut frame = blank();
        let start = self.clock.now().0;
        let mut run = Run {
            map: ClockMap::new(),
            last: None,
            last_tracked: start,
            lost_sent: true,
            last_datagram: f64::NEG_INFINITY,
            last_handshake: f64::NEG_INFINITY,
            last_sender_seq: None,
            seq: 0,
            latencies: [0; WINDOW],
            latency_count: 0,
        };
        while !self.stop.load(Ordering::Relaxed) {
            let now = self.clock.now().0;
            self.renew(&mut run, now);
            // An error is the tick's timeout, or transient for a datagram socket (an ICMP "port unreachable" from a
            // phone not listening yet surfaces here on some systems), so the loop carries on either way.
            if let Ok((len, from)) = self.socket.recv_from(&mut buf)
                && self.phone.is_none_or(|ip| ip == from.ip())
            {
                let arrived = self.clock.now();
                run.last_datagram = arrived.0;
                match self.decoder.decode(&buf[..len], &mut frame) {
                    Ok(Some(stamp)) => self.publish(&mut run, &mut frame, stamp, arrived),
                    Ok(None) => {}
                    Err(_) => {
                        self.shared.refused.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            self.check_silence(&mut run);
        }
    }

    /// Sends the handshake when it is due: VTube Studio renews its lease every second; iFacialMocap asks again after
    /// two seconds without data.
    fn renew(&self, run: &mut Run, now: f64) {
        let Some((to, bytes)) = &self.handshake else { return };
        let due = match self.protocol {
            Protocol::VtubeStudio => now - run.last_handshake >= vtube_studio::RENEW_S,
            _ => now - run.last_handshake >= IFACIALMOCAP_RETRY_S && now - run.last_datagram >= IFACIALMOCAP_RETRY_S,
        };
        if due {
            // A send can fail while the network comes up; the next renewal tries again.
            let _ = self.socket.send_to(bytes, to);
            run.last_handshake = now;
        }
    }

    fn publish(&mut self, run: &mut Run, frame: &mut TrackFrame, stamp: Stamp, arrived: Seconds) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // stage seconds are non-negative and small
        let sent = stamp.sent.unwrap_or(DeviceTime((arrived.0.max(0.0) * 1e9) as u64));
        let time = match run.map.map(sent, arrived) {
            Ok(t) => t,
            Err(ClockError::Backwards { .. }) => {
                // The sender restarted (a new clock epoch): start a new fit rather than drop every frame from now on.
                run.map.reset();
                match run.map.map(sent, arrived) {
                    Ok(t) => t,
                    Err(_) => return,
                }
            }
            Err(_) => return,
        };
        let gap = match (run.last_sender_seq, stamp.seq) {
            (Some(last), Some(seq)) if seq > last => seq - last - 1,
            _ => 0,
        };
        if stamp.seq.is_some() {
            run.last_sender_seq = stamp.seq;
        }
        run.seq += 1 + gap;
        frame.seq = run.seq;
        frame.time = time;
        self.writer.publish(frame);
        run.last = Some(*frame);
        if frame.status == TrackStatus::Tracking {
            run.last_tracked = arrived.0;
            run.lost_sent = false;
        } else {
            run.lost_sent = true;
        }
        self.shared.dropped.fetch_add(gap, Ordering::Relaxed);
        let frames = self.shared.frames.fetch_add(1, Ordering::Relaxed) + 1;
        let latency = (self.clock.now().0 - time.0).max(0.0);
        self.record(run, latency, frames);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a latency in µs, clamped to u32
    fn record(&self, run: &mut Run, latency_s: f64, frames: u64) {
        let us = (latency_s * 1e6).min(f64::from(u32::MAX)) as u32;
        run.latencies[(run.latency_count % WINDOW as u64) as usize] = us;
        run.latency_count += 1;
        self.shared.max_ns.fetch_max(u64::from(us) * 1000, Ordering::Relaxed);
        if frames % PERCENTILE_EVERY == 1 || run.latency_count <= PERCENTILE_EVERY {
            let n = run.latency_count.min(WINDOW as u64) as usize;
            let mut sorted = run.latencies;
            sorted[..n].sort_unstable();
            // Nearest rank: the value below which `percent` of the window lies.
            let at = |percent: usize| u64::from(sorted[(n - 1) * percent / 100]) * 1000;
            self.shared.p50_ns.store(at(50), Ordering::Relaxed);
            self.shared.p95_ns.store(at(95), Ordering::Relaxed);
        }
    }

    /// Publishes one lost frame once tracked frames have stopped for `lost_after`, so the stage never holds a stale
    /// face; it repeats the last frame's `seq`, since it stands for no sender frame.
    fn check_silence(&mut self, run: &mut Run) {
        let now = self.clock.now();
        if run.lost_sent || now.0 - run.last_tracked < self.lost_after {
            return;
        }
        let seq = run.last.map_or(run.seq, |f| f.seq);
        let lost = TrackFrame { seq, time: now, status: TrackStatus::Lost, ..blank() };
        self.writer.publish(&lost);
        run.last = Some(lost);
        run.lost_sent = true;
        self.shared.frames.fetch_add(1, Ordering::Relaxed);
    }
}
