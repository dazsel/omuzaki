//! The fakes behave as the contract says a real runtime, session, slot and stream must: registrations are found,
//! probing and loading follow the slot signature and `io` renames, refusals and crashes surface as errors, smoke
//! tests separate a good model from a bad one, and a stream misses exactly the late blocks and then stalls.

// The helpers below are test code too; clippy only exempts `#[test]` functions themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use omuzaki_ai_api::{
    Block, DType, EvalEnv, EvalError, EvalLevel, FileSource, LoadError, LocalFile, ModelManifest, ModelSource, Origin,
    Progress, RunError, Session, SessionOptions, Slot, SlotMode, StreamSpec, TensorBuf, TensorRef, find_runtime,
    find_slot, shipped_models,
};
use omuzaki_ai_mock::{
    ECHO_MODEL, ECHO_MODEL_URL, EchoSlot, FakeSession, InlineStream, echo_signature, fake_model, fixtures_fetcher,
};

/// Fixtures are never needed by the echo slots.
struct NoEnv;
impl FileSource for NoEnv {
    fn get(
        &self,
        pin: &omuzaki_ai_api::PinnedFile,
        _: &dyn Progress,
    ) -> Result<std::path::PathBuf, omuzaki_ai_api::FetchError> {
        Err(omuzaki_ai_api::FetchError::Network { url: pin.urls.join(" "), reason: "no fixtures in this test".into() })
    }
}
impl EvalEnv for NoEnv {
    fn files(&self) -> &dyn FileSource {
        self
    }
    fn shipped(&self, slot: &omuzaki_ai_api::SlotRef) -> Result<Box<dyn Session>, EvalError> {
        Err(EvalError::Needs(slot.clone()))
    }
}

fn load(dir: &Path, model_toml: &str, io: &BTreeMap<String, String>) -> Result<Box<dyn Session>, LoadError> {
    let path = dir.join("model.fake.toml");
    std::fs::write(&path, model_toml).unwrap();
    let files = [LocalFile { name: "model.fake.toml".into(), path }];
    let model = fake_model();
    let source = ModelSource { model: &model, files: &files, stages: &[], natives: &NoEnv };
    let runtime = find_runtime(&"fake".parse().unwrap()).expect("the fake runtime is registered");
    runtime.probe(&source)?;
    runtime.load(&source, &echo_signature(), io, &SessionOptions::default())
}

#[test]
fn the_mock_registrations_are_found_and_the_shipped_manifest_is_valid() {
    let echo = find_slot(&"test_echo@1".parse().unwrap()).expect("test_echo@1 is registered");
    assert!(matches!(echo.info().mode, SlotMode::Call { .. }));
    let stream = find_slot(&"test_stream@1".parse().unwrap()).expect("test_stream@1 is registered");
    assert!(matches!(stream.info().mode, SlotMode::Stream { queue: 2, stall_blocks: 4 }));

    let shipped = shipped_models().find(|m| m.id == "echo-fake").expect("echo-fake is registered");
    let manifest = ModelManifest::from_toml(shipped.manifest).unwrap();
    assert_eq!(manifest.id.as_str(), shipped.id);
    assert_eq!(manifest.validate(Origin::Shipped), Ok(()));
    let file = &manifest.files[0];
    assert_eq!((file.urls.as_slice(), file.size), (&[ECHO_MODEL_URL.to_owned()][..], ECHO_MODEL.len() as u64));
}

#[test]
fn the_fixtures_fetcher_serves_ranges_counts_and_goes_offline() {
    use omuzaki_ai_api::{FetchError, Fetcher};
    use std::io::Read;
    let read = |body: omuzaki_ai_api::Body| {
        let mut text = String::new();
        let mut reader = body.reader;
        reader.read_to_string(&mut text).unwrap();
        (body.start, text)
    };
    let fetcher = fixtures_fetcher();
    assert_eq!(read(fetcher.open(ECHO_MODEL_URL, 0).unwrap()), (0, ECHO_MODEL.to_owned()));
    assert_eq!(read(fetcher.open(ECHO_MODEL_URL, 10).unwrap()), (10, ECHO_MODEL[10..].to_owned()), "a range resumes");
    fetcher.set_ignore_ranges(true);
    assert_eq!(read(fetcher.open(ECHO_MODEL_URL, 10).unwrap()).0, 0, "a server without ranges starts over");
    assert!(matches!(fetcher.open("https://models.invalid/nothing", 0), Err(FetchError::Network { .. })));
    fetcher.set_offline(true);
    assert!(fetcher.open(ECHO_MODEL_URL, 0).is_err());
    assert_eq!(fetcher.requests(), 5);
}

#[test]
fn the_identity_passes_the_smoke_test_and_a_wrong_model_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut good = load(dir.path(), ECHO_MODEL, &BTreeMap::new()).unwrap();
    assert!(EchoSlot.evaluate(good.as_mut(), &NoEnv, EvalLevel::Smoke).unwrap().passed());
    let doubled = ECHO_MODEL.replace("scale = 1.0", "scale = 2.0");
    let mut bad = load(dir.path(), &doubled, &BTreeMap::new()).unwrap();
    let report = EchoSlot.evaluate(bad.as_mut(), &NoEnv, EvalLevel::Smoke).unwrap();
    assert!(!report.passed());
    assert_eq!(report.failures().next().map(|c| c.metric.value), Some(1.0));
}

#[test]
fn renamed_ports_load_through_the_io_table_and_refusals_surface() {
    let dir = tempfile::tempdir().unwrap();
    let renamed = ECHO_MODEL.replace("name = \"x\"", "name = \"in\"").replace("name = \"y\"", "name = \"out\"");
    assert!(matches!(load(dir.path(), &renamed, &BTreeMap::new()), Err(LoadError::Format(_))));
    let io = BTreeMap::from([("x".to_owned(), "in".to_owned()), ("y".to_owned(), "out".to_owned())]);
    assert!(load(dir.path(), &renamed, &io).is_ok());
    let refused = format!("refuse = \"custom op domain com.example\"\n{ECHO_MODEL}");
    assert_eq!(load(dir.path(), &refused, &io).err(), Some(LoadError::Refused("custom op domain com.example".into())));
}

#[test]
fn sessions_check_calls_and_crash_on_cue() {
    let mut session = FakeSession::affine(echo_signature(), 1.0, 0.0).crashing_after(1);
    let mut out = TensorBuf::for_outputs(session.signature());
    let x = [1.0f32, 2.0];
    assert!(matches!(session.run(&[], &mut out), Err(RunError::Mismatch { .. })));
    session.run(&[TensorRef::f32(&x, &[2])], &mut out).unwrap();
    assert_eq!(out[0].as_f32(), Some(&x[..]));
    assert_eq!(session.run(&[TensorRef::f32(&x, &[2])], &mut out), Err(RunError::Crashed));
    assert_eq!(session.runs(), 1);

    let mut wrong_type = vec![TensorBuf::new(DType::U8)];
    assert!(matches!(session.run(&[TensorRef::f32(&x, &[2])], &mut wrong_type), Err(RunError::Mismatch { .. })));
}

/// Pushes `blocks` blocks of `[seq; 4]` and takes each one `queue` blocks later, as a voice stage does; returns what
/// each take found.
fn play(late: fn(u64) -> bool, blocks: u64) -> (Vec<Block>, omuzaki_ai_api::StreamStats) {
    let spec = StreamSpec { sample_rate: 16_000, block: 4, latency: 8 };
    let session = Box::new(FakeSession::affine(echo_signature(), 1.0, 0.0));
    let (mut sink, mut source) = InlineStream::late_when(session, spec, 2, 4, late);
    assert_eq!(source.latency(), spec.added_latency(2));
    let mut out = TensorBuf::for_outputs(&echo_signature());
    let mut found = Vec::new();
    for seq in 0..blocks {
        #[allow(clippy::cast_precision_loss)]
        let block = [seq as f32; 4];
        assert_eq!(sink.push(&[TensorRef::f32(&block, &[4])]).unwrap(), seq);
        if let Some(due) = seq.checked_sub(u64::from(source.queue())) {
            let taken = source.take(due, &mut out);
            #[allow(clippy::cast_precision_loss)]
            if taken == Block::Ready {
                assert_eq!(out[0].as_f32(), Some(&[due as f32; 4][..]), "block {due} is its own output");
            }
            found.push(taken);
        }
    }
    (found, source.stats())
}

#[test]
fn a_stream_misses_exactly_the_late_blocks() {
    let (found, stats) = play(|seq| seq % 3 == 1, 11);
    let missed: Vec<usize> = found.iter().enumerate().filter(|(_, b)| **b == Block::Missed).map(|(i, _)| i).collect();
    assert_eq!(missed, [1, 4, 7]);
    assert_eq!((stats.pushed, stats.ready, stats.missed, stats.stalled), (11, 6, 3, false));
}

#[test]
fn a_stream_stalls_after_too_many_misses_in_a_row_and_stays_dry() {
    let (found, stats) = play(|seq| (3..7).contains(&seq), 14);
    assert!(found[..3].iter().all(|b| *b == Block::Ready));
    assert!(found[3..].iter().all(|b| *b == Block::Missed), "{found:?}");
    assert!(stats.stalled);
}

#[test]
fn a_delay_session_delays_by_exactly_its_latency_and_resets() {
    let mut session = FakeSession::delay(echo_signature(), 3);
    let mut out = TensorBuf::for_outputs(session.signature());
    session.run(&[TensorRef::f32(&[1.0, 2.0], &[2])], &mut out).unwrap();
    assert_eq!(out[0].as_f32(), Some(&[0.0, 0.0][..]));
    session.run(&[TensorRef::f32(&[3.0, 4.0], &[2])], &mut out).unwrap();
    assert_eq!(out[0].as_f32(), Some(&[0.0, 1.0][..]), "sample n comes out at n + 3");
    session.reset();
    session.run(&[TensorRef::f32(&[5.0, 6.0], &[2])], &mut out).unwrap();
    assert_eq!(out[0].as_f32(), Some(&[0.0, 0.0][..]), "reset clears the line");
}

#[test]
fn a_block_of_the_wrong_size_is_refused() {
    let spec = StreamSpec { sample_rate: 16_000, block: 4, latency: 0 };
    let session = Box::new(FakeSession::affine(echo_signature(), 1.0, 0.0));
    let (mut sink, _) = InlineStream::open(session, spec, 2, 4);
    assert!(matches!(sink.push(&[TensorRef::f32(&[0.0; 3], &[3])]), Err(RunError::Mismatch { .. })));
}
