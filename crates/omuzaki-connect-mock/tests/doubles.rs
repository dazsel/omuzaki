//! The run context's doubles (#997): `FakePace`, `MemoryConfig`, `MemoryInputs` and `RecordingMeter`. `FakeHttp`
//! moved to `omuzaki-http-mock` (design D13), whose own tests cover routes, cassettes, hangs and cancellation.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use omuzaki_ai_api::NoProgress;
use omuzaki_connect_api::{ConfigSource, ConnectError, ConnectorConfig, ConnectorId, Inputs, Meter, Pace};
use omuzaki_connect_mock::{FakePace, MemoryConfig, MemoryInputs, RecordingMeter};
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_store_api::ContentHash;

#[test]
fn fake_pace_advances_the_clock_exactly() {
    let clock = Arc::new(FakeClock::new(Seconds(1.0)));
    let pace = FakePace::new(Arc::clone(&clock));
    pace.wait(2.5, &NoProgress);
    assert_eq!((clock.now(), pace.waits.load(Ordering::SeqCst)), (Seconds(3.5), 1));
}

#[test]
fn memory_config_returns_what_was_set_and_the_default_otherwise() {
    let config = MemoryConfig::new();
    let id = ConnectorId::new("comfyui").unwrap();
    let set = ConnectorConfig { enabled: true, plan: Some("pro".into()), ..ConnectorConfig::default() };
    config.set(id.clone(), set.clone());
    assert_eq!(config.get(&id), set);
    assert_eq!(config.get(&ConnectorId::new("other").unwrap()), ConnectorConfig::default());
}

#[test]
fn memory_inputs_hand_out_only_what_they_hold() {
    let mut inputs = MemoryInputs::new();
    let hash = inputs.add(b"png bytes".to_vec(), "image/png");
    assert_eq!(hash, ContentHash::of(b"png bytes"));
    let asset = inputs.get(&hash).unwrap();
    assert_eq!((asset.bytes.as_slice(), asset.media_type.as_str()), (&b"png bytes"[..], "image/png"));
    assert!(matches!(inputs.get(&ContentHash::of(b"other")), Err(ConnectError::Refused { .. })));
}

#[test]
fn recording_meter_records_and_refuses_past_its_cap() {
    let open = RecordingMeter::new();
    open.charge(5).unwrap();
    open.charge(7).unwrap();
    assert_eq!((open.charges(), open.total()), (vec![5, 7], 12));
    let capped = RecordingMeter::capped(10);
    capped.charge(6).unwrap();
    assert_eq!(capped.charge(6), Err(ConnectError::cap_reached(10, 12)));
    assert_eq!(capped.total(), 12, "the refused charge was billed and stays on record");
}
