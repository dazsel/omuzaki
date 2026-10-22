//! The phone source end to end (PF-02 to PF-04): a VMC sender on loopback UDP, the real `NetTracker` behind
//! `LiveInput::with_phone`, the `input.*` host choosing the iPhone source and starting it, and the performer with the
//! real `ArkitRetarget` reading the source the input posted. Waits are bounded loops on a condition, never a sleep.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "live/support.rs"]
mod support;

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Channel, HumanoidPose};
use omuzaki_core::time::SystemClock;
use omuzaki_ops_api::Mode;
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{PerformHost, perform_channel};
use omuzaki_stage::perform::input::{InputError, InputHost, LiveInput, NoPrompt, PreviewState, SourceKind, SourceSlot};
use omuzaki_track_api::tracker::TrackerConfig;
use omuzaki_track_net::protocol::osc::{Arg, Writer};
use omuzaki_track_net::{NetTracker, PhoneSettings, Protocol};
use support::{Cameras, mid_step, performer};

/// A port the OS says is free now. The tracker binds it a moment later; a collision in between fails loudly.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn vmc_frame(jaw: f32) -> Vec<u8> {
    Writer::bundle(&[
        Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str("JawOpen"), Arg::Float(jaw)]),
        Writer::message("/VMC/Ext/Blend/Apply", &[]),
    ])
}

#[test]
fn a_vmc_sender_drives_the_performer_through_the_input() {
    let port = free_port();
    let settings = PhoneSettings::default().with_protocol(Protocol::Vmc).with_port(Protocol::Vmc, port);
    let phone = NetTracker::new(settings, Arc::new(SystemClock::new()));

    let mut performer = performer(&HumanoidPose::REST);
    let (handle, mut control) = perform_channel(&performer);
    let slot = Arc::new(SourceSlot::new());
    let perform: Arc<dyn PerformHost> = Arc::new(handle.clone());
    let (cameras, _log) = Cameras::new(Vec::new());
    let input =
        LiveInput::new(Box::new(cameras), Box::new(NoPrompt), TrackerConfig::default(), Arc::clone(&slot), perform)
            .with_phone(Box::new(phone));

    input.select(SourceKind::Iphone, Mode::Apply).unwrap();
    assert!(!input.preview(true, Mode::Apply).unwrap(), "the mirror was off");
    assert_eq!(
        input.state().preview,
        PreviewState::On { camera: omuzaki_track_api::camera::CameraId("net:vmc".into()) }
    );

    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut step = 0;
    let jaw = loop {
        assert!(Instant::now() < deadline, "the performer never saw the sender's jaw");
        // Resent each round, as a sender streams: the performer reads only the newest frame.
        sender.send_to(&vmc_frame(0.8), ("127.0.0.1", port)).unwrap();
        slot.swap_into(&mut performer);
        let out = control.advance(&mut performer, mid_step(step));
        step += 1;
        if let Some(out) = out
            && out.tracking == Tracking::Tracking
            && out.bus.values[Channel::JAW_OPEN] > 0.3
        {
            break out.bus.values[Channel::JAW_OPEN];
        }
        std::thread::yield_now();
    };
    assert!(jaw <= 1.0);

    // Stopping the mirror hands the performer a silent source again; the life layer keeps the face.
    input.preview(false, Mode::Apply).unwrap();
    assert_eq!(input.state().preview, PreviewState::Off);
}

#[test]
fn without_a_phone_link_the_iphone_source_is_unsupported() {
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let (cameras, _log) = Cameras::new(Vec::new());
    let input = LiveInput::new(
        Box::new(cameras),
        Box::new(NoPrompt),
        TrackerConfig::default(),
        Arc::new(SourceSlot::new()),
        perform,
    );
    input.select(SourceKind::Iphone, Mode::Apply).unwrap();
    assert_eq!(input.preview(true, Mode::Apply), Err(InputError::Unsupported(SourceKind::Iphone)));
}
