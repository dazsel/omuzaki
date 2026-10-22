//! The replay ops (#1021): start the buffer into the configured folder, save a clip, read the status, stop; the errors
//! say what to do. Runs the real VP9 alpha path, so it skips, saying so, on a machine without libvpx.

// Integration-test helpers may panic: a panic is a clear test failure. The test reads its own temporary folder.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Mode, OpCtx, OpError, Services, ServicesBuilder, find};
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt as _};
use omuzaki_stream::replay::settings::Dir;
use omuzaki_stream::service::{Program, PushOutcome, StreamService};
use omuzaki_stream_api::time::Ticks;
use serde_json::{Value, json};

const SIZE: UVec2 = UVec2::new(64, 36);
const FRAME_NS: i64 = 1_000_000_000 / 30;

fn try_call(name: &str, args: Value, services: &Services, dry_run: bool) -> Result<Value, OpError> {
    omuzaki_stream::link();
    let op = (find(name).expect("registered").decode)(args).unwrap();
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx).map(|plan| plan.output.expect("the op answers"))
}

fn call(name: &str, args: Value, services: &Services) -> Value {
    try_call(name, args, services, false).unwrap_or_else(|e| panic!("{name}: {} ({:?})", e.message, e.hint))
}

fn picture() -> Image {
    let mut rgba8 = Vec::new();
    for _ in 0..SIZE.x * SIZE.y {
        rgba8.extend_from_slice(&[120, 60, 30, 160]);
    }
    Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8 }
}

/// Waits, with a deadline, until `done` holds.
fn wait(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::yield_now();
    }
}

#[test]
fn the_replay_ops_start_save_report_and_stop() {
    if let Err(e) = omuzaki_stream::vp9::available() {
        eprintln!("skipped: {e}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(StreamService::new().unwrap());
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    let settings = Arc::new(omuzaki_settings_mock::MemorySettings::default());
    let person = Actor::Person { name: "host".into() };
    settings.set::<Dir>(&dir.path().to_string_lossy().into_owned(), Layer::Base, Mode::Apply, &person).unwrap();
    let services =
        ServicesBuilder::new().insert::<StreamService>(service.clone()).insert::<dyn Settings>(settings).build();

    assert_eq!(call("record.replay_status", json!({}), &services)["running"], false);
    let off = try_call("record.save_replay", json!({}), &services, false).unwrap_err();
    assert_eq!((off.code, off.hint.as_deref()), (ErrorCode::Conflict, Some("start it with record.replay_start")));
    let dry = try_call("record.replay_start", json!({ "seconds": 5 }), &services, true).unwrap();
    assert_eq!((dry["started"].clone(), dry["available"].clone()), (json!(false), json!(true)));
    assert_eq!(call("record.replay_status", json!({}), &services)["running"], false, "a dry run starts nothing");
    let bad = try_call("record.replay_start", json!({ "seconds": 400 }), &services, false).unwrap_err();
    assert_eq!(bad.code, ErrorCode::Invalid);

    let started = call("record.replay_start", json!({ "seconds": 5 }), &services);
    assert_eq!((started["started"].clone(), started["length_s"].clone()), (json!(true), json!(5.0)));
    let again = try_call("record.replay_start", json!({}), &services, false).unwrap_err();
    assert_eq!(again.code, ErrorCode::Conflict);

    let frame = picture();
    for n in 0..240u64 {
        assert_eq!(service.push(&frame, Ticks(n as i64 * FRAME_NS)), PushOutcome::Queued);
        // A real source sends the next frame a frame period later; here it comes once this one is encoded.
        wait("the encoder", || service.replay_status().frames > n);
    }
    let status = call("record.replay_status", json!({}), &services);
    assert!(status["buffered_s"].as_f64().unwrap() >= 5.0, "{status}");
    assert_eq!(status["dropped_frames"], 0, "{status}");

    let saved = call("record.save_replay", json!({ "seconds": 5 }), &services);
    let path = std::path::PathBuf::from(saved["path"].as_str().unwrap());
    assert!(path.exists() && path.starts_with(dir.path()), "{saved}");
    assert!(path.extension().is_some_and(|e| e == "webm"));
    assert!((5.0..=6.1).contains(&saved["actual_s"].as_f64().unwrap()), "{saved}");
    let info = omuzaki_stream::sinks::webm::read(&std::fs::read(&path).unwrap()).unwrap();
    assert!(info.frames[0].key && info.frames.iter().all(|f| f.alpha.is_some()));
    assert_eq!(call("record.replay_status", json!({}), &services)["last_save"]["path"], saved["path"]);

    let stopped = call("record.replay_stop", json!({}), &services);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(call("record.replay_status", json!({}), &services)["running"], false);
    assert_eq!(try_call("record.replay_stop", json!({}), &services, false).unwrap_err().code, ErrorCode::Conflict);
}
