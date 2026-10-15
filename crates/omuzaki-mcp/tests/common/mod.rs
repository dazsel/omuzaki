//! Test support: the shared in-memory `Runner` (`omuzaki-ops-mock`) over a `MemDoc`, and a few ops registered with
//! `register_op!`, standing in for `omuzaki-ops`.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

pub mod http;

use omuzaki_core::id::Slug;
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CostHint, Grants, Mode, Op, OpCtx, OpError, OpInfo, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo,
};
use omuzaki_ops_mock::MockRunner;
use rmcp::model::{CallToolResult, Tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- reading tool results ------------------------------------------------------------------------------------

/// The first text block of a tool result: what a text-only client shows its model.
pub fn text(result: &CallToolResult) -> String {
    result.content[0].as_text().expect("text content").text.clone()
}

/// A tool result's `structuredContent`.
pub fn data(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().expect("structured content")
}

/// The tool called `name`, or a panic naming it.
pub fn tool<'a>(tools: &'a [Tool], name: &str) -> &'a Tool {
    tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no tool {name}"))
}

// ---- what a server offers ------------------------------------------------------------------------------------

/// Ops a server holds back unless its host wired the service they read (`McpOptions::with_connections`). The test
/// servers wire none, so `tools/list` is the runner's ops minus these.
pub const WITHHELD_WITHOUT_SERVICE: [&str; 1] = ["agent.connection"];

/// The runner's ops that a server with no host services offers as tools: all but [`WITHHELD_WITHOUT_SERVICE`].
pub fn offered_ops(runner: &dyn Runner) -> Vec<OpInfo> {
    runner.ops().into_iter().filter(|op| !WITHHELD_WITHOUT_SERVICE.contains(&op.name)).collect()
}

// ---- ops ---------------------------------------------------------------------------------------------------

/// Set the blend between a character's base face and its face-shape profile.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetBlend {
    /// The character to change.
    character: Slug,
    /// 0 = base face, 1 = full profile.
    #[schemars(range(min = 0.0, max = 1.0))]
    blend: f32,
}

impl Op for SetBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_blend",
            summary: "Set how far a character's face follows its face-shape profile.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.blend) {
            return Err(
                OpError::invalid("blend", format!("{} is out of range", self.blend)).hint("use a value from 0 to 1")
            );
        }
        let path = face_path(&self.character).join("blend");
        let face = face_path(&self.character);
        ctx.doc
            .get(&face)
            .ok_or_else(|| OpError::not_found(format!("no character {}", self.character)).hint("create it first"))?;
        Ok(Plan::edit(Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), Some(json!(self.blend)))))
    }
}

/// Read a character's face settings.
#[derive(Serialize, Deserialize, JsonSchema)]
struct DescribeFace {
    /// The character to describe.
    character: Slug,
}

impl Op for DescribeFace {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.describe_face",
            summary: "Describe a character's face settings.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let face = ctx
            .doc
            .get(&face_path(&self.character))
            .ok_or_else(|| OpError::not_found(format!("no character {}", self.character)))?;
        Ok(Plan::answer(face.clone()))
    }
}

/// List a character's parts (a big answer, for testing that answers are never cut).
#[derive(Serialize, Deserialize, JsonSchema)]
struct ListParts {
    /// How many parts to list.
    count: u32,
}

impl Op for ListParts {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.list_parts",
            summary: "List the parts of a character.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({"parts": (0..self.count).map(|i| format!("part-{i:05}")).collect::<Vec<_>>()})))
    }
}

/// Render a preview image.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Preview {
    /// The character to render.
    character: Slug,
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.preview",
            summary: "Render a preview image of a character.",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({"image": format!("omuzaki://render/{}.png", self.character)})))
    }
}

/// Measure something numeric.
#[derive(Serialize, Deserialize, JsonSchema)]
struct MeasureBlend {
    /// The character to measure.
    character: Slug,
}

impl Op for MeasureBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.blend",
            summary: "Measure the face blend.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.doc.get(&face_path(&self.character).join("blend")).cloned().unwrap_or(Value::Null)))
    }
}

/// Export a model file (needs the `publish` scope).
#[derive(Serialize, Deserialize, JsonSchema)]
struct ExportVrm {
    /// The character to export.
    character: Slug,
}

impl Op for ExportVrm {
    fn meta() -> OpMeta {
        OpMeta {
            name: "export.vrm",
            summary: "Export a character as a VRM file.",
            kind: OpKind::Job,
            scope: Scope::Publish,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut plan = Plan::answer(json!({"file": format!("{}.vrm", self.character)}));
        plan.cost = Some(CostHint { seconds: Some(12.0), gpu_mib: Some(2048), usd: None });
        Ok(plan)
    }
}

/// An op with no arguments at all.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Ping;

impl Op for Ping {
    fn meta() -> OpMeta {
        OpMeta {
            name: "project.ping",
            summary: "Check that the project is open.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!("pong")))
    }
}

/// An op with a bug in it, to prove one bad op can't take the session down.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Crash;

impl Op for Crash {
    fn meta() -> OpMeta {
        OpMeta { name: "debug.crash", summary: "Panics.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        panic!("deliberate panic in debug.crash (expected in the test log)")
    }
}

omuzaki_ops_api::register_op!(Crash);
omuzaki_ops_api::register_op!(ListParts);
omuzaki_ops_api::register_op!(SetBlend);
omuzaki_ops_api::register_op!(DescribeFace);
omuzaki_ops_api::register_op!(Preview);
omuzaki_ops_api::register_op!(MeasureBlend);
omuzaki_ops_api::register_op!(ExportVrm);
omuzaki_ops_api::register_op!(Ping);

fn face_path(character: &Slug) -> DocPath {
    DocPath::parse("characters").expect("valid").join(character.as_str()).join("face")
}

// ---- runner ------------------------------------------------------------------------------------------------

/// The shared in-memory runner over a `MemDoc`.
pub type FakeRunner = MockRunner<MemDoc>;

/// A runner over a document holding one character, `mira`, with an empty face.
pub fn fake_runner() -> FakeRunner {
    fake_runner_with(json!({"characters": {"mira": {"face": {}}}}))
}

/// A runner over `doc` instead of the default one, for tests that need other characters.
pub fn fake_runner_with(doc: Value) -> FakeRunner {
    MockRunner::new(MemDoc::from_value(doc))
}

/// Shortcuts over `mira`'s face for the tests; they run through the runner like any surface would.
pub trait FakeRunnerExt {
    /// The `blend` of `mira`'s face, as stored in the document.
    fn blend(&self) -> Option<Value>;
    /// A person (with every scope) sets `mira`'s blend directly through the runner, as the UI would.
    fn person_sets_blend(&self, blend: f32);
    /// `actor` (with every scope) sets `mira`'s blend directly through the runner, outside MCP.
    fn sets_blend_as(&self, actor: Actor, blend: f32);
}

impl FakeRunnerExt for FakeRunner {
    fn blend(&self) -> Option<Value> {
        self.get(&DocPath::parse("characters/mira/face/blend").expect("valid"), &Grants::person())
            .expect("a person may read")
    }

    fn person_sets_blend(&self, blend: f32) {
        self.sets_blend_as(Actor::Person { name: "tester".to_owned() }, blend);
    }

    fn sets_blend_as(&self, actor: Actor, blend: f32) {
        let request = Request {
            op: "face.set_blend".to_owned(),
            args: json!({"character": "mira", "blend": blend}),
            mode: Mode::Apply,
            actor,
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        self.run(request).expect("the edit applies");
    }
}
