//! `NetTracker` end to end over loopback UDP: a test socket plays the phone, the tracker's source is polled as the
//! stage polls it. Waits are bounded polls on a condition (the frame arriving), never a sleep that orders the test.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Channel, FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::{Clock, Seconds, SystemClock};
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use omuzaki_track_net::protocol::live_link::{self, VALUES};
use omuzaki_track_net::protocol::osc::{Arg, Writer};
use omuzaki_track_net::{NetTracker, PhoneSettings, Protocol};

/// Long enough for a loaded CI machine; a passing run takes milliseconds.
const PATIENCE: Duration = Duration::from_secs(10);

fn tracker(settings: PhoneSettings) -> NetTracker {
    NetTracker::new(settings, Arc::new(SystemClock::new()))
}

/// Every protocol on an OS-picked port, so tests never collide with each other or a real app.
fn any_ports() -> PhoneSettings {
    Protocol::ALL.into_iter().fold(PhoneSettings::default(), |s, p| s.with_port(p, 0))
}

fn device(protocol: Protocol) -> CameraId {
    CameraId(protocol.device_id().to_owned())
}

/// Polls `source` until a frame satisfying `want` arrives.
fn wait_for(source: &mut dyn FrameSource, want: impl Fn(&TrackFrame) -> bool) -> TrackFrame {
    let clock = SystemClock::new();
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Some(frame) = source.poll(clock.now())
            && want(&frame)
        {
            return frame;
        }
        std::thread::yield_now();
    }
    panic!("no matching frame within {PATIENCE:?}");
}

fn target(tracker: &NetTracker) -> SocketAddr {
    let port = tracker.local_addr().expect("running").port();
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[test]
fn every_protocol_is_listed_as_a_device() {
    let ids: Vec<_> = tracker(any_ports()).devices().into_iter().map(|d| d.id.0).collect();
    assert_eq!(ids, ["net:ifacialmocap", "net:vtube_studio", "net:live_link_face", "net:vmc"]);
}

#[test]
fn live_link_face_frames_arrive_with_their_drops_counted() {
    let mut tracker = tracker(any_ports());
    let mut source = tracker.start(&device(Protocol::LiveLinkFace), &TrackerConfig::default()).unwrap();
    let phone = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut values = [0.0; VALUES];
    for (frame, jaw) in [(10, 0.1), (11, 0.2), (13, 0.4)] {
        values[17] = jaw; // jawOpen
        phone.send_to(&live_link::encode("phone", "face", frame, 60, &values), target(&tracker)).unwrap();
        let got = wait_for(&mut *source, |f| (f.get(Channel::JAW_OPEN) - jaw).abs() < 1e-6);
        assert_eq!(got.status, TrackStatus::Tracking);
        assert!(got.head.is_some());
    }
    let stats = tracker.latency();
    assert_eq!(stats.frames, 3);
    assert_eq!(stats.dropped, 1, "frame 12 never came");
    assert!(stats.p95 < Duration::from_secs(1), "{stats:?}");
}

#[test]
fn vmc_bundles_drive_the_head_and_garbage_is_counted_not_fatal() {
    let mut tracker = tracker(any_ports());
    let mut source = tracker.start(&device(Protocol::Vmc), &TrackerConfig::default()).unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.send_to(b"not osc at all", target(&tracker)).unwrap();
    let q = omuzaki_core::glam::Quat::from_rotation_y(-0.25);
    let bundle = Writer::bundle(&[
        Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str("EyeBlinkRight"), Arg::Float(0.9)]),
        Writer::message(
            "/VMC/Ext/Bone/Pos",
            &[
                Arg::Str("Head"),
                Arg::Float(0.0),
                Arg::Float(1.5),
                Arg::Float(0.0),
                Arg::Float(q.x),
                Arg::Float(q.y),
                Arg::Float(q.z),
                Arg::Float(q.w),
            ],
        ),
        Writer::message("/VMC/Ext/Blend/Apply", &[]),
    ]);
    sender.send_to(&bundle, target(&tracker)).unwrap();
    let frame = wait_for(&mut *source, |f| f.present.contains(Channel::EYE_BLINK_RIGHT));
    assert!((frame.get(Channel::EYE_BLINK_RIGHT) - 0.9).abs() < 1e-6);
    assert!((frame.head.unwrap().yaw - 0.25).abs() < 1e-5);
    assert_eq!(tracker.refused(), 1, "the garbage datagram arrived first and was refused");
}

#[test]
fn silence_publishes_one_lost_frame_and_stop_ends_the_source() {
    let mut tracker = tracker(any_ports());
    let config = TrackerConfig::default().with_lost_after(Duration::from_millis(30));
    let mut source = tracker.start(&device(Protocol::LiveLinkFace), &config).unwrap();
    let phone = UdpSocket::bind("127.0.0.1:0").unwrap();
    phone.send_to(&live_link::encode("phone", "face", 1, 60, &[0.5; VALUES]), target(&tracker)).unwrap();
    let seen = wait_for(&mut *source, |f| f.status == TrackStatus::Tracking);
    let lost = wait_for(&mut *source, |f| f.status == TrackStatus::Lost);
    assert_eq!(lost.seq, seen.seq, "a lost frame stands for no sender frame");
    let port = tracker.local_addr().unwrap().port();
    tracker.stop();
    assert!(source.poll(Seconds(1e9)).is_none());
    assert!(tracker.local_addr().is_none());
    // The port is released: binding it again succeeds.
    UdpSocket::bind(("0.0.0.0", port)).unwrap();
}

#[test]
fn a_phone_protocol_without_the_phone_address_refuses_to_start() {
    let mut tracker = tracker(any_ports());
    for protocol in [Protocol::Ifacialmocap, Protocol::VtubeStudio] {
        assert!(tracker.start(&device(protocol), &TrackerConfig::default()).is_err(), "{protocol:?}");
    }
    assert!(tracker.start(&CameraId("net:nope".into()), &TrackerConfig::default()).is_err());
}

#[test]
fn a_busy_port_is_reported_as_busy() {
    let held = UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let mut tracker = tracker(any_ports().with_port(Protocol::Vmc, port));
    let error = tracker.start(&device(Protocol::Vmc), &TrackerConfig::default()).err().unwrap();
    assert_eq!(error, omuzaki_track_api::error::TrackError::Busy(device(Protocol::Vmc)));
}

/// The two protocols that ask the phone first, with the phone on a second loopback address (Linux routes all of
/// 127/8 to loopback; macOS does not, so this runs on Linux and Windows CI).
#[cfg(any(target_os = "linux", windows))]
mod handshakes {
    use super::*;

    const PHONE: std::net::Ipv4Addr = std::net::Ipv4Addr::new(127, 0, 0, 2);

    fn phone_settings() -> PhoneSettings {
        any_ports().with_address(Some(PHONE.into()))
    }

    #[test]
    fn vtube_studio_asks_for_data_on_its_own_port_and_ignores_other_senders() {
        let Ok(phone) = UdpSocket::bind((PHONE, omuzaki_track_net::protocol::vtube_studio::PORT)) else {
            eprintln!("skipped: the VTube Studio port is in use on this machine");
            return;
        };
        phone.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut tracker = tracker(phone_settings());
        let mut source = tracker.start(&device(Protocol::VtubeStudio), &TrackerConfig::default()).unwrap();
        let mut buf = [0u8; 1024];
        let (len, from) = phone.recv_from(&mut buf).unwrap();
        let request: serde_json::Value = serde_json::from_slice(&buf[..len]).unwrap();
        assert_eq!(request["messageType"], "iOSTrackingDataRequest");
        assert_eq!(request["ports"][0], from.port(), "the request names the port it was sent from");

        // A stranger on another address is ignored; the phone is heard.
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fake = br#"{"FaceFound": true, "BlendShapes": [{"k": "JawOpen", "v": 1.0}]}"#;
        stranger.send_to(fake, (PHONE, from.port())).unwrap();
        let real = br#"{"Timestamp": 5000, "FaceFound": true, "BlendShapes": [{"k": "JawOpen", "v": 0.25}]}"#;
        phone.send_to(real, (PHONE, from.port())).unwrap();
        let frame = wait_for(&mut *source, |f| f.present.contains(Channel::JAW_OPEN));
        assert!((frame.get(Channel::JAW_OPEN) - 0.25).abs() < 1e-6, "the stranger's frame got through");
        assert_eq!(tracker.refused(), 0);
    }

    #[test]
    fn ifacialmocap_sends_its_handshake_and_decodes_the_reply() {
        let Ok(phone) = UdpSocket::bind((PHONE, omuzaki_track_net::protocol::ifacialmocap::PORT)) else {
            eprintln!("skipped: the iFacialMocap port is in use on this machine");
            return;
        };
        phone.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut tracker = tracker(phone_settings());
        let mut source = tracker.start(&device(Protocol::Ifacialmocap), &TrackerConfig::default()).unwrap();
        let mut buf = [0u8; 256];
        let (len, from) = phone.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], omuzaki_track_net::protocol::ifacialmocap::HANDSHAKE);
        phone.send_to(b"eyeBlink_R-50|jawOpen-20|=head#0,15,0,0,0,-0.5|", from).unwrap();
        let frame = wait_for(&mut *source, |f| f.present.contains(Channel::EYE_BLINK_RIGHT));
        assert!((frame.get(Channel::EYE_BLINK_RIGHT) - 0.5).abs() < 1e-6);
        assert!((frame.head.unwrap().yaw - 15f32.to_radians()).abs() < 1e-6);
    }
}
