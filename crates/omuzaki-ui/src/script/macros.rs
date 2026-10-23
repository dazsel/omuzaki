//! The Macros panel (#1221, `scripting.md` sections 7 and 8): record the person's own edits, list the saved macros with
//! their steps, replay one as one undo step, and save it: to My scripts, to the project, as a script, or as a library
//! item when it meets the library's safety rules.
//!
//! Recording is [`omuzaki_script::MacroRecorder`] over the runner's events (only the person's own kept edits; agents'
//! work, replays and scripts are never recorded), started here, from Ctrl+Shift+M anywhere, or from the palette; the
//! status bar shows "● REC" while it runs. Replay is [`omuzaki_script::play`]: one transaction named after the macro,
//! with the `Script` cause, rolled back whole on the first failure. Saving goes through `script.save_mine` and
//! `script.save`.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::{Actor, Grants, OpInfo, OpKind, Scope};
use omuzaki_script::{Macro, Place, PlayError, PlayReport};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::{Value, json};

use super::console::no_scripting;
use super::registration::CONSOLE_PANEL;
use super::{Body, Listed, Scripting, slug_of, source_gen, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Button, Section, StatusChip, TextField};
use crate::panels::text::say;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// The op that saves a library item (LB-03); the entry is offered only when the build has it.
pub const LIBRARY_SAVE: &str = "library.save_as_item";
/// The argument that names the character a step edits.
const TARGET: &str = "character";

/// Why a macro can't become a library item (library design D3: edits of `edit:character` or `edit:scene`, confined
/// to one target, of known ops).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotForLibrary {
    /// It has no steps.
    Empty,
    /// A step names an op this build lacks.
    Unknown(String),
    /// A step is not an edit, or edits beyond a character or the scene.
    Scope(String, Scope),
    /// Its steps edit more than one character.
    TwoTargets,
}

/// Whether `m` meets the library's three safety rules.
///
/// # Errors
/// The first rule it breaks.
pub fn library_check(m: &Macro, ops: &[OpInfo]) -> Result<(), NotForLibrary> {
    if m.steps.is_empty() {
        return Err(NotForLibrary::Empty);
    }
    let mut target: Option<&Value> = None;
    for step in &m.steps {
        let Some(info) = ops.iter().find(|i| i.name == step.op) else {
            return Err(NotForLibrary::Unknown(step.op.clone()));
        };
        if info.kind != OpKind::Edit || !matches!(info.scope, Scope::EditCharacter | Scope::EditScene) {
            return Err(NotForLibrary::Scope(step.op.clone(), info.scope));
        }
        if let Some(this) = step.args.get(TARGET) {
            match target {
                Some(seen) if seen != this => return Err(NotForLibrary::TwoTargets),
                _ => target = Some(this),
            }
        }
    }
    Ok(())
}

/// A replay on its thread.
type Replaying = Receiver<Result<PlayReport, PlayError>>;

/// The Macros panel's state.
#[derive(Default)]
pub struct Macros {
    listed: Vec<Listed>,
    read_at: Option<f64>,
    /// The name typed for the new recording.
    name: String,
    replaying: Option<Replaying>,
    /// What the last action did.
    said: Option<Said>,
}

/// What the last action did.
#[derive(Clone, Debug, PartialEq)]
enum Said {
    /// A replay played this many steps.
    Replayed(usize),
    /// A replay failed at a step, and was rolled back.
    ReplayFailed { step: usize, op: String, message: String },
    /// A macro was saved under this name.
    Saved(String),
    /// Something was refused, in the op's words.
    Failed(String),
}

impl std::fmt::Debug for Macros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Macros").field("listed", &self.listed.len()).finish_non_exhaustive()
    }
}

impl Macros {
    /// Draws the panel.
    pub fn ui(&mut self, ui: &mut Ui, scripting: &Scripting, actor: &Actor, requests: Option<&ShellRequests>) {
        let ds = ds(ui.ctx());
        let now = ui.input(|i| i.time);
        if self.read_at.is_none_or(|at| now - at >= 1.0) {
            self.read_at = Some(now);
            if let Ok(listed) = scripting.list(actor) {
                self.listed = listed.into_iter().filter(Listed::is_macro).collect();
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        self.collect();
        self.record_bar(ui, &ds, scripting, actor);
        if let Some(said) = &self.said {
            let (ok, line) = match said {
                Said::Replayed(steps) => (true, say(&ds, text::REPLAYED, &[("count", &steps.to_string())])),
                Said::ReplayFailed { step, op, message } => (
                    false,
                    say(&ds, text::REPLAY_FAILED, &[("step", &step.to_string()), ("op", op), ("message", message)]),
                ),
                Said::Saved(name) => (true, say(&ds, text::SAVED, &[("name", name)])),
                Said::Failed(message) => (false, message.as_str().into()),
            };
            ui.label(RichText::new(line.as_ref()).color(if ok { ds.theme.ok } else { ds.theme.danger }));
        }
        ui.add_space(space::S4);
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            if let Some(recorded) = scripting.recorded() {
                self.new_recording(ui, &ds, scripting, actor, &recorded, requests);
            }
            self.saved(ui, &ds, scripting, actor, requests);
        });
    }

    /// Takes in a finished replay.
    fn collect(&mut self) {
        let Some(answer) = &self.replaying else { return };
        match answer.try_recv() {
            Ok(Ok(report)) => {
                self.said = Some(Said::Replayed(report.steps));
                self.replaying = None;
            }
            Ok(Err(error)) => {
                let (step, op, message) = (error.step, error.op, error.error.message);
                self.said = Some(Said::ReplayFailed { step, op, message });
                self.replaying = None;
            }
            Err(TryRecvError::Disconnected) => self.replaying = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Record or Stop, and the REC chip while recording.
    fn record_bar(&mut self, ui: &mut Ui, ds: &Ds, scripting: &Scripting, actor: &Actor) {
        ui.horizontal(|ui| {
            let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::M);
            if scripting.recording() {
                ui.add(StatusChip::new(Tone::Danger, &say(ds, text::REC, &[])));
                let stop = say(ds, text::STOP_RECORDING, &[]);
                if ui.add(Button::new(&stop).danger().icon(icons::STOP).shortcut_hint(shortcut)).clicked() {
                    scripting.stop_recording(&say(ds, text::NEW_MACRO, &[]));
                }
            } else {
                let record = say(ds, text::RECORD, &[]);
                if ui.add(Button::new(&record).icon(icons::RECORD).shortcut_hint(shortcut)).clicked() {
                    match scripting.start_recording(actor) {
                        Ok(()) => self.said = None,
                        Err(error) => self.said = Some(Said::Failed(error.message)),
                    }
                }
            }
        });
        if scripting.recording() {
            ui.label(RichText::new(say(ds, text::RECORDING_HOW, &[])).color(ds.theme.text2));
        }
    }

    /// The last recording, with its name field and the ways to keep it.
    fn new_recording(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        scripting: &Scripting,
        actor: &Actor,
        recorded: &Macro,
        requests: Option<&ShellRequests>,
    ) {
        let title = say(ds, text::NEW_RECORDING, &[]);
        Section::new(&title).id_salt("script.macros.new").default_open(true).show(ui, |ui| {
            if scripting.full() {
                ui.label(RichText::new(say(ds, text::REC_FULL, &[])).color(ds.theme.warn));
            }
            if self.name.is_empty() {
                self.name.clone_from(&recorded.name);
            }
            let label = say(ds, text::MACRO_NAME, &[]);
            ui.add(TextField::new(&label, &mut self.name).id_salt("script.macros.name"));
            let named = Macro { name: self.name.trim().to_owned(), steps: recorded.steps.clone() };
            steps(ui, ds, &named, &scripting.runner().ops());
            if named.steps.is_empty() {
                ui.label(RichText::new(say(ds, text::NO_STEPS, &[])).color(ds.theme.text2));
            }
            let blank = named.name.is_empty() || named.steps.is_empty();
            let why = say(ds, text::NEEDS_NAME_AND_STEPS, &[]);
            ui.horizontal_wrapped(|ui| {
                let mine = say(ds, text::SAVE_MINE, &[]);
                if ui.add(Button::new(&mine).primary().disabled_if(blank, &why)).clicked() {
                    self.save(scripting, actor, &named, Place::Mine, true);
                }
                let project = say(ds, text::SAVE_PROJECT, &[]);
                if ui.add(Button::new(&project).disabled_if(blank, &why)).clicked() {
                    self.save(scripting, actor, &named, Place::Project, true);
                }
                let as_script = say(ds, text::SAVE_AS_SCRIPT, &[]);
                if ui.add(Button::new(&as_script).disabled_if(blank, &why)).clicked() {
                    self.save_as_script(scripting, actor, &named, requests);
                }
                let discard = say(ds, text::DISCARD, &[]);
                if ui.add(Button::new(&discard).ghost()).clicked() {
                    scripting.discard_recorded();
                    self.name.clear();
                }
            });
        });
        ui.add_space(space::S8);
    }

    /// The saved macros; the chosen one opens with its steps and actions.
    fn saved(&mut self, ui: &mut Ui, ds: &Ds, scripting: &Scripting, actor: &Actor, requests: Option<&ShellRequests>) {
        let title = say(ds, text::SAVED_MACROS, &[]);
        Section::new(&title).id_salt("script.macros.saved").default_open(true).show(ui, |ui| {
            if self.listed.is_empty() {
                ui.label(RichText::new(say(ds, text::NO_MACROS, &[])).color(ds.theme.text2));
            }
            let chosen = scripting.chosen_macro();
            let ops = scripting.runner().ops();
            for item in self.listed.clone() {
                let open = chosen.as_ref().is_some_and(|(id, place)| *id == item.id && *place == item.place);
                let place = match item.place {
                    Place::Project => say(ds, text::IN_PROJECT, &[]),
                    Place::Mine => say(ds, text::MINE, &[]),
                };
                let line = format!("{}  ·  {place}", item.title());
                if ui.selectable_label(open, RichText::new(line).color(ds.theme.text)).clicked() {
                    scripting.choose_macro((!open).then(|| (item.id.clone(), item.place)));
                }
                if !open {
                    continue;
                }
                let m = match scripting.body(&item) {
                    Ok(Body::Macro(m)) => m,
                    Ok(Body::Source(_)) => continue,
                    Err(error) => {
                        ui.label(RichText::new(error.message).color(ds.theme.danger));
                        continue;
                    }
                };
                ui.indent(("script.macros.steps", &item.id), |ui| {
                    steps(ui, ds, &m, &ops);
                    ui.horizontal_wrapped(|ui| {
                        let replay = say(ds, text::REPLAY, &[]);
                        let busy = say(ds, text::REPLAYING, &[]);
                        let button = Button::new(&replay).primary().icon(icons::PLAY);
                        if ui.add(button.disabled_if(self.replaying.is_some(), &busy)).clicked() {
                            self.replay(scripting, actor, m.clone());
                        }
                        let as_script = say(ds, text::SAVE_AS_SCRIPT, &[]);
                        if ui.add(Button::new(&as_script)).clicked() {
                            self.save_as_script(scripting, actor, &m, requests);
                        }
                        if item.place == Place::Mine {
                            let project = say(ds, text::SAVE_PROJECT, &[]);
                            if ui.add(Button::new(&project)).clicked() {
                                self.save(scripting, actor, &m, Place::Project, false);
                            }
                        }
                        library_button(ui, ds, &m, &ops, requests);
                    });
                });
                ui.add_space(space::S4);
            }
        });
    }

    /// Plays `m` on a thread of its own.
    fn replay(&mut self, scripting: &Scripting, actor: &Actor, m: Macro) {
        let (tx, rx) = channel();
        let (runner, actor) = (Arc::clone(scripting.runner()), actor.clone());
        let spawned = std::thread::Builder::new().name("macro-replay".into()).spawn(move || {
            let _ = tx.send(omuzaki_script::play(runner.as_ref(), &m, &actor, &Grants::person()));
        });
        if spawned.is_ok() {
            self.replaying = Some(rx);
            self.said = None;
        }
    }

    /// Saves `m` to `place`; `recording` when it is the new recording, which is then put away.
    fn save(&mut self, scripting: &Scripting, actor: &Actor, m: &Macro, place: Place, recording: bool) {
        let op = match place {
            Place::Project => "script.save",
            Place::Mine => "script.save_mine",
        };
        let args = json!({ "id": slug_of(&m.name), "name": m.name, "steps": m.steps });
        match super::run(scripting.runner().as_ref(), actor, op, args) {
            Ok(_) => {
                self.said = Some(Said::Saved(m.name.clone()));
                self.read_at = None;
                if recording {
                    scripting.discard_recorded();
                    self.name.clear();
                }
            }
            Err(error) => self.said = Some(Said::Failed(error.message)),
        }
    }

    /// Writes `m` as a script into My scripts and opens it in the console.
    fn save_as_script(&mut self, scripting: &Scripting, actor: &Actor, m: &Macro, requests: Option<&ShellRequests>) {
        let source = source_gen::macro_source(m, omuzaki_script::script_call);
        let id = format!("{}-script", slug_of(&m.name));
        let args = json!({ "id": id, "name": m.name, "source": source });
        match super::run(scripting.runner().as_ref(), actor, "script.save_mine", args) {
            Ok(_) => {
                let opened = super::Opened {
                    saved: Some((id, Place::Mine)),
                    name: m.name.clone(),
                    source,
                    author: super::consent::Author::Me,
                };
                scripting.open(opened);
                if let Some(requests) = requests {
                    requests.push(ShellRequest::ShowPanel(CONSOLE_PANEL));
                }
            }
            Err(error) => self.said = Some(Said::Failed(error.message)),
        }
    }
}

/// A macro's steps, one line each; a step naming an op this build lacks says so.
fn steps(ui: &mut Ui, ds: &Ds, m: &Macro, ops: &[OpInfo]) {
    for (i, step) in m.steps.iter().enumerate() {
        let known = ops.iter().any(|info| info.name == step.op);
        let line = format!("{}. {} {}", i + 1, step.op, short(&step.args));
        ui.label(RichText::new(line).font(ds.text.mono.clone()).color(if known {
            ds.theme.text
        } else {
            ds.theme.text2
        }));
        if !known {
            let missing = say(ds, text::UNAVAILABLE_OP, &[("op", &step.op)]);
            ui.label(RichText::new(missing).font(ds.text.small.clone()).color(ds.theme.danger));
        }
    }
}

/// "Save as library item": offered when the macro meets the library's rules and the build has the library's save
/// op, which opens on the palette's form with the steps filled in; otherwise disabled with the reason.
fn library_button(ui: &mut Ui, ds: &Ds, m: &Macro, ops: &[OpInfo], requests: Option<&ShellRequests>) {
    let label = say(ds, text::SAVE_LIBRARY, &[]);
    let reason = match library_check(m, ops) {
        Err(NotForLibrary::Empty) => Some(say(ds, text::LIB_EMPTY, &[]).into_owned()),
        Err(NotForLibrary::Unknown(op)) => Some(say(ds, text::UNAVAILABLE_OP, &[("op", &op)]).into_owned()),
        Err(NotForLibrary::Scope(op, scope)) => {
            Some(say(ds, text::LIB_SCOPE, &[("op", &op), ("scope", &scope.to_string())]).into_owned())
        }
        Err(NotForLibrary::TwoTargets) => Some(say(ds, text::LIB_TWO_TARGETS, &[]).into_owned()),
        Ok(()) if !ops.iter().any(|info| info.name == LIBRARY_SAVE) => {
            Some(say(ds, text::LIB_NOT_IN_BUILD, &[]).into_owned())
        }
        Ok(()) => None,
    };
    let button = Button::new(&label).icon(icons::FOLDER);
    let button = match &reason {
        Some(why) => button.disabled(why),
        None => button,
    };
    if ui.add(button).clicked()
        && let Some(requests) = requests
    {
        let args = json!({ "from": { "macro": { "name": m.name, "steps": m.steps } }, "title": m.name });
        requests.push(ShellRequest::Adjust { op: LIBRARY_SAVE.to_owned(), args });
    }
}

/// Arguments in a line: compact JSON, cut at 100 characters.
fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 100 { format!("{}…", text.chars().take(99).collect::<String>()) } else { text }
}

/// The Macros panel, registered.
#[derive(Debug, Default)]
pub struct MacrosPanel(pub Macros);

impl Panel for MacrosPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match ctx.view::<Scripting>() {
            Some(scripting) => {
                self.0.ui(ui, scripting, ctx.actor(), ctx.view::<ShellRequests>());
            }
            None => no_scripting(ui),
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{OpMeta, Undo};
    use omuzaki_script::RecordedCall;

    use super::*;

    fn op(name: &'static str, kind: OpKind, scope: Scope) -> OpInfo {
        OpInfo::new(OpMeta { name, summary: "", kind, scope, undo: Undo::Auto }, schemars::Schema::default())
    }

    fn of(steps: &[(&str, Value)]) -> Macro {
        Macro {
            name: "m".into(),
            steps: steps.iter().map(|(op, args)| RecordedCall { op: (*op).into(), args: args.clone() }).collect(),
        }
    }

    #[test]
    fn a_one_character_edit_macro_may_be_a_library_item_and_others_say_why_not() {
        let ops = [
            op("face.set_blush", OpKind::Edit, Scope::EditCharacter),
            op("stream.publish", OpKind::Edit, Scope::Publish),
        ];
        let one =
            of(&[("face.set_blush", json!({ "character": "aki" })), ("face.set_blush", json!({ "character": "aki" }))]);
        assert_eq!(library_check(&one, &ops), Ok(()));
        let two =
            of(&[("face.set_blush", json!({ "character": "aki" })), ("face.set_blush", json!({ "character": "ren" }))]);
        assert_eq!(library_check(&two, &ops), Err(NotForLibrary::TwoTargets));
        let publish = of(&[("stream.publish", json!({}))]);
        assert_eq!(library_check(&publish, &ops), Err(NotForLibrary::Scope("stream.publish".into(), Scope::Publish)));
        assert_eq!(library_check(&of(&[("gone.op", json!({}))]), &ops), Err(NotForLibrary::Unknown("gone.op".into())));
        assert_eq!(library_check(&of(&[]), &ops), Err(NotForLibrary::Empty));
    }
}
