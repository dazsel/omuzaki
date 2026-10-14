//! `omuzaki ops` and `omuzaki help <op>` against ops registered the way real ops are (`register_op!`), run through the
//! runner the binary opens when no project is given. The snapshots in `tests/snapshots/` are the documented output.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::io;

use common::snapshot;
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::commands::execute;
use omuzaki_cli::session::{Session, open_runner};
use omuzaki_ops_api::{ErrorCode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Blend 0 keeps the character's base face; 1 uses the face-shape profile completely.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetProfileBlend {
    /// The character.
    character: omuzaki_core::id::Slug,
    /// 0 = base face, 1 = full profile.
    #[schemars(range(min = 0.0, max = 1.0))]
    blend: f32,
    /// Why the blend changed, for the history.
    #[schemars(length(max = 80))]
    note: Option<String>,
}

impl Op for SetProfileBlend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_profile_blend",
            summary: "Set the blend between the base face and the face-shape profile.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}

omuzaki_ops_api::register_op!(SetProfileBlend);

/// Reports the project's name, schema version and number of characters, scenes and shows.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Describe {}

impl Op for Describe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.describe",
            summary: "Describe the open project.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}

omuzaki_ops_api::register_op!(Describe);

/// How much work a render does.
#[derive(Serialize, Deserialize, JsonSchema)]
enum Quality {
    /// Fast, with visible noise.
    Draft,
    /// Slow, final quality.
    Final,
}

/// Where the output goes.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Target {
    /// Folder to write into.
    folder: String,
    /// Container format, e.g. `mp4`.
    format: Option<String>,
}

/// The render runs in the job manager, so this returns at once with a job id. Progress and cancellation are
/// reported through events.
#[derive(Serialize, Deserialize, JsonSchema)]
struct RenderClip {
    /// The scene to render.
    scene: omuzaki_core::id::Slug,
    /// How much work to do.
    quality: Quality,
    /// Frames to render.
    #[serde(default = "default_frames")]
    #[schemars(range(min = 1, max = 600))]
    frames: u32,
    /// Output size in pixels, width then height.
    size: (u32, u32),
    /// Labels for the finished clip.
    #[schemars(length(max = 8))]
    tags: Vec<String>,
    /// Where to write; defaults to the project's renders folder.
    target: Option<Target>,
    /// Seconds of padding before and after.
    #[schemars(range(min = 0.0))]
    padding: Option<f32>,
}

fn default_frames() -> u32 {
    120
}

impl Op for RenderClip {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.render_clip",
            summary: "Render a clip of a scene to a video file.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}

omuzaki_ops_api::register_op!(RenderClip);

fn omuzaki(args: &[&str]) -> Result<String, OpError> {
    let cli = try_parse_from(std::iter::once("omuzaki").chain(args.iter().copied())).expect("valid command line");
    execute(&cli, open_runner(&cli)?, &Session::person("tester"), &mut io::empty())
}

/// Only the ops this file registers: `omuzaki-cli` links the product's crates, whose ops have their own tests.
fn is_test_op(name: &str) -> bool {
    name.starts_with("test.")
}

#[test]
fn ops_lists_every_registered_op_sorted() {
    let listing = omuzaki(&["ops"]).unwrap();
    // The columns are as wide as the widest of all the ops, so the rows are compared with their spacing squeezed.
    let rows: Vec<String> = listing
        .lines()
        .filter(|l| l.starts_with("NAME") || is_test_op(l))
        .map(|l| l.split("  ").filter(|cell| !cell.is_empty()).map(str::trim).collect::<Vec<_>>().join(" | "))
        .collect();
    snapshot("ops.txt", &format!("{}\n", rows.join("\n")));
}

#[test]
fn ops_json_has_stable_fields_and_is_valid() {
    let all: serde_json::Value = serde_json::from_str(&omuzaki(&["ops", "--json"]).unwrap()).unwrap();
    let rows: Vec<&serde_json::Value> =
        all.as_array().unwrap().iter().filter(|r| is_test_op(r["name"].as_str().unwrap())).collect();
    snapshot("ops.json", &format!("{}\n", serde_json::to_string_pretty(&rows).unwrap()));
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["test.describe", "test.render_clip", "test.set_profile_blend"]);
}

#[test]
fn help_documents_arguments_from_the_schema() {
    snapshot("help_set_profile_blend.txt", &omuzaki(&["help", "test.set_profile_blend"]).unwrap());
}

#[test]
fn help_covers_enums_nesting_tuples_defaults_and_long_descriptions() {
    snapshot("help_render_clip.txt", &omuzaki(&["help", "test.render_clip"]).unwrap());
}

#[test]
fn help_for_an_op_without_arguments_says_so() {
    snapshot("help_describe.txt", &omuzaki(&["help", "test.describe"]).unwrap());
}

#[test]
fn help_json_carries_the_registry_entry_and_the_schema() {
    let text = omuzaki(&["help", "test.set_profile_blend", "--json"]).unwrap();
    snapshot("help_set_profile_blend.json", &text);
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["schema"]["properties"]["blend"]["maximum"], 1.0);
}

#[test]
fn help_for_an_unknown_op_is_not_found_with_a_hint() {
    let e = omuzaki(&["help", "test.nope"]).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.hint.as_deref().is_some_and(|h| h.contains("omuzaki ops")), "{e:?}");
}

#[test]
fn help_without_an_op_shows_the_command_help_with_the_exit_codes() {
    let text = omuzaki(&["help"]).unwrap();
    assert!(text.contains("Usage: omuzaki"), "{text}");
    assert!(text.contains("Exit codes:"), "{text}");
    assert!(text.ends_with(".\n") && !text.ends_with("\n\n"), "ends with exactly one newline: {text:?}");
}

#[test]
fn help_for_a_cli_command_name_points_at_its_own_help() {
    let e = omuzaki(&["help", "run"]).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(e.hint.as_deref(), Some("`run` is a omuzaki command, not an op; see `omuzaki run --help`"));
}
