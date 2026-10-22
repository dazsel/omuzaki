//! `output.recommend` (#1048) as an op: a read-scope query that asks the app's OBS probe and answers with the pick,
//! the others and the OBS steps. The machine table is in the module's unit tests.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, Services, ServicesBuilder, find};
use omuzaki_stream_api::output::ObsProbe;
use serde_json::{Value, json};

struct Obs(bool);

impl ObsProbe for Obs {
    fn obs_running(&self) -> bool {
        self.0
    }
}

fn recommend(services: &Services) -> Value {
    omuzaki_stream::link();
    let reg = find("output.recommend").expect("registered");
    assert_eq!(((reg.meta)().kind, (reg.meta)().scope), (OpKind::Query, Scope::Read));
    let op = (reg.decode)(json!({})).unwrap();
    let (doc, clock, actor) =
        (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Agent { id: "helper#1".into(), model: None });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services);
    op.plan(&ctx).unwrap().output.expect("the op answers")
}

#[test]
fn the_probe_asks_the_apps_obs_probe() {
    for running in [true, false] {
        let services = ServicesBuilder::new().insert::<dyn ObsProbe>(Arc::new(Obs(running))).build();
        let answer = recommend(&services);
        assert_eq!(answer["obs"], running, "{answer}");
        // This test build registers no output kind and has no window host: nothing to recommend, so no OBS steps.
        assert_eq!((answer["kind"].clone(), answer["obs_hint"].clone()), (Value::Null, Value::Null), "{answer}");
        assert_eq!(answer["others"], json!([]));
    }
}
