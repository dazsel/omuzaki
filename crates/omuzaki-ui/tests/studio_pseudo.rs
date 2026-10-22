//! The pseudo-locale run over the whole studio (SE-03, #227): every workspace and every registered panel, with no
//! character open and with the kit character open, painted and spoken in the pseudo-locale. Every message there comes
//! out accented, so plain English in a frame was written into the code instead of a message table.
//!
//! What the document holds is the person's, not the studio's (a part called "hair", the character's name), and op ids
//! are never translated: a string made only of the document's own words is left alone.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Key, Modifiers, Ui, vec2};
use egui_kittest::Harness;
use omuzaki_char_api::{BuildServices, RegistryBuilder};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, Services};
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::set_strings;
use omuzaki_ui::ds::strings::{English, Pseudo, pseudo};
use omuzaki_ui::ds::testing::{all_strings, press_for};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Document, core_on};
use omuzaki_ui_api::registry::Registry;
use serde_json::json;

struct Studio {
    shell: Shell,
    runner: Arc<dyn Runner>,
}

/// The studio as `main.rs` builds it, in the pseudo-locale, with the kit character open when `populated`.
fn studio(populated: bool) -> Harness<'static, Studio> {
    omuzaki_ui::link();
    let core = core_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(&core.runner), None);
    if populated {
        let character = studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        // One step of the person's own, so History has a row by "You".
        let rename = Request {
            op: "character.rename".into(),
            args: json!({"character": studio::SAMPLE_ID, "name": "Kit"}),
            mode: Mode::Apply,
            actor: Actor::Person { name: "you".into() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        core.runner.run(rename).unwrap_or_else(|e| panic!("{e}"));
        shell.insert_view(EditFocus::new(character));
    }
    shell.insert_view(Agents(Arc::clone(&core.sessions)));
    shell.insert_view(ViewportServices {
        gpu: None,
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    let mut h = Harness::builder()
        .with_size(vec2(1920.0, 1080.0))
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui: &mut Ui, s: &mut Studio| s.shell.ui(ui), Studio { shell, runner: core.runner });
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(4);
    h
}

/// The words of `text`, lower-cased.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

/// Every word of every key and text in `value`.
fn words_of(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => into.extend(words(text)),
        Value::Array(items) => items.iter().for_each(|item| words_of(item, into)),
        Value::Object(map) => {
            for (key, item) in map {
                into.extend(words(key));
                words_of(item, into);
            }
        }
        _ => {}
    }
}

/// The plain English the frame shows or speaks, leaving out what is made only of the document's words.
fn english(h: &Harness<'_, Studio>) -> Vec<String> {
    let document = h.state().runner.get(&DocPath::root(), &Grants::person()).ok().flatten().unwrap_or(Value::Null);
    let mut data = BTreeSet::new();
    words_of(&document, &mut data);
    for op in h.state().runner.ops() {
        data.extend(words(op.name));
    }
    let strings = all_strings(h);
    plain_english(strings.iter().map(String::as_str))
        .into_iter()
        .filter(|text| !words(text).filter(|w| !w.chars().all(|c| c.is_ascii_digit())).all(|w| data.contains(&w)))
        .collect()
}

#[test]
fn every_workspace_and_panel_speaks_only_through_its_message_tables() {
    let registry = Registry::collect();
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut seen = 0;
    for populated in [false, true] {
        let mut h = studio(populated);
        let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (index, ws) in registry.workspaces().iter().enumerate() {
            press_for(&mut h, Modifiers::COMMAND, keys[index], 4);
            seen += all_strings(&h).len();
            found.extend(english(&h).into_iter().map(|text| format!("{} workspace: {text:?}", ws.id)));
        }
        press_for(&mut h, Modifiers::COMMAND, Key::Num1, 4);
        for def in registry.panels() {
            h.state_mut().shell.show_panel(def.id);
            h.run_steps(4);
            seen += all_strings(&h).len();
            found.extend(english(&h).into_iter().map(|text| format!("{} shown: {text:?}", def.id)));
            if populated && def.id == "history" {
                // The person's own steps are by "You", from the catalogue (#296).
                let you = pseudo("You");
                let rows = all_strings(&h);
                assert!(rows.iter().any(|row| row.starts_with(&you)), "no row by {you:?} in {rows:?}");
            }
        }
    }
    assert!(seen > 500, "the scan saw only {seen} strings");
    assert!(
        found.is_empty(),
        "plain English in the pseudo-locale:\n{}",
        found.into_iter().collect::<Vec<_>>().join("\n")
    );
}
