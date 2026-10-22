//! Palette parity (QG-11, #215): every op the studio registers is reachable from the command palette. Searching for
//! its name ranks it first, its form opens with the MCP tool it is, and no field of that form falls back to a
//! read-only row (a shape the form can edit only as JSON still runs, as over MCP). A planted op with a schema the form
//! cannot edit is named in the failure.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{
    Actor, Event as OpEvent, Grants, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Request, Runner, Scope,
    Services, TxId, Undo, tool_name,
};
use omuzaki_ui::ds::schema_form::Form;
use omuzaki_ui::ds::testing::press;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::wiring::{Document, runner_on};
use schemars::Schema;
use serde_json::json;

/// The studio's own runner, with every op the app links.
fn studio_runner() -> Arc<dyn Runner> {
    omuzaki_ui::link();
    runner_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"))
}

/// The ops of `runner` the palette does not reach, each with why: not ranked first for its own name, no form, or a
/// field its form cannot edit.
fn unreachable(runner: Arc<dyn Runner>) -> Vec<String> {
    let ops = runner.ops();
    let mut h = Harness::builder()
        .with_size(vec2(1366.0, 768.0))
        .build_ui_state(|ui: &mut Ui, shell: &mut Shell| shell.ui(ui), Shell::new(runner, None));
    h.run_steps(4);
    let mut out = Vec::new();
    for op in &ops {
        press(&mut h, Modifiers::COMMAND, Key::K);
        h.event(Event::Text(format!(">{}", op.name)));
        h.run_steps(2);
        let first = h.query_all_by_role(Role::ListBoxOption).find_map(|node| node.accesskit_node().label());
        if first.as_deref() != Some(op.name) {
            out.push(format!("{}: the search for its name ranks {first:?} first", op.name));
        } else if has_arguments(&op.schema) {
            // An op with arguments opens its form, which names the MCP tool it is. (One with none runs at once from
            // the same entry; it is not run here.)
            press(&mut h, Modifiers::NONE, Key::Enter);
            let mcp = format!("Same as MCP tool {}", tool_name(op.name));
            if h.query_by_label_contains(&mcp).is_none() {
                out.push(format!("{}: Enter did not open its form", op.name));
            }
        }
        out.extend(
            Form::from_schema(&op.schema)
                .read_only()
                .into_iter()
                .map(|(path, ty)| format!("{}: the form cannot edit {path} ({ty})", op.name)),
        );
        for _ in 0..3 {
            if h.state().palette_open() {
                press(&mut h, Modifiers::NONE, Key::Escape);
            }
        }
    }
    out
}

/// Whether `schema` takes any argument at all.
fn has_arguments(schema: &Schema) -> bool {
    schema.get("properties").and_then(Value::as_object).is_some_and(|p| !p.is_empty())
}

#[test]
fn every_op_the_studio_registers_is_reachable_from_the_palette() {
    let runner = studio_runner();
    let count = runner.ops().len();
    assert!(count > 50, "the studio links the kit, the face and the scene ops: {count}");
    let missing = unreachable(runner);
    assert!(missing.is_empty(), "ops the palette does not reach:\n{}", missing.join("\n"));
}

#[test]
fn a_planted_op_whose_schema_the_form_cannot_edit_fails_with_its_name() {
    let schema = json!({
        "type": "object",
        "properties": {"code": {"type": "string", "pattern": "(?=a)b"}},
        "required": ["code"]
    });
    let schema = Schema::try_from(schema).unwrap_or_else(|_| panic!("a schema"));
    let meta = OpMeta {
        name: "planted.lookahead",
        summary: "A planted op",
        kind: OpKind::Edit,
        scope: Scope::EditCharacter,
        undo: Undo::Auto,
    };
    let planted = Arc::new(Planted { inner: studio_runner(), extra: OpInfo::new(meta, schema) });
    let missing = unreachable(planted);
    assert_eq!(missing.len(), 1, "only the planted op: {missing:?}");
    assert!(missing[0].starts_with("planted.lookahead: the form cannot edit /code"), "{missing:?}");
}

/// The studio's runner with one more op.
struct Planted {
    inner: Arc<dyn Runner>,
    extra: OpInfo,
}

impl Runner for Planted {
    fn ops(&self) -> Vec<OpInfo> {
        let mut ops = self.inner.ops();
        ops.push(self.extra.clone());
        ops
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.inner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<OpEvent>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, since: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(since, limit, grants)
    }
}
