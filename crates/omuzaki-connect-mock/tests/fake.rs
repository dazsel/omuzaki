//! `FakeConnector` (#1452): prices, the cap, cancellation, scripted failures, latency through the fake clock, the PNG
//! outputs and the registrations.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::atomic::Ordering;

use common::{manifest, run, run_with};
use omuzaki_connect_api::{
    ConnectError, Connector, ConnectorConfig, ImageRequest, Output, Request, Target, registered,
};
use omuzaki_connect_mock::{FakeConnector, Script};
use omuzaki_core::time::{Clock as _, Seconds};

fn images(count: u8, seed: Option<u64>) -> Request {
    Request::Image(ImageRequest {
        prompt: "fox".into(),
        width: 100,
        height: 40,
        count,
        seed,
        ..ImageRequest::default()
    })
}

fn assets(outputs: &[Output]) -> Vec<Vec<u8>> {
    outputs
        .iter()
        .map(|o| match o {
            Output::Asset(a) => a.bytes.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn four_paid_images_cost_what_was_estimated() {
    let m = manifest("mock-image-paid");
    let fake = FakeConnector::image(Script::priced(40_000));
    let counters = fake.counters();
    let target = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
    let estimate = fake.estimate(&m, &target, &images(4, Some(1))).unwrap();
    let ran = run(&fake, &m, &images(4, Some(1)));
    let out = ran.result.unwrap();
    assert_eq!((out.usage.usd_micros, estimate.usd_micros, ran.meter.total()), (160_000, 160_000, 160_000));
    let bytes = assets(&out.outputs);
    assert_eq!(bytes.len(), 4);
    assert!(bytes.iter().enumerate().all(|(i, b)| bytes[i + 1..].iter().all(|other| other != b)), "four distinct");
    let again = run(&fake, &m, &images(4, Some(1))).result.unwrap();
    assert_eq!(assets(&again.outputs), bytes, "the same request gives byte-equal outputs");
    assert_eq!(counters.http_calls.load(Ordering::SeqCst), 0);
    assert_eq!((counters.runs.load(Ordering::SeqCst), counters.estimates.load(Ordering::SeqCst)), (2, 1));
}

#[test]
fn the_cap_stops_before_the_output_that_would_pass_it() {
    let m = manifest("mock-image-paid");
    let fake = FakeConnector::image(Script::priced(40_000));
    let out = run_with(&fake, &m, &images(4, None), 100_000, u32::MAX).result.unwrap();
    assert_eq!((out.outputs.len(), out.usage.usd_micros), (2, 80_000));
    assert_eq!(out.stopped, Some(ConnectError::cap_reached(100_000, 120_000)));
    let none = run_with(&fake, &m, &images(4, None), 10_000, u32::MAX);
    assert_eq!(none.result, Err(ConnectError::cap_reached(10_000, 40_000)));
    assert_eq!(none.meter.charges(), Vec::<u64>::new());
}

#[test]
fn a_cancel_keeps_what_is_complete() {
    let m = manifest("mock-image");
    let fake = FakeConnector::image(Script { latency_s: 1.0, ..Script::default() });
    let out = run_with(&fake, &m, &images(4, None), u64::MAX, 2).result.unwrap();
    assert_eq!((out.outputs.len(), out.stopped), (2, Some(ConnectError::Cancelled)));
    assert_eq!(run_with(&fake, &m, &images(4, None), u64::MAX, 0).result, Err(ConnectError::Cancelled));
}

#[test]
fn a_scripted_failure_keeps_earlier_outputs() {
    let m = manifest("mock-image");
    let auth = ConnectError::auth("check the key");
    let fake = FakeConnector::image(Script { fail: Some((2, auth.clone())), ..Script::default() });
    let out = run(&fake, &m, &images(4, None)).result.unwrap();
    assert_eq!((out.outputs.len(), out.stopped), (2, Some(auth.clone())));
    let first = FakeConnector::image(Script { fail: Some((0, auth.clone())), ..Script::default() });
    assert_eq!(run(&first, &m, &images(4, None)).result, Err(auth));
}

#[test]
fn a_refusing_meter_stops_after_the_charge_it_refused() {
    let m = manifest("mock-image-paid");
    let fake = FakeConnector::image(Script::priced(40_000));
    let target = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
    let http = omuzaki_http_mock::FakeHttp::new();
    let inputs = omuzaki_connect_mock::MemoryInputs::new();
    let meter = omuzaki_connect_mock::RecordingMeter::capped(50_000);
    let env = omuzaki_connect_api::RunEnv {
        progress: &omuzaki_ai_api::NoProgress,
        cancel: std::sync::Arc::new(omuzaki_http_api::NeverCancel),
        meter: &meter,
    };
    let clock = omuzaki_core::time::FakeClock::new(Seconds(0.0));
    let pace = omuzaki_connect_mock::FakePace::default();
    let cx = omuzaki_connect_api::RunCx::new(&target, &http, &inputs, &env, &pace, &clock, u64::MAX);
    let out = fake.run(&m, &images(4, None), &cx).unwrap();
    // The second charge passes the meter's cap: that output was billed, so it is kept, and nothing more is asked.
    assert_eq!((out.outputs.len(), out.usage.usd_micros, meter.charges()), (2, 80_000, vec![40_000, 40_000]));
    assert!(matches!(out.stopped, Some(ConnectError::CapReached { .. })));
}

#[test]
fn latency_advances_the_fake_clock() {
    let m = manifest("mock-image");
    let fake = FakeConnector::image(Script { latency_s: 2.0, ..Script::default() });
    let ran = run(&fake, &m, &images(2, None));
    assert_eq!((ran.clock.now(), ran.waits), (Seconds(4.0), 2));
}

#[test]
fn images_are_pngs_at_the_clamped_size() {
    let m = manifest("mock-image");
    let out = run(&FakeConnector::image(Script::default()), &m, &images(1, Some(5))).result.unwrap();
    let Output::Asset(a) = &out.outputs[0] else { panic!() };
    let decoded = image::load_from_memory(&a.bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (64, 40));
    assert_eq!((a.media_type.as_str(), a.model.as_str(), a.seed), ("image/png", "mock-image@1", Some(5)));
    let other = run(&FakeConnector::image(Script::default()), &m, &images(1, Some(6))).result.unwrap();
    assert_ne!(assets(&out.outputs), assets(&other.outputs), "different seeds, different bytes");
}

#[test]
fn an_undeclared_capability_is_unsupported() {
    let m = manifest("mock-image");
    let req = omuzaki_connect_mock::conformance::sample_request(omuzaki_connect_api::Capability::Music);
    assert!(matches!(
        run(&FakeConnector::music(Script::default()), &m, &req).result,
        Err(ConnectError::Unsupported { .. })
    ));
}

#[test]
fn the_registrations_are_found_and_validate() {
    omuzaki_connect_mock::link();
    let mut ids: Vec<&str> = registered().map(|r| r.id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "mock-3d",
            "mock-dcc",
            "mock-image",
            "mock-image-paid",
            "mock-motion",
            "mock-music",
            "mock-texture",
            "mock-voice"
        ]
    );
    for r in registered() {
        let m = r.parse().unwrap();
        assert_eq!(m.validate(), Ok(()), "{}", r.id);
        assert_eq!(m.licence.register, "Omuzaki mock connector");
        assert_eq!(m.licence.commercial, omuzaki_store_api::Commercial::Ok);
        let _fresh = (r.make)();
    }
    let paid = manifest("mock-image-paid");
    assert!(paid.paid() && paid.privacy.leaves_machine && paid.licence.requires_plan == ["paid"]);
}
