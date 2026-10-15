//! Declared gates: what `project.describe` lists as failing is read as gate names, and any other shape is the
//! harness's error, never a failing gate the agent is blamed for.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_ops_mock::MockRunner;
use omuzaki_test::eval::check::{Checker, Outcome};
use omuzaki_test::eval::task::Task;
use omuzaki_test::eval::transcript::Transcript;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A `project.describe` that lists whatever the document keeps under `failed`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Describe {}

impl Op for Describe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "project.describe",
            summary: "Test op.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let failed = ctx.doc.get(&DocPath::parse("failed").unwrap()).cloned();
        Ok(Plan::answer(json!({ "gates": { "failed": failed } })))
    }
}
omuzaki_ops_api::register_op!(Describe);

fn runner(failed: &Value) -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(json!({ "failed": failed }))))
}

fn declared(failed: &Value) -> (Outcome, String) {
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    let task: Task = toml::from_str(&format!("{head}[[check]]\nid='c'\nkind='gate'\ndeclared=true\n")).unwrap();
    let report = Checker::new(runner(&json!([])), runner(failed), Transcript::default()).grade(&task);
    (report.results[0].outcome, report.results[0].detail.clone())
}

#[test]
fn declared_gates_fail_on_names_and_error_on_any_other_shape() {
    assert_eq!(declared(&json!([])).0, Outcome::Pass);
    let (outcome, detail) = declared(&json!(["hair-collision", "face-symmetry"]));
    assert_eq!(outcome, Outcome::Fail);
    assert!(detail.contains("hair-collision, face-symmetry"), "{detail}");
    for malformed in [json!([1]), json!(["a", null]), json!([{"gate": "a"}]), json!("a"), json!({"a": 1}), json!(null)]
    {
        assert_eq!(declared(&malformed).0, Outcome::Error, "{malformed}");
    }
}
