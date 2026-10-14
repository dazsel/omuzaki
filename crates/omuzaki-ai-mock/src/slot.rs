//! Two test slots, `test_echo@1` (request/response) and `test_stream@1` (streaming): one `f32` input `x`, one
//! output `y`, and a smoke test that passes only for the identity. A shipped model for the echo slot, `echo-fake`, is
//! registered too, pinned to [`ECHO_MODEL`] and served by [`fixtures_fetcher`].

use omuzaki_ai_api::{
    Better, Check, DType, Dim, EvalEnv, EvalError, EvalLevel, EvalReport, Likeness, Metric, Session, Signature, Slot,
    SlotId, SlotInfo, SlotMode, SlotRef, TensorBuf, TensorRef, TensorSpec, TestSet, TestSetKind, register_model,
    register_slot,
};

use crate::MemFetcher;

/// The fake model file `echo-fake` ships: the identity, in the `fake` runtime's format.
pub const ECHO_MODEL: &str = include_str!("../models/echo.fake.toml");

/// Where `echo-fake`'s manifest says [`ECHO_MODEL`] is downloaded from.
pub const ECHO_MODEL_URL: &str = "https://models.invalid/echo-fake/1.0.0/echo.fake.toml";

register_model!("echo-fake", include_str!("../models/echo-fake.model.toml"));

/// A fetcher serving every file the mock's shipped models pin.
pub fn fixtures_fetcher() -> MemFetcher {
    MemFetcher::new().serving(ECHO_MODEL_URL, ECHO_MODEL)
}

/// The test slots' signature: `x: f32[any] → y: f32[any]`.
pub fn echo_signature() -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("x", DType::F32, [Dim::ANY])],
        outputs: vec![TensorSpec::new("y", DType::F32, [Dim::ANY])],
    }
}

fn slot_ref(id: &str) -> SlotRef {
    SlotRef::new(SlotId::new(id).unwrap_or_else(|_| unreachable!("a literal slug")), 1)
}

/// Feeds a ramp and reports the largest difference from it: 0 for the identity. Floor 1e-6.
fn echo_eval(session: &mut dyn Session, level: EvalLevel) -> Result<EvalReport, EvalError> {
    const RAMP: [f32; 4] = [0.0, 0.25, 0.5, 1.0];
    let mut out = TensorBuf::for_outputs(session.signature());
    session.run(&[TensorRef::f32(&RAMP, &[RAMP.len()])], &mut out)?;
    let y = out.first().and_then(TensorBuf::as_f32).ok_or_else(|| EvalError::Output("no f32 output y".into()))?;
    if y.len() != RAMP.len() {
        return Err(EvalError::Output(format!("y has {} values, expected {}", y.len(), RAMP.len())));
    }
    let error = y.iter().zip(RAMP).map(|(a, b)| f64::from((a - b).abs())).fold(0.0, f64::max);
    let set = TestSet { id: "echo-ramp-1".into(), kind: TestSetKind::Synthetic };
    let metric = Metric { name: "max_abs_error".into(), value: error, better: Better::Lower, set };
    Ok(EvalReport { level, checks: vec![Check { metric, floor: Some(1e-6) }] })
}

/// `test_echo@1`: request/response, 100 ms stall deadline.
#[derive(Default)]
pub struct EchoSlot;

impl Slot for EchoSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            slot: slot_ref("test_echo"),
            summary: "Test slot: returns its input.".into(),
            without_model: "the test sees no output".into(),
            mode: SlotMode::Call { stall_ms: 100 },
            threads: 1,
            likeness: None,
        }
    }

    fn signature(&self) -> Signature {
        echo_signature()
    }

    fn evaluate(&self, session: &mut dyn Session, _: &dyn EvalEnv, level: EvalLevel) -> Result<EvalReport, EvalError> {
        echo_eval(session, level)
    }
}

/// `test_stream@1`: streaming, two blocks in flight, four missed blocks in a row stall it; its models may imitate a
/// voice (so consent rules are testable).
#[derive(Default)]
pub struct StreamEchoSlot;

impl Slot for StreamEchoSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            slot: slot_ref("test_stream"),
            summary: "Test slot: streams its input back.".into(),
            without_model: "the test plays its dry input".into(),
            mode: SlotMode::Stream { queue: 2, stall_blocks: 4 },
            threads: 1,
            likeness: Some(Likeness::VoiceConversion),
        }
    }

    fn signature(&self) -> Signature {
        echo_signature()
    }

    fn evaluate(&self, session: &mut dyn Session, _: &dyn EvalEnv, level: EvalLevel) -> Result<EvalReport, EvalError> {
        echo_eval(session, level)
    }
}

register_slot!(EchoSlot);
register_slot!(StreamEchoSlot);
