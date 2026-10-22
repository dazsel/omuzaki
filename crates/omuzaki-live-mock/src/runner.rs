//! [`FakeRunner`]: answers op calls from scripts, so node and engine tests need no executor.
//!
//! Calls are numbered from 1 in request order. A call to an op with a script gets a [`Reply`] due at `now + latency`;
//! the host delivers it as an inbox item, ordered exactly as the engine orders replies. Without a script a call stays
//! unanswered, and the test sees it in [`calls`](FakeRunner::calls).

use std::collections::BTreeMap;
use std::fmt;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::node::CallId;
use omuzaki_ops_api::OpError;
use serde::Serialize;
use serde_json::Value;

/// One op call a node made.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Call {
    /// The call.
    pub id: CallId,
    /// When it was made, on the runner clock.
    pub at: Seconds,
    /// The op.
    pub op: String,
    /// Its arguments.
    pub args: Value,
}

/// A scripted answer to a call, due at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// The call it answers.
    pub call: CallId,
    /// When it arrives, on the runner clock.
    pub at: Seconds,
    /// The op's output, or why it failed.
    pub outcome: Result<Option<Value>, OpError>,
}

/// How a script answers an op's arguments.
type Answer = Box<dyn FnMut(&Value) -> Result<Option<Value>, OpError>>;

/// Op scripts and the calls made so far.
#[derive(Default)]
pub struct FakeRunner {
    scripts: BTreeMap<String, (Seconds, Answer)>,
    calls: Vec<Call>,
}

impl fmt::Debug for FakeRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeRunner")
            .field("scripts", &self.scripts.keys().collect::<Vec<_>>())
            .field("calls", &self.calls)
            .finish()
    }
}

impl FakeRunner {
    /// No scripts and no calls.
    pub fn new() -> Self {
        Self::default()
    }

    /// From now on, answers each call to `op` with `answer(args)`, `latency` after the call. Replaces an earlier
    /// script for `op`; calls already made keep the answer they had.
    pub fn respond(
        &mut self,
        op: &str,
        latency: Seconds,
        answer: impl FnMut(&Value) -> Result<Option<Value>, OpError> + 'static,
    ) {
        self.scripts.insert(op.to_owned(), (latency, Box::new(answer)));
    }

    /// Records a call to `op` with `args` at `at`, and its scripted reply if `op` has a script.
    pub fn request(&mut self, at: Seconds, op: &str, args: Value) -> (CallId, Option<Reply>) {
        let id = CallId(u64::try_from(self.calls.len()).unwrap_or(u64::MAX).saturating_add(1));
        let reply = self.scripts.get_mut(op).map(|(latency, answer)| Reply {
            call: id,
            at: Seconds(at.0 + latency.0.max(0.0)),
            outcome: answer(&args),
        });
        self.calls.push(Call { id, at, op: op.to_owned(), args });
        (id, reply)
    }

    /// Every call made, in order.
    pub fn calls(&self) -> &[Call] {
        &self.calls
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scripted_ops_answer_after_their_latency_and_others_stay_pending() {
        let mut runner = FakeRunner::new();
        runner.respond("chat.send", Seconds(0.25), |args| Ok(Some(json!({"echo": args["text"]}))));
        let (first, reply) = runner.request(Seconds(1.0), "chat.send", json!({"text": "hi"}));
        let reply = reply.unwrap();
        assert_eq!((first, reply.at, reply.outcome), (CallId(1), Seconds(1.25), Ok(Some(json!({"echo": "hi"})))));
        let (second, none) = runner.request(Seconds(2.0), "throw.item", json!({}));
        assert_eq!((second, none), (CallId(2), None));
        assert_eq!(runner.calls().len(), 2);
    }
}
