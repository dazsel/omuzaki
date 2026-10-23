//! A small studio for the op tests: the mock character in a document, the host library as the app wires it, and
//! stand-ins for the character ops the recipes call (the real ones are in `omuzaki-char` and `omuzaki-face`, which
//! the app's end-to-end test runs). Each op is planned and its patch applied, as the executor does; undo applies the
//! inverse.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::sync::Arc;

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_library::HostLibrary;
use omuzaki_library_api::Library;
use omuzaki_ops_api::{
    Actor, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Services, ServicesBuilder, Undo, find, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn character() -> CharId {
    CharId::new("mira").unwrap()
}

pub struct Studio {
    pub doc: MemDoc,
    pub library: Arc<HostLibrary>,
    services: Services,
    clock: FakeClock,
    undo: Vec<Patch>,
}

impl Studio {
    pub fn new(library: HostLibrary) -> Self {
        let def: Value = serde_json::from_str(omuzaki_char_mock::CHARACTER_JSON).unwrap();
        let library = Arc::new(library);
        let services = ServicesBuilder::new()
            .insert::<dyn Library>(library.clone())
            .insert::<HostLibrary>(library.clone())
            .build();
        Self {
            doc: MemDoc::from_value(json!({ "characters": { "mira": def } })),
            library,
            services,
            clock: FakeClock::new(Seconds(0.0)),
            undo: Vec::new(),
        }
    }

    fn plan_as(&self, actor: &Actor, op: &str, args: Value, dry_run: bool) -> Result<Plan, OpError> {
        let registration = find(op).unwrap_or_else(|| panic!("{op} is not registered"));
        let op = (registration.decode)(args)?;
        let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { Grants::agent_default() };
        let ctx = OpCtx::new(&self.doc, actor, &self.clock)
            .with_services(&self.services)
            .with_grants(&grants)
            .with_dry_run(dry_run);
        op.plan(&ctx)
    }

    /// Plans `op` as a person and applies it; returns its output.
    pub fn run(&mut self, op: &str, args: Value) -> Result<Value, OpError> {
        self.run_as(&Actor::Person { name: "Tester".into() }, op, args)
    }

    pub fn run_as(&mut self, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
        let plan = self.plan_as(actor, op, args, false)?;
        if !plan.patch.is_empty() {
            self.doc.apply(&plan.patch).unwrap();
            self.undo.push(plan.patch.inverse());
        }
        Ok(plan.output.unwrap_or(Value::Null))
    }

    /// Plans `op` as a dry run and returns the plan, applying nothing.
    pub fn dry(&self, op: &str, args: Value) -> Result<Plan, OpError> {
        self.plan_as(&Actor::Person { name: "Tester".into() }, op, args, true)
    }

    pub fn undo(&mut self) {
        let inverse = self.undo.pop().expect("something to undo");
        self.doc.apply(&inverse).unwrap();
    }

    pub fn at(&self, path: &str) -> Option<Value> {
        let at = path.split('/').fold(paths::character(&character()), |p, s| p.join(s));
        self.doc.get(&at).cloned()
    }

    /// A person's hand edit, outside the library.
    pub fn hand_edit(&mut self, path: &str, value: Option<Value>) {
        let at = path.split('/').fold(paths::character(&character()), |p, s| p.join(s));
        let before = self.doc.get(&at).cloned();
        self.doc.apply(&Patch::new().with(at, before, value)).unwrap();
    }

    pub fn snapshot(&self) -> Value {
        self.doc.get(&DocPath::root()).cloned().unwrap()
    }
}

fn meta(name: &'static str) -> OpMeta {
    OpMeta { name, summary: "A stand-in.", kind: OpKind::Edit, scope: Scope::EditCharacter, undo: Undo::Auto }
}

fn set(ctx: &OpCtx<'_>, at: &DocPath, value: Value) -> Plan {
    Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(at).cloned(), Some(value)))
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PartAdd {
    character: CharId,
    kind: String,
    id: PartId,
    layer: Option<i32>,
}

impl Op for PartAdd {
    fn meta() -> OpMeta {
        meta("part.add")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::part(&self.character, &self.id);
        if ctx.doc.get(&at).is_some() {
            return Err(OpError::invalid("id", "a part with this id exists"));
        }
        Ok(set(ctx, &at, json!({ "kind": self.kind, "layer": self.layer })))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PaletteSet {
    character: CharId,
    slot: String,
    colour: String,
}

impl Op for PaletteSet {
    fn meta() -> OpMeta {
        meta("palette.set")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::palette(&self.character).join(&self.slot);
        if ctx.doc.get(&at).is_none() {
            return Err(OpError::invalid("slot", "no such palette slot"));
        }
        Ok(set(ctx, &at, json!(self.colour)))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetStyle {
    character: CharId,
    path: String,
    value: Value,
}

impl Op for SetStyle {
    fn meta() -> OpMeta {
        meta("face.set_style")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::character(&self.character).join(self.path.as_str());
        Ok(set(ctx, &at, self.value.clone()))
    }
}

register_op!(PartAdd);
register_op!(PaletteSet);
register_op!(SetStyle);
