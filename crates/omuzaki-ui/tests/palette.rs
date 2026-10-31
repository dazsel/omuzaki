//! The command palette end to end through the shell (#153, `shell.md` section 8), headless and keyboard only: fifty
//! generated ops over every argument shape the registered ops use are each found, filled from their own schema and run;
//! an edit's form previews itself with a dry run; the recents come first and survive a restart; and "Copy as MCP call"
//! copies exactly the tool and the arguments the run sends.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, PoisonError};

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, OutputCommand, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{
    Actor, Event as OpEvent, Grants, Mode, OpError, OpInfo, OpKind, OpMeta, OpRecord, Outcome, Plan, Request, Runner,
    Scope, TxId, Undo, tool_name,
};
use omuzaki_ui::ds::schema_form::Form;
use omuzaki_ui::ds::testing::press;
use omuzaki_ui::shell::Shell;
use schemars::Schema;
use serde_json::json;

/// How many ops the fake registers.
const OPS: usize = 50;

/// One argument shape each, cycled over the ops: everything `tests/op_forms.rs` finds in the registered ops' schemas.
fn shape(i: usize) -> (&'static str, Value) {
    match i % 10 {
        0 => ("slider", json!({"type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.5})),
        1 => ("count", json!({"type": "integer", "minimum": 0, "maximum": 3, "default": 1})),
        2 => ("choice", json!({"type": "string", "enum": ["round", "sharp", "soft"], "default": "round"})),
        3 => ("slug", json!({"type": "string", "pattern": "^[a-z0-9]+(-[a-z0-9]+)*$", "default": "front-hair"})),
        4 => ("toggle", json!({"type": "boolean", "default": false})),
        5 => ("maybe", json!({"type": ["string", "null"]})),
        6 => ("list", json!({"type": "array", "items": {"type": "string"}, "default": []})),
        7 => (
            "section",
            json!({"type": "object", "properties": {"size": {"type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.2}}}),
        ),
        8 => ("either", json!({"oneOf": [{"type": "string"}, {"type": "integer"}], "default": 1})),
        _ => ("colour", json!({"type": "string", "x-ui": {"widget": "colour"}, "default": "#ff8800"})),
    }
}

/// Op `i`: `gen.<shape>_<i>`, a query when `i` is a multiple of seven, an edit otherwise.
fn op(i: usize) -> OpInfo {
    let (name, field) = shape(i);
    let name: &'static str = Box::leak(format!("gen.{name}_{i:02}").into_boxed_str());
    let summary: &'static str = Box::leak(format!("Generated op {i}").into_boxed_str());
    let kind = if i.is_multiple_of(7) { OpKind::Query } else { OpKind::Edit };
    let schema =
        json!({"type": "object", "properties": {"value": field, "part": {"type": "string"}}, "required": ["part"]});
    let schema = Schema::try_from(schema).unwrap_or_else(|_| panic!("a schema"));
    OpInfo::new(OpMeta { name, summary, kind, scope: Scope::EditCharacter, undo: Undo::Auto }, schema)
}

/// A runner with fifty generated ops that records every request and answers each with one change.
struct Generated {
    ops: Vec<OpInfo>,
    asked: Mutex<Vec<(String, Value, Mode)>>,
}

impl Generated {
    fn new() -> Arc<Self> {
        Arc::new(Self { ops: (0..OPS).map(op).collect(), asked: Mutex::new(Vec::new()) })
    }

    fn asked(&self) -> Vec<(String, Value, Mode)> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn last(&self) -> Option<(String, Value, Mode)> {
        self.asked().last().cloned()
    }
}

impl Runner for Generated {
    fn ops(&self) -> Vec<OpInfo> {
        self.ops.clone()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let info = self.ops.iter().find(|op| op.name == req.op).ok_or_else(|| OpError::not_found("no such op"))?;
        if req.args.get("part").and_then(Value::as_str).is_none() {
            return Err(OpError::invalid("part", "`part` is required"));
        }
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).push((req.op.clone(), req.args.clone(), req.mode));
        let path = DocPath::parse("characters/c/value").map_err(|e| OpError::invalid("path", e.to_string()))?;
        let plan = match info.kind {
            OpKind::Query => Plan::answer(json!({"asked": req.args})),
            OpKind::Edit | OpKind::Job => Plan::edit(Patch::new().with(path, None, req.args.get("value").cloned())),
        };
        Ok(Outcome::new(plan))
    }
    fn begin(&self, _: &str, _: &Actor) -> TxId {
        TxId(1)
    }
    fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn subscribe(&self, _: &Grants) -> Result<Receiver<OpEvent>, OpError> {
        Ok(channel().1)
    }
    fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

/// The shell and what it put on the clipboard.
struct Rig {
    shell: Shell,
    copied: Vec<String>,
}

fn studio(runner: Arc<Generated>, recents: Option<std::path::PathBuf>) -> Harness<'static, Rig> {
    let mut shell = Shell::new(runner, None);
    if let Some(file) = recents {
        shell.keep_palette_recents_in(file);
    }
    let mut harness = Harness::builder().with_size(vec2(1366.0, 768.0)).build_ui_state(
        |ui: &mut Ui, rig: &mut Rig| {
            rig.shell.ui(ui);
            ui.ctx().output(|o| {
                for command in &o.commands {
                    if let OutputCommand::CopyText(text) = command {
                        rig.copied.push(text.clone());
                    }
                }
            });
        },
        Rig { shell, copied: Vec::new() },
    );
    harness.run_steps(4);
    harness
}

/// Ctrl+K, then `query` typed into the palette.
fn find(h: &mut Harness<'_, Rig>, query: &str) {
    press(h, Modifiers::COMMAND, Key::K);
    assert!(h.state().shell.palette_open(), "Ctrl+K opens the palette");
    h.event(Event::Text(query.to_owned()));
    h.run_steps(2);
}

/// The palette's results, top first.
fn results(h: &Harness<'_, Rig>) -> Vec<String> {
    h.query_all_by_role(Role::ListBoxOption).filter_map(|node| node.accesskit_node().label()).collect()
}

/// What the form of op `i` sends when nothing in it is changed, with the part the test types in.
fn defaults(i: usize, part: &str) -> Value {
    let info = op(i);
    let mut value = Value::from(&Form::from_schema(&info.schema).default_value());
    value["part"] = json!(part);
    value
}

/// Presses Tab until the control `role` called `label` has the keyboard.
fn tab_to(h: &mut Harness<'_, Rig>, role: Role, label: &str) {
    for _ in 0..12 {
        press(h, Modifiers::NONE, Key::Tab);
        if h.query_by_role_and_label(role, label).is_some_and(|n| n.is_focused()) {
            return;
        }
    }
    let inputs: Vec<_> = h.query_all_by_role(role).map(|n| (n.accesskit_node().label(), n.is_focused())).collect();
    panic!("Tab never reached {label}: {inputs:?}");
}

/// Types `part` into the form's part field, reached with Tab alone.
fn fill_part(h: &mut Harness<'_, Rig>, part: &str) {
    tab_to(h, Role::TextInput, "Part");
    h.event(Event::Text(part.to_owned()));
    h.run_steps(2);
}

#[test]
fn each_of_fifty_generated_ops_is_found_filled_from_its_schema_and_run_from_the_keyboard() {
    let runner = Generated::new();
    let mut h = studio(Arc::clone(&runner), None);
    for i in 0..OPS {
        let name = op(i).name;
        find(&mut h, &format!(">{name}"));
        assert_eq!(results(&h).first().map(String::as_str), Some(name), "the exact name ranks first");
        press(&mut h, Modifiers::NONE, Key::Tab);
        assert!(h.query_by_label_contains(&format!("Same as MCP tool {}", tool_name(name))).is_some(), "{name}");
        fill_part(&mut h, "hair");
        press(&mut h, Modifiers::NONE, Key::Enter);
        let (ran, args, mode) = runner.last().unwrap_or_else(|| panic!("{name} ran"));
        assert_eq!((ran.as_str(), mode), (name, Mode::Apply));
        assert_eq!(args, defaults(i, "hair"), "{name} sends its schema's defaults");
        let query = op(i).kind == OpKind::Query;
        assert_eq!(h.query_by_role_and_label(Role::TextInput, "Part").is_some(), query, "only a query keeps its form");
        if query {
            press(&mut h, Modifiers::NONE, Key::Escape);
        }
        press(&mut h, Modifiers::NONE, Key::Escape);
        assert!(!h.state().shell.palette_open(), "Esc closes the form, then the palette");
    }
}

#[test]
fn an_edit_previews_itself_with_a_dry_run_and_shift_enter_is_a_dry_run() {
    let runner = Generated::new();
    let mut h = studio(Arc::clone(&runner), None);
    find(&mut h, ">gen.slider_10");
    press(&mut h, Modifiers::NONE, Key::Tab);
    fill_part(&mut h, "hair");
    h.run_steps(30);
    let previews: Vec<_> = runner
        .asked()
        .into_iter()
        .filter(|(_, args, mode)| *mode == Mode::DryRun && args["part"] == json!("hair"))
        .collect();
    assert_eq!(previews.len(), 1, "one dry run once the arguments settle, not one per frame or keystroke");
    assert_eq!(previews[0].1, defaults(10, "hair"));
    assert!(h.query_by_label_contains("Live preview").is_some());
    assert!(h.query_by_label_contains("Values it would change: 1").is_some());

    // Arrow keys move the slider; the preview follows the new value.
    tab_to(&mut h, Role::Slider, "Value");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    h.run_steps(30);
    let (_, args, mode) = runner.last().unwrap_or_else(|| panic!("a preview"));
    assert_eq!(mode, Mode::DryRun);
    assert!(args["value"].as_f64().is_some_and(|v| v > 0.5), "the preview has the moved value: {args}");

    press(&mut h, Modifiers::SHIFT, Key::Enter);
    let (_, _, mode) = runner.last().unwrap_or_else(|| panic!("a dry run"));
    assert_eq!(mode, Mode::DryRun);
    assert!(h.query_by_label_contains("dry run, nothing changed. Values it would change: 1").is_some());
    assert!(runner.asked().iter().all(|(_, _, mode)| *mode == Mode::DryRun), "nothing applied");
}

#[test]
fn copy_as_mcp_call_is_exactly_the_tool_and_the_arguments_the_run_sends() {
    let runner = Generated::new();
    let mut h = studio(Arc::clone(&runner), None);
    find(&mut h, ">gen.slug_13");
    press(&mut h, Modifiers::NONE, Key::Tab);
    fill_part(&mut h, "hair");
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::C);
    let copied = h.state().copied.last().cloned().unwrap_or_else(|| panic!("something was copied"));
    let call: Value = serde_json::from_str(&copied).unwrap_or_else(|e| panic!("{e}: {copied}"));
    assert_eq!(call["name"], json!("gen_slug_13"), "the MCP tool, not the dotted op name");
    assert_eq!(call.as_object().map(serde_json::Map::len), Some(2), "only name and arguments: {call}");
    assert!(h.query_by_label_contains("Copied the MCP call").is_some());

    press(&mut h, Modifiers::NONE, Key::Enter);
    let (_, sent, mode) = runner.last().unwrap_or_else(|| panic!("it ran"));
    assert_eq!(mode, Mode::Apply);
    assert_eq!(call["arguments"], sent, "the copied arguments are the ones the run sent");

    // The command line form of the same run.
    press(&mut h, Modifiers::NONE, Key::Escape);
    find(&mut h, ">gen.slug_13");
    press(&mut h, Modifiers::NONE, Key::Tab);
    fill_part(&mut h, "it's");
    h.get_by_role_and_label(Role::Button, "Copy as CLI").click();
    h.run_steps(2);
    let line = h.state().copied.last().cloned().unwrap_or_default();
    let quoted = line.strip_prefix("omuzaki run gen.slug_13 --args '").and_then(|rest| rest.strip_suffix('\''));
    let json = quoted.unwrap_or_else(|| panic!("an `omuzaki run` line: {line}")).replace("'\\''", "'");
    let args: Value = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {json}"));
    assert_eq!(args["part"], json!("it's"), "a quote in the arguments survives the shell");
}

#[test]
fn the_ops_run_last_come_first_and_are_kept_for_the_next_start() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let file = dir.path().join("palette-recents.json");
    let runner = Generated::new();
    let mut h = studio(Arc::clone(&runner), Some(file.clone()));
    for i in [21, 34] {
        find(&mut h, &format!(">{}", op(i).name));
        press(&mut h, Modifiers::NONE, Key::Tab);
        fill_part(&mut h, "hair");
        press(&mut h, Modifiers::NONE, Key::Enter);
        // A query keeps its form open to show the answer; an edit's closes.
        while h.state().shell.palette_open() {
            press(&mut h, Modifiers::NONE, Key::Escape);
        }
    }
    press(&mut h, Modifiers::COMMAND, Key::K);
    h.run_steps(2);
    assert_eq!(results(&h)[..2], ["gen.toggle_34".to_owned(), "gen.count_21".to_owned()], "most recent first");
    // Typing narrows, and among equal matches the recent one still leads.
    h.event(Event::Text(">gen.c".to_owned()));
    h.run_steps(2);
    assert_eq!(results(&h).first().map(String::as_str), Some("gen.count_21"));

    let saved: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(saved, ["gen.toggle_34", "gen.count_21"]);
    let mut again = studio(Generated::new(), Some(file));
    assert_eq!(again.state().shell.palette_recents(), ["gen.toggle_34", "gen.count_21"]);
    press(&mut again, Modifiers::COMMAND, Key::K);
    again.run_steps(2);
    assert_eq!(results(&again).first().map(String::as_str), Some("gen.toggle_34"));
}

/// A query that reads like a description offers to make a character from it, first; Enter makes it through
/// `character.create_from_description` on the studio's own core, as one op. Short queries don't offer it.
#[test]
fn a_description_typed_into_the_palette_makes_a_character() {
    omuzaki_ui::link();
    let core = omuzaki_ui::wiring::core_on(
        omuzaki_ui::wiring::Document::empty(),
        omuzaki_ops_api::ServicesBuilder::new().build(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let runner = Arc::clone(&core.runner);
    let mut h = Harness::builder()
        .with_size(vec2(1366.0, 768.0))
        .build_ui_state(|ui: &mut Ui, shell: &mut Shell| shell.ui(ui), Shell::new(Arc::clone(&core.runner), None));
    h.run_steps(4);
    press(&mut h, Modifiers::COMMAND, Key::K);
    h.event(Event::Text("part add".to_owned()));
    h.run_steps(2);
    let first: Vec<String> =
        h.query_all_by_role(Role::ListBoxOption).filter_map(|node| node.accesskit_node().label()).collect();
    assert!(!first.iter().any(|l| l.contains("Make a character")), "two words are a search: {first:?}");
    h.event(Event::Text(" knight with a red cape".to_owned()));
    h.run_steps(2);
    let shown: Vec<String> =
        h.query_all_by_role(Role::ListBoxOption).filter_map(|node| node.accesskit_node().label()).collect();
    assert!(shown.first().is_some_and(|l| l.contains("Make a character from these words")), "{shown:?}");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    let stored = runner
        .get(&omuzaki_doc_api::DocPath::parse("characters").unwrap_or_else(|e| panic!("{e}")), &Grants::person())
        .unwrap_or_else(|e| panic!("{e}"));
    let made: Vec<String> = stored.and_then(|c| c.as_object().map(|m| m.keys().cloned().collect())).unwrap_or_default();
    assert!(made.iter().any(|id| id.contains("knight")), "{made:?}");
}
