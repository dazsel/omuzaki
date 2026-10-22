//! Unit tests for what the integration tests can't reach: a slot that refuses a stream spec, the written manifest
//! and the ids and runtimes it picks.

use omuzaki_ai_api::{
    EvalEnv, EvalError, EvalLevel, EvalReport, ModelManifest, Session, Signature, Slot, SlotInfo, SlotRef, StreamSpec,
};
use omuzaki_ai_mock::StreamEchoSlot;

use super::import::Imported;
use super::manifest::{runtime_of, unique_id, written};
use super::stream_problems;

/// `test_stream@1` that, like voice conversion, refuses blocks over 100 ms.
struct Strict;

impl Slot for Strict {
    fn info(&self) -> SlotInfo {
        StreamEchoSlot.info()
    }
    fn signature(&self) -> Signature {
        StreamEchoSlot.signature()
    }
    fn check_stream(&self, spec: &StreamSpec) -> Result<(), String> {
        if spec.block_duration().as_millis() > 100 {
            Err("blocks over 100 ms add too much delay".into())
        } else {
            Ok(())
        }
    }
    fn evaluate(&self, _: &mut dyn Session, _: &dyn EvalEnv, _: EvalLevel) -> Result<EvalReport, EvalError> {
        Err(EvalError::Output("unused".into()))
    }
}

fn manifest_with(stream: Option<StreamSpec>) -> ModelManifest {
    let files = [Imported { name: "m.onnx".into(), sha256: omuzaki_ai_api::Sha256([7; 32]), size: 3 }];
    let slot: SlotRef = "test_stream@1".parse().unwrap();
    ModelManifest { stream, ..written("user-m".parse().unwrap(), &slot, "onnx".parse().unwrap(), &files) }
}

#[test]
fn a_stream_spec_the_slot_refuses_is_reported_in_its_words() {
    let info = Strict.info();
    let long = StreamSpec { sample_rate: 16_000, block: 4_000, latency: 0 };
    let short = StreamSpec { block: 160, ..long };
    assert_eq!(stream_problems(&Strict, &info, &manifest_with(Some(long))), ["blocks over 100 ms add too much delay"]);
    assert_eq!(stream_problems(&Strict, &info, &manifest_with(Some(short))).len(), 0);
    assert_eq!(stream_problems(&Strict, &info, &manifest_with(None)).len(), 1);
}

#[test]
fn the_written_manifest_round_trips_through_toml() {
    let manifest = manifest_with(None);
    assert_eq!((manifest.version.to_string().as_str(), manifest.name.as_str()), ("0.0.0", "m"));
    assert!(manifest.card.licence.is_unknown());
    let text = manifest.to_toml().unwrap();
    assert_eq!(ModelManifest::from_toml(&text).unwrap(), manifest);
}

#[test]
fn ids_are_slugs_made_unique_and_runtimes_come_from_the_file() {
    assert_eq!(unique_id("My Voice!!", |_| false).unwrap().as_str(), "user-my-voice");
    assert_eq!(unique_id("###", |_| false).unwrap().as_str(), "user-model");
    let taken = |id: &omuzaki_ai_api::ModelId| id.as_str() == "user-a" || id.as_str() == "user-a-2";
    assert_eq!(unique_id("a", taken).unwrap().as_str(), "user-a-3");
    assert_eq!(runtime_of("x.ONNX", b"").unwrap().as_str(), "onnx");
    assert_eq!(runtime_of("x.bin", b"lmgg\0").unwrap().as_str(), "whisper-ggml");
    assert!(runtime_of("x.bin", b"????").is_none());
}
