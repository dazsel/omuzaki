//! Every channel across two real processes (`docs/design/supervisor.md` §15.3): the objects travel as descriptors over
//! a socketpair that is the child's stdin, with the worker protocol's own `write_msg_fds`. The child is this test
//! binary run again with `OMUZAKI_SHM_CHILD` naming its role.
//!
//! - every type round-trips both ways;
//! - a producer killed mid-write (`SIGKILL` after a torn write) never yields a torn record: `Latest::read` and
//!   `SampleRing::clock` return `None` within the bounded tries, in under 1 ms, and keep returning it, while a new
//!   generation's objects work at once;
//! - a withheld (muted or private) run puts no sample value in shared memory: the whole mapping is scanned for a
//!   marker the child was given as muted audio.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use omuzaki_shm::{ClockMapping, FrameSlots, LATEST_READ_TRIES, Latest, PacketChannel, RunInfo, SampleRing, Share};
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};
use omuzaki_stream_api::worker::{
    ChannelNames, ChannelRef, ChannelRole, HostMsg, StartParams, WorkerMsg, read_msg, read_msg_fds, write_msg,
    write_msg_fds,
};

const ROLE: &str = "OMUZAKI_SHM_CHILD";
/// A value no real audio has, fed to the child as muted audio.
const MARKER: f32 = 1_234.567_8;

fn fd(share: &Share) -> BorrowedFd<'_> {
    let Share::Fd(fd) = share;
    fd.as_fd()
}

/// This binary again, as the child `role`, its stdin one end of a socketpair; the other end is returned.
fn spawn(role: &str) -> (Child, UnixStream) {
    let (host, child) = UnixStream::pair().unwrap();
    let process = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .stdin(Stdio::from(OwnedFd::from(child)))
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    (process, host)
}

fn start(sock: &UnixStream, roles: &[ChannelRole], shares: &[&Share]) {
    let entries = roles.iter().enumerate().map(|(i, r)| (*r, ChannelRef::fd(u16::try_from(i).unwrap()))).collect();
    let fds: Vec<BorrowedFd<'_>> = shares.iter().map(|s| fd(s)).collect();
    write_msg_fds(sock, &HostMsg::start(ChannelNames::new(1, entries), StartParams::Audio), &fds).unwrap();
}

fn wait_for(sock: &mut UnixStream) -> WorkerMsg {
    read_msg::<_, WorkerMsg>(sock, &mut Vec::new()).unwrap().expect("the child said nothing")
}

fn packet() -> Packet {
    Packet {
        stream: StreamId(0),
        pts: 41,
        dts: 41,
        duration: 1,
        keyframe: true,
        data: Bytes::from_static(b"frame bytes"),
        side_data: vec![SideData {
            kind: SideKind::BlockAdditional { id: 1 },
            data: Bytes::from_static(b"alpha plane"),
        }],
    }
}

/// The child: runs its role and exits. A no-op when the test runs in the parent.
#[test]
fn child() {
    let Ok(role) = std::env::var(ROLE) else { return };
    let stdin = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let mut sock = UnixStream::from(stdin);
    let (msg, fds) = read_msg_fds::<HostMsg>(&sock, &mut Vec::new()).unwrap().unwrap();
    assert!(matches!(msg, HostMsg::Start { .. }));
    let mut shares = fds.into_iter().map(Share::Fd);
    match role.as_str() {
        "echo" => {
            let slots = FrameSlots::open(shares.next().unwrap()).unwrap();
            let latest = Latest::open(shares.next().unwrap()).unwrap();
            let ring = SampleRing::open(shares.next().unwrap()).unwrap();
            let packets = PacketChannel::open(shares.next().unwrap()).unwrap();
            // What the host wrote, back through every channel.
            let frame = slots.read(0).unwrap();
            slots.write(1, &frame.iter().rev().copied().collect::<Vec<_>>()).unwrap();
            latest.publish(&frame).unwrap();
            assert_eq!(ring.push(48_000, &[0.25, -0.25, 0.5, -0.5]), 2);
            assert_eq!(ring.push_withheld(48_002, 3, true, false), 3);
            ring.publish_clock(ClockMapping { sample: 48_000, at_ns: 7, seconds_per_sample: 1.0 / 48_000.0 });
            assert!(packets.send(&packet()).unwrap());
            write_msg(&mut sock, &WorkerMsg::gone("echoed")).unwrap();
        }
        "torn" => {
            let latest = Latest::open(shares.next().unwrap()).unwrap();
            let ring = SampleRing::open(shares.next().unwrap()).unwrap();
            latest.publish(b"whole").unwrap();
            ring.publish_clock(ClockMapping { sample: 1, at_ns: 2, seconds_per_sample: 0.5 });
            latest.publish_torn(b"never finished").unwrap();
            ring.publish_clock_torn(ClockMapping { sample: 9, at_ns: 9, seconds_per_sample: 9.0 });
            write_msg(&mut sock, &WorkerMsg::gone("torn")).unwrap();
            // Wait to be killed.
            std::thread::sleep(Duration::from_secs(30));
        }
        "withhold" => {
            let ring = SampleRing::open(shares.next().unwrap()).unwrap();
            let muted = vec![MARKER; 2 * 64];
            // The engine knows the run is muted: only its length and flags are pushed, never these values.
            assert_eq!(ring.push_withheld(0, muted.len() / 2, true, false), 64);
            assert_eq!(ring.push_withheld(64, 64, false, true), 64);
            assert_eq!(ring.push(128, &[0.1; 8]), 4);
            write_msg(&mut sock, &WorkerMsg::gone("withheld")).unwrap();
        }
        other => panic!("unknown role {other}"),
    }
}

#[test]
fn every_channel_round_trips_across_processes() {
    let (slots, s1) = FrameSlots::create(2, 64).unwrap();
    let (latest, s2) = Latest::create(64).unwrap();
    let (ring, s3) = SampleRing::create(2, 256).unwrap();
    let (packets, s4) = PacketChannel::create(4096).unwrap();
    slots.write(0, b"host frame").unwrap();
    let (mut child, mut sock) = spawn("echo");
    start(
        &sock,
        &[ChannelRole::Frames, ChannelRole::LiveInputs, ChannelRole::Audio, ChannelRole::Packets],
        &[&s1, &s2, &s3, &s4],
    );
    drop((s1, s2, s3, s4));
    assert_eq!(wait_for(&mut sock), WorkerMsg::gone("echoed"));
    assert!(child.wait().unwrap().success());
    assert_eq!(slots.read(1).unwrap(), b"emarf tsoh");
    let mut out = [0; 64];
    let (_, len) = latest.read(&mut out).unwrap();
    assert_eq!(&out[..len], b"host frame");
    let mut samples = [0.0_f32; 16];
    assert_eq!(ring.pop(&mut samples), RunInfo { len: 2, start_sample: 48_000, ..RunInfo::default() });
    assert_eq!(&samples[..4], &[0.25, -0.25, 0.5, -0.5]);
    assert_eq!(ring.pop(&mut samples), RunInfo { len: 3, start_sample: 48_002, muted: true, ..RunInfo::default() });
    assert_eq!(ring.clock().unwrap().sample, 48_000);
    let mut got = Vec::new();
    assert_eq!(packets.recv(&mut got).unwrap(), 1);
    assert_eq!(got, vec![packet()], "the side data crossed byte for byte");
}

#[test]
fn a_producer_killed_mid_write_never_yields_a_torn_record() {
    let (latest, s1) = Latest::create(64).unwrap();
    let (ring, s2) = SampleRing::create(1, 64).unwrap();
    let (mut child, mut sock) = spawn("torn");
    start(&sock, &[ChannelRole::LiveInputs, ChannelRole::Audio], &[&s1, &s2]);
    drop((s1, s2));
    assert_eq!(wait_for(&mut sock), WorkerMsg::gone("torn"));
    child.kill().unwrap();
    child.wait().unwrap();
    let mut out = [0; 64];
    for _ in 0..100 {
        let began = Instant::now();
        assert_eq!(latest.read(&mut out), None);
        assert_eq!(ring.clock(), None);
        assert!(began.elapsed() < Duration::from_millis(1), "{} tries took {:?}", LATEST_READ_TRIES, began.elapsed());
    }
    // The next generation's objects work at once.
    let (fresh, _share) = Latest::create(64).unwrap();
    fresh.publish(b"next").unwrap();
    assert_eq!(fresh.read(&mut out).map(|(_, len)| len), Some(4));
}

#[test]
fn a_withheld_run_puts_no_sample_value_in_shared_memory() {
    let (ring, share) = SampleRing::create(2, 256).unwrap();
    let (mut child, mut sock) = spawn("withhold");
    start(&sock, &[ChannelRole::Audio], &[&share]);
    drop(share);
    assert_eq!(wait_for(&mut sock), WorkerMsg::gone("withheld"));
    assert!(child.wait().unwrap().success());
    let marker = MARKER.to_le_bytes();
    let mapped = ring.snapshot();
    assert!(!mapped.windows(4).any(|w| w == marker), "a muted sample value reached shared memory");
    let mut out = [9.0_f32; 256];
    let muted = ring.pop(&mut out);
    assert!(muted.muted && muted.len == 64 && out[..128].iter().all(|v| *v == 0.0));
    let private = ring.pop(&mut out);
    assert!(private.private && private.len == 64);
    assert_eq!(ring.pop(&mut out).len, 4);
}
