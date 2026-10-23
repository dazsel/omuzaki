//! The Console panel (#1097, `scripting.md` section 8): the editor above, the output below, a divider between.
//!
//! - **Editor:** line numbers, Rhai highlighting, the `script.check` problems underlined at their line and column
//!   300 ms after the last keystroke (on the UI clock), an op's summary on hover, and completions from the registry
//!   (after `ops::`, after `ops::<area>::`, inside a call's `#{`, and after `get("`), opened as you type or with
//!   Ctrl+Space. F8 and Shift+F8 go to the next and previous problem.
//! - **Running:** Ctrl+Enter runs, Ctrl+Shift+Enter dry-runs, Esc stops. The run goes through
//!   [`RunnerCalls`](omuzaki_script::RunnerCalls) as the person on a thread of its own, so the panel never waits; it is
//!   one transaction, one undo step, and a stopped or failed run is rolled back whole. A script someone else saved, or
//!   one that names an op reaching outside the studio, asks first ([`consent`](super::consent)).
//! - **Output:** what the script printed, its `log` lines muted, the error with its line (click to go there), and the
//!   run's step ("Undo: tidy, 4 ops", which opens History). A dry run lists each call it planned with its changes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::text::{CCursor, CCursorRange};
use egui::{
    Align, Id, Key, Layout, Modifiers, RichText, ScrollArea, Sense, TextEdit, Ui, UiBuilder, Vec2, WidgetInfo,
    WidgetType,
};
use omuzaki_ops_api::{Actor, Grants, OpError};
use omuzaki_script::{
    Cancel, CheckReport, Limits, OutputKind, Place, RunRequest, RunnerCalls, ScriptError, ScriptErrorKind,
    ScriptOutcome, content_hash, registered_host,
};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::{Map, Value, json};

use super::complete::{self, Completion, Completions, Detail, FieldKind};
use super::consent::{Author, Consent, consent};
use super::highlight;
use super::{Scripting, slug_of, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Button, EmptyState, StatusChip, TextField};
use crate::panels::edit::EditFocus;
use crate::panels::text::say;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// How long the source must stay the same before it is checked, in seconds.
const CHECK_AFTER: f64 = 0.3;
/// The editor's share of the panel's height when it opens.
const EDITOR_SHARE: f32 = 0.62;
/// The most completions shown at once.
const SHOWN: usize = 8;
/// The History panel, where a run's step is.
const HISTORY: &str = "history";
/// The editor's id, which holds its cursor.
pub const EDITOR_ID: &str = "script.console.editor";

/// A run on its thread.
struct Running {
    answer: Receiver<Result<ScriptOutcome, ScriptError>>,
    stop: Arc<AtomicBool>,
    dry: bool,
    name: String,
}

/// How the last run ended.
enum Ran {
    /// It finished.
    Done { outcome: ScriptOutcome, dry: bool, name: String },
    /// It failed, or was stopped, and was rolled back.
    Failed(ScriptError),
}

/// The completion list while it is open.
struct Popup {
    list: Completions,
    chosen: usize,
}

/// A run waiting on the consent card.
struct Asking {
    card: Consent,
    dry: bool,
    hash: String,
}

/// The console's state.
pub struct Console {
    /// Which open of [`Scripting`] the buffer holds.
    loaded: Option<u64>,
    /// The id and place it was saved under.
    saved: Option<(String, Place)>,
    /// What the last save said: the name it was saved as, or why not.
    save_note: Option<Result<String, String>>,
    author: Author,
    name: String,
    source: String,
    /// When the source last changed and wasn't checked since, on the UI clock.
    edited: Option<f64>,
    checking: Option<(String, Receiver<Result<CheckReport, OpError>>)>,
    /// The last report, with the source it was made for.
    report: Option<(String, CheckReport)>,
    running: Option<Running>,
    ran: Option<Ran>,
    popup: Option<Popup>,
    asking: Option<Asking>,
    share: f32,
    /// Where to put the cursor on the next frame, in characters.
    jump: Option<usize>,
    /// The name a run of an unnamed script gets, in the person's language.
    untitled: String,
    /// Whether the editor had the keyboard at the end of the last frame: egui drops focus when Esc is pressed, before
    /// the console reads the key.
    had_focus: bool,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            loaded: None,
            saved: None,
            save_note: None,
            author: Author::Me,
            name: String::new(),
            source: String::new(),
            edited: None,
            checking: None,
            report: None,
            running: None,
            ran: None,
            popup: None,
            asking: None,
            share: EDITOR_SHARE,
            jump: None,
            untitled: String::new(),
            had_focus: false,
        }
    }
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("name", &self.name)
            .field("running", &self.running.is_some())
            .finish_non_exhaustive()
    }
}

/// A stop the console sets from Esc or the Stop button.
struct Stop(Arc<AtomicBool>);

impl Cancel for Stop {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl Console {
    /// The source in the editor.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether a run is under way.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// The problems of the last check, when it was made for the source as it is now.
    pub fn problems(&self) -> Vec<omuzaki_script::Diagnostic> {
        match &self.report {
            Some((checked, report)) if *checked == self.source => report.problems(),
            _ => Vec::new(),
        }
    }

    /// Draws the console.
    pub fn ui(&mut self, ui: &mut Ui, scripting: &Scripting, actor: &Actor, ctx: Option<&PanelCtx<'_>>) {
        let ds = ds(ui.ctx());
        self.untitled = say(&ds, text::UNTITLED, &[]).into_owned();
        self.follow_open(scripting);
        self.collect(ui, scripting, actor);
        let id = Id::new(EDITOR_ID);
        let focused = ui.ctx().memory(|m| m.has_focus(id));
        // The console's keys, before the editor would take them.
        let (run, dry, complete, next, previous) = ui.input_mut(|i| {
            let dry = i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
            let run = i.consume_key(Modifiers::COMMAND, Key::Enter);
            let complete = focused && i.consume_key(Modifiers::COMMAND, Key::Space);
            let previous = focused && i.consume_key(Modifiers::SHIFT, Key::F8);
            let next = focused && i.consume_key(Modifiers::NONE, Key::F8);
            (run, dry, complete, next, previous)
        });
        if self.running.is_some()
            && (focused || self.had_focus || ui.ui_contains_pointer())
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.stop();
        }
        let toolbar = self.toolbar(ui, &ds);
        if run || toolbar.run {
            self.ask_to_run(scripting, actor, false);
        }
        if dry || toolbar.dry {
            self.ask_to_run(scripting, actor, true);
        }
        if toolbar.stop {
            self.stop();
        }
        if let Some(place) = toolbar.save {
            self.save(scripting, actor, place);
        }
        if next || previous {
            self.go_to_problem(ui, next);
        }
        self.problems_line(ui, &ds);
        let rest = ui.available_rect_before_wrap();
        let handle = 6.0;
        let editor_height =
            (rest.height() * self.share).clamp(80.0_f32.min(rest.height()), (rest.height() - 80.0).max(80.0));
        let editor_rect = egui::Rect::from_min_size(rest.min, Vec2::new(rest.width(), editor_height));
        let handle_rect = egui::Rect::from_min_size(editor_rect.left_bottom(), Vec2::new(rest.width(), handle));
        let output_rect = egui::Rect::from_min_max(handle_rect.left_bottom(), rest.max);
        ui.scope_builder(UiBuilder::new().max_rect(editor_rect), |ui| {
            self.editor(ui, &ds, scripting, ctx, complete);
        });
        self.divider(ui, &ds, handle_rect, rest.height());
        ui.scope_builder(UiBuilder::new().max_rect(output_rect), |ui| {
            self.output(ui, &ds, ctx.and_then(PanelCtx::view::<ShellRequests>));
        });
        ui.advance_cursor_after_rect(rest);
        self.consent_card(ui.ctx(), &ds, scripting, actor);
    }

    /// Loads the script [`Scripting`] opened last, once per open.
    fn follow_open(&mut self, scripting: &Scripting) {
        let (count, opened) = scripting.opened();
        if self.loaded == Some(count) {
            return;
        }
        self.loaded = Some(count);
        self.source = opened.source;
        self.name = opened.name;
        self.author = opened.author;
        self.saved = opened.saved;
        self.save_note = None;
        self.edited = Some(f64::NEG_INFINITY);
        self.report = None;
        self.popup = None;
    }

    /// Takes in a finished check or run, and starts a check that is due.
    fn collect(&mut self, ui: &Ui, scripting: &Scripting, actor: &Actor) {
        if let Some((source, answer)) = &self.checking {
            match answer.try_recv() {
                Ok(Ok(report)) => {
                    self.report = Some((source.clone(), report));
                    self.checking = None;
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.checking = None,
                Err(TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(50)),
            }
        }
        if let Some(running) = &self.running {
            match running.answer.try_recv() {
                Ok(answer) => {
                    let (dry, name) = (running.dry, running.name.clone());
                    self.ran = Some(match answer {
                        Ok(outcome) => Ran::Done { outcome, dry, name },
                        Err(error) => Ran::Failed(error),
                    });
                    self.running = None;
                }
                Err(TryRecvError::Disconnected) => self.running = None,
                Err(TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(50)),
            }
        }
        let now = ui.input(|i| i.time);
        if let Some(edited) = self.edited
            && self.checking.is_none()
        {
            if now - edited >= CHECK_AFTER {
                self.edited = None;
                self.check(scripting, actor);
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(CHECK_AFTER - (now - edited)));
            }
        }
    }

    /// Checks the source through `script.check` on a thread of its own.
    fn check(&mut self, scripting: &Scripting, actor: &Actor) {
        let (runner, actor) = (Arc::clone(scripting.runner()), actor.clone());
        let (tx, rx) = channel();
        let source = self.source.clone();
        let args = json!({ "source": source });
        let spawned = std::thread::Builder::new().name("script-check".into()).spawn(move || {
            let answer = super::run(runner.as_ref(), &actor, "script.check", args).and_then(|v| {
                serde_json::from_value::<CheckReport>(v).map_err(|e| OpError::internal(format!("script.check: {e}")))
            });
            let _ = tx.send(answer);
        });
        if spawned.is_ok() {
            self.checking = Some((source, rx));
        }
    }

    /// The script's name, Save, and the run, dry-run and stop buttons.
    fn toolbar(&mut self, ui: &mut Ui, ds: &Ds) -> Toolbar {
        let mut out = Toolbar::default();
        ui.horizontal(|ui| {
            let label = say(ds, text::SCRIPT_NAME, &[]);
            let untitled = say(ds, text::UNTITLED, &[]);
            let field =
                TextField::new(&label, &mut self.name).hint(&untitled).width(180.0).id_salt("script.console.name");
            ui.add(field);
            let save = say(ds, text::SAVE, &[]);
            let in_project = self.saved.as_ref().is_some_and(|(_, place)| *place == Place::Project);
            if ui.add(Button::new(&save).icon(icons::FLOPPY)).clicked() {
                out.save = Some(if in_project { Place::Project } else { Place::Mine });
            }
            if !in_project {
                let to_project = say(ds, text::SAVE_PROJECT, &[]);
                if ui.add(Button::new(&to_project).ghost()).clicked() {
                    out.save = Some(Place::Project);
                }
            }
            match &self.save_note {
                Some(Ok(name)) => {
                    ui.label(RichText::new(say(ds, text::SAVED, &[("name", name)])).color(ds.theme.ok));
                }
                Some(Err(why)) => {
                    ui.label(RichText::new(why).color(ds.theme.danger));
                }
                None => {}
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.running.is_some() {
                    let stop = say(ds, text::STOP, &[]);
                    out.stop =
                        ui.add(Button::new(&stop).danger().icon(icons::STOP).shortcut_hint(Key::Escape)).clicked();
                    ui.spinner();
                } else {
                    let empty = say(ds, text::NOTHING_TO_RUN, &[]);
                    let blank = self.source.trim().is_empty();
                    let run = say(ds, text::RUN, &[]);
                    let run_hint = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
                    let button = Button::new(&run).primary().icon(icons::PLAY).shortcut_hint(run_hint);
                    out.run = ui.add(button.disabled_if(blank, &empty)).clicked();
                    let dry = say(ds, text::DRY_RUN, &[]);
                    let dry_hint = egui::KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
                    let button = Button::new(&dry).icon(icons::EYE).shortcut_hint(dry_hint);
                    out.dry = ui.add(button.disabled_if(blank, &empty)).clicked();
                }
            });
        });
        out
    }

    /// The problem count and the first problem, which goes to its line when clicked.
    fn problems_line(&mut self, ui: &mut Ui, ds: &Ds) {
        let problems = self.problems();
        ui.horizontal(|ui| match problems.len() {
            0 if self.report.as_ref().is_some_and(|(s, _)| *s == self.source) && !self.source.trim().is_empty() => {
                ui.add(StatusChip::new(Tone::Ok, &say(ds, text::NO_PROBLEMS, &[])));
            }
            0 => {}
            n => {
                let count = if n == 1 {
                    say(ds, text::ONE_PROBLEM, &[])
                } else {
                    say(ds, text::PROBLEMS, &[("count", &n.to_string())])
                };
                ui.add(StatusChip::new(Tone::Danger, &count));
                let first = &problems[0];
                let line = say(
                    ds,
                    text::AT_LINE,
                    &[
                        ("line", &first.line.to_string()),
                        ("column", &first.column.to_string()),
                        ("message", &first.message),
                    ],
                );
                if ui.add(Button::new(&line).ghost()).clicked() {
                    self.jump = char_at(&self.source, first.line, first.column);
                }
            }
        });
    }

    /// The editor with its line numbers, highlighting, problems and completions.
    fn editor(&mut self, ui: &mut Ui, ds: &Ds, scripting: &Scripting, ctx: Option<&PanelCtx<'_>>, complete: bool) {
        let id = Id::new(EDITOR_ID);
        if let Some(text) = scripting.take_insert() {
            let at = cursor_of(ui.ctx(), id).unwrap_or_else(|| self.source.chars().count());
            let byte = byte_of(&self.source, at);
            self.source.insert_str(byte, &text);
            self.jump = Some(at + text.chars().count());
            self.edited = Some(ui.input(|i| i.time));
        }
        if let Some(at) = self.jump.take() {
            let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
            state.store(ui.ctx(), id);
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
        // The open list takes the arrows, Enter, Tab and Esc before the editor does.
        let mut accept = None;
        if let Some(popup) = &mut self.popup
            && ui.ctx().memory(|m| m.has_focus(id))
        {
            let shown = popup.list.items.len();
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    popup.chosen = (popup.chosen + 1) % shown;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    popup.chosen = (popup.chosen + shown - 1) % shown;
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                    accept = Some(popup.chosen);
                }
            });
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.popup = None;
            }
        }
        if let Some(chosen) = accept {
            self.accept(ui, chosen);
        }
        let problems: Vec<std::ops::Range<usize>> =
            self.problems().iter().filter_map(|p| highlight::problem_range(&self.source, p.line, p.column)).collect();
        let font = ds.text.mono.clone();
        let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap: f32| {
            let job = highlight::layout(ds, text.as_str(), &font, &problems, wrap);
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };
        let name = say(ds, text::EDITOR, &[]);
        let lines = self.source.split('\n').count().max(1);
        let mut output = None;
        ScrollArea::both().id_salt("script.console.scroll").auto_shrink([false; 2]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let numbers = (1..=lines).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
                ui.add(
                    egui::Label::new(RichText::new(numbers).font(ds.text.mono.clone()).color(ds.theme.text3))
                        .selectable(false),
                );
                let hint = say(ds, text::EDITOR_HINT, &[]);
                let edit = TextEdit::multiline(&mut self.source)
                    .id(id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(12)
                    .hint_text(hint.as_ref())
                    .layouter(&mut layouter);
                let shown = edit.show(ui);
                shown.response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, true, name.as_ref()));
                // A script is many lines; a screen reader says so.
                ui.ctx().accesskit_node_builder(shown.response.id, |node| {
                    node.set_role(egui::accesskit::Role::MultilineTextInput);
                });
                output = Some(shown);
            });
        });
        let Some(shown) = output else { return };
        self.had_focus = shown.response.has_focus();
        let now = ui.input(|i| i.time);
        if shown.response.changed() {
            self.edited = Some(now);
            self.refresh_popup(scripting, ctx, &shown, true);
        } else if complete {
            self.refresh_popup(scripting, ctx, &shown, false);
        } else if self.popup.is_some() && !shown.response.has_focus() {
            self.popup = None;
        }
        self.draw_popup(ui, ds, &shown);
        self.hover_summary(ui, ds, scripting, &shown);
    }

    /// Works out the completions at the cursor; `typed` when the text just changed (the list then opens only where
    /// something completes, and closes where nothing does).
    fn refresh_popup(
        &mut self,
        scripting: &Scripting,
        ctx: Option<&PanelCtx<'_>>,
        shown: &egui::text_edit::TextEditOutput,
        typed: bool,
    ) {
        let Some(cursor) = shown.cursor_range.map(|r| r.primary.index.0) else {
            self.popup = None;
            return;
        };
        let last = self.source.chars().nth(cursor.saturating_sub(1));
        let paths = paths_of(scripting, ctx);
        let found = complete::complete(&scripting.callables(), &self.source, cursor, &paths);
        // While typing, a list opens on a word, right after `::`, `"` or `{`, or at a call map's next key; Ctrl+Space
        // opens it anywhere something completes.
        let word = last.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '"' | '_' | '{'));
        let opens = !typed || self.popup.is_some() || word || after_map_open(&self.source, cursor);
        self.popup = found.filter(|_| opens).map(|list| Popup { list, chosen: 0 });
    }

    /// Puts completion `chosen` in place of the word being typed.
    fn accept(&mut self, ui: &Ui, chosen: usize) {
        let Some(popup) = self.popup.take() else { return };
        let Some(item) = popup.list.items.get(chosen) else { return };
        let id = Id::new(EDITOR_ID);
        let cursor = cursor_of(ui.ctx(), id).unwrap_or(popup.list.start);
        let (from, to) = (byte_of(&self.source, popup.list.start), byte_of(&self.source, cursor.max(popup.list.start)));
        self.source.replace_range(from..to, &item.insert);
        self.jump = Some(popup.list.start + item.insert.chars().count());
        self.edited = Some(ui.input(|i| i.time));
    }

    /// The completion list under the cursor.
    fn draw_popup(&mut self, ui: &mut Ui, ds: &Ds, shown: &egui::text_edit::TextEditOutput) {
        let Some(popup) = &self.popup else { return };
        let Some(range) = shown.cursor_range else { return };
        let at = shown.galley_pos + shown.galley.pos_from_cursor(range.primary).left_bottom().to_vec2();
        let mut clicked = None;
        let chosen = popup.chosen;
        let first = chosen.saturating_sub(SHOWN - 1);
        egui::Area::new(Id::new("script.console.completions"))
            .order(egui::Order::Foreground)
            .fixed_pos(at + Vec2::new(0.0, 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    for (i, item) in popup.list.items.iter().enumerate().skip(first).take(SHOWN) {
                        let detail = detail_words(ds, item);
                        let label = format!("{}  {detail}", item.label);
                        let row =
                            ui.selectable_label(i == chosen, RichText::new(&item.label).font(ds.text.mono.clone()));
                        let row = row.on_hover_text(&detail);
                        row.widget_info(|| {
                            WidgetInfo::selected(WidgetType::SelectableLabel, true, i == chosen, &label)
                        });
                        if row.clicked() {
                            clicked = Some(i);
                        }
                        if !detail.is_empty() {
                            ui.label(RichText::new(detail).font(ds.text.small.clone()).color(ds.theme.text2));
                        }
                    }
                });
            });
        if let Some(i) = clicked {
            self.accept(ui, i);
        }
    }

    /// The summary of the op under the pointer.
    fn hover_summary(&self, ui: &Ui, ds: &Ds, scripting: &Scripting, shown: &egui::text_edit::TextEditOutput) {
        let Some(pointer) = shown.response.hover_pos() else { return };
        let cursor = shown.galley.cursor_from_pos(pointer - shown.galley_pos);
        let at = byte_of(&self.source, cursor.index.0);
        let Some(call) = highlight::tokens(&self.source)
            .into_iter()
            .find(|(range, token)| *token == highlight::Token::Op && range.contains(&at))
            .map(|(range, _)| self.source[range].to_owned())
        else {
            return;
        };
        let callables = scripting.callables();
        let Some(op) = callables.iter().find(|op| call == format!("ops::{}::{}", op.area, op.verb)) else { return };
        let line = say(
            ds,
            text::OP_HOVER,
            &[("op", op.info.name), ("scope", &op.info.scope.to_string()), ("summary", op.info.summary)],
        );
        let _ = ui;
        shown.response.response.clone().on_hover_text_at_pointer(line.as_ref());
    }

    /// The divider between the editor and the output; dragging it moves the share.
    fn divider(&mut self, ui: &mut Ui, ds: &Ds, rect: egui::Rect, height: f32) {
        let response = ui.interact(rect, Id::new("script.console.divider"), Sense::drag());
        if response.dragged() && height > 0.0 {
            self.share = (self.share + response.drag_delta().y / height).clamp(0.15, 0.9);
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        let name = say(ds, text::DIVIDER, &[]);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, name.as_ref()));
        let colour = if response.hovered() || response.dragged() { ds.theme.accent } else { ds.theme.line };
        ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, colour));
    }

    /// What the last run printed and how it ended.
    fn output(&mut self, ui: &mut Ui, ds: &Ds, requests: Option<&ShellRequests>) {
        let mut jump = None;
        ScrollArea::vertical().id_salt("script.console.output").auto_shrink([false; 2]).stick_to_bottom(true).show(
            ui,
            |ui| match &self.ran {
                None if self.running.is_some() => {
                    ui.label(RichText::new(say(ds, text::RUNNING, &[])).color(ds.theme.text2));
                }
                None => {
                    ui.label(RichText::new(say(ds, text::OUTPUT_EMPTY, &[])).color(ds.theme.text2));
                }
                Some(Ran::Done { outcome, dry, name }) => {
                    lines(ui, ds, &outcome.output);
                    if *dry {
                        ui.add_space(space::S4);
                        ui.label(RichText::new(say(ds, text::DRY_DONE, &[])).color(ds.theme.text2));
                        for planned in &outcome.plans {
                            ui.label(RichText::new(&planned.op).font(ds.text.mono.clone()).color(ds.theme.text));
                            for change in &planned.plan.patch.changes {
                                let before = change.before.as_ref().map_or_else(|| "∅".to_owned(), short);
                                let after = change.after.as_ref().map_or_else(|| "∅".to_owned(), short);
                                let line = format!("  {}: {before} → {after}", change.path);
                                ui.label(RichText::new(line).font(ds.text.mono.clone()).color(ds.theme.text2));
                            }
                        }
                    } else if outcome.calls > 0 {
                        ui.add_space(space::S4);
                        let step = say(ds, text::UNDO_STEP, &[("name", name), ("count", &outcome.calls.to_string())]);
                        if ui.add(Button::new(&step).ghost().icon(icons::CLOCK)).clicked()
                            && let Some(requests) = requests
                        {
                            requests.push(ShellRequest::ShowPanel(HISTORY));
                        }
                    } else {
                        ui.label(RichText::new(say(ds, text::NOTHING_CHANGED, &[])).color(ds.theme.text2));
                    }
                }
                Some(Ran::Failed(error)) => {
                    lines(ui, ds, &error.output);
                    ui.add_space(space::S4);
                    if error.kind == ScriptErrorKind::Cancelled {
                        ui.label(RichText::new(say(ds, text::STOPPED, &[])).color(ds.theme.warn));
                    } else {
                        let line = say(
                            ds,
                            text::FAILED_AT,
                            &[
                                ("line", &error.line.to_string()),
                                ("column", &error.column.to_string()),
                                ("message", &error.message),
                            ],
                        );
                        let failed = Button::new(&line).ghost().icon(icons::WARNING_CIRCLE);
                        if ui.add(failed).clicked() {
                            jump = char_at(&self.source, error.line, error.column);
                        }
                        if let Some(hint) = error.op_error.as_ref().and_then(|e| e.hint.as_ref()) {
                            ui.label(RichText::new(hint).color(ds.theme.text2));
                        }
                        ui.label(RichText::new(say(ds, text::ROLLED_BACK, &[])).color(ds.theme.text2));
                    }
                }
            },
        );
        if jump.is_some() {
            self.jump = jump;
        }
    }

    /// Runs the script, or shows the consent card first when it needs one.
    fn ask_to_run(&mut self, scripting: &Scripting, actor: &Actor, dry: bool) {
        if self.running.is_some() || self.source.trim().is_empty() {
            return;
        }
        let hash = content_hash(&self.source);
        let report = registered_host(Limits::console()).check(&self.source);
        let named: Vec<String> = report.ops.iter().map(|op| op.name.clone()).collect();
        let remembered = scripting.consent().agreed(scripting.project(), &hash);
        // A dry run changes nothing and reaches nothing, so it never asks.
        match consent(&self.author, &named, &scripting.runner().ops(), remembered) {
            Some(card) if !dry => self.asking = Some(Asking { card, dry, hash }),
            _ => self.start(scripting, actor, dry),
        }
    }

    /// The consent card, while a run waits on it.
    fn consent_card(&mut self, ctx: &egui::Context, ds: &Ds, scripting: &Scripting, actor: &Actor) {
        let Some(asking) = &self.asking else { return };
        let (mut go, mut cancel) = (false, false);
        let modal = egui::Modal::new(Id::new("script.console.consent")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(RichText::new(say(ds, text::CONSENT_TITLE, &[])).font(ds.text.title.clone()));
            ui.add_space(space::S4);
            let who = match &asking.card.author {
                Author::Me => say(ds, text::BY_YOU, &[]),
                Author::Agent(id) => say(ds, text::BY_AGENT, &[("agent", id)]),
                Author::Person(name) => say(ds, text::BY_PERSON, &[("person", name)]),
                Author::Plugin(id) => say(ds, text::BY_PLUGIN, &[("plugin", id)]),
                Author::System => say(ds, text::BY_SYSTEM, &[]),
            };
            ui.label(RichText::new(who).color(ds.theme.text));
            if asking.card.reaches() {
                ui.label(RichText::new(say(ds, text::REACHES, &[])).color(ds.theme.warn));
            }
            ui.add_space(space::S4);
            ui.label(RichText::new(say(ds, text::CONSENT_CALLS, &[])).font(ds.text.strong.clone()));
            for op in &asking.card.ops {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&op.name).font(ds.text.mono.clone()).color(ds.theme.text));
                    let tone = if super::consent::REACHING.contains(&op.scope) { Tone::Warn } else { Tone::Info };
                    ui.add(StatusChip::new(tone, &op.scope.to_string()));
                });
                if !op.summary.is_empty() {
                    ui.label(RichText::new(&op.summary).font(ds.text.small.clone()).color(ds.theme.text2));
                }
            }
            ui.add_space(space::S8);
            ui.horizontal(|ui| {
                go = ui.add(Button::new(&say(ds, text::RUN, &[])).primary()).clicked();
                cancel = ui.add(Button::new(&say(ds, text::CANCEL, &[])).ghost()).clicked();
            });
        });
        if go {
            let (dry, hash) = (asking.dry, asking.hash.clone());
            scripting.consent().agree(scripting.project(), &hash);
            self.asking = None;
            self.start(scripting, actor, dry);
        } else if cancel || modal.should_close() {
            self.asking = None;
        }
    }

    /// Starts the run on a thread of its own.
    fn start(&mut self, scripting: &Scripting, actor: &Actor, dry: bool) {
        let name = if self.name.trim().is_empty() { self.untitled.clone() } else { self.name.clone() };
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let runner = Arc::clone(scripting.runner());
        let request = RunRequest {
            name: name.clone(),
            source: self.source.clone(),
            actor: actor.clone(),
            grants: Grants::person(),
            tx: None,
            params: Map::new(),
            dry_run: dry,
        };
        let (thread_actor, thread_stop) = (actor.clone(), Arc::clone(&stop));
        let spawned = std::thread::Builder::new().name("script-run".into()).spawn(move || {
            let calls = RunnerCalls::new(runner, request.name.clone(), thread_actor, Grants::person());
            let answer = registered_host(Limits::console()).run(request, &calls, &Stop(thread_stop));
            let _ = tx.send(answer);
        });
        if spawned.is_ok() {
            self.ran = None;
            self.running = Some(Running { answer: rx, stop, dry, name });
        }
    }

    /// Saves the script to `place` through `script.save` or `script.save_mine`, under its id there or one made from its
    /// name.
    fn save(&mut self, scripting: &Scripting, actor: &Actor, place: Place) {
        let name = if self.name.trim().is_empty() { self.untitled.clone() } else { self.name.trim().to_owned() };
        let id = match &self.saved {
            Some((id, saved)) if *saved == place => id.clone(),
            _ => slug_of(&name),
        };
        let op = match place {
            Place::Project => "script.save",
            Place::Mine => "script.save_mine",
        };
        let args = json!({ "id": id, "name": name, "source": self.source });
        match super::run(scripting.runner().as_ref(), actor, op, args) {
            Ok(_) => {
                self.saved = Some((id, place));
                self.author = Author::Me;
                self.name.clone_from(&name);
                self.save_note = Some(Ok(name));
            }
            Err(error) => self.save_note = Some(Err(error.message)),
        }
    }

    /// Stops the run under way; the host rolls it back.
    fn stop(&self) {
        if let Some(running) = &self.running {
            running.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Puts the cursor on the next (or previous) problem after (or before) it.
    fn go_to_problem(&mut self, ui: &Ui, next: bool) {
        let cursor = cursor_of(ui.ctx(), Id::new(EDITOR_ID)).unwrap_or(0);
        let mut at: Vec<usize> =
            self.problems().iter().filter_map(|p| char_at(&self.source, p.line, p.column)).collect();
        at.sort_unstable();
        at.dedup();
        let found = if next {
            at.iter().find(|a| **a > cursor).or_else(|| at.first())
        } else {
            at.iter().rev().find(|a| **a < cursor).or_else(|| at.last())
        };
        self.jump = found.copied();
    }
}

/// What the toolbar's buttons asked for.
#[derive(Default)]
struct Toolbar {
    run: bool,
    dry: bool,
    stop: bool,
    /// Save, to this place.
    save: Option<Place>,
}

/// The printed and logged lines of a run.
fn lines(ui: &mut Ui, ds: &Ds, output: &[omuzaki_script::OutputLine]) {
    for line in output {
        let colour = match line.kind {
            OutputKind::Print => ds.theme.text,
            OutputKind::Note => ds.theme.text2,
        };
        ui.label(RichText::new(&line.text).font(ds.text.mono.clone()).color(colour));
    }
}

/// A value in a line: compact JSON, cut at 80 characters.
fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 80 { format!("{}…", text.chars().take(79).collect::<String>()) } else { text }
}

/// The line a completion's list shows after its label.
fn detail_words(ds: &Ds, item: &Completion) -> String {
    match &item.detail {
        Detail::Area { ops } => say(ds, text::AREA_OPS, &[("count", &ops.to_string())]).into_owned(),
        Detail::Op { scope, summary } => format!("{scope} · {summary}"),
        Detail::Path => String::new(),
        Detail::Field(field) => {
            let kind = match field.kind {
                FieldKind::Number => say(ds, text::KIND_NUMBER, &[]),
                FieldKind::Integer => say(ds, text::KIND_INTEGER, &[]),
                FieldKind::Text => say(ds, text::KIND_TEXT, &[]),
                FieldKind::Bool => say(ds, text::KIND_BOOL, &[]),
                FieldKind::List => say(ds, text::KIND_LIST, &[]),
                FieldKind::Map => say(ds, text::KIND_MAP, &[]),
                FieldKind::Any => say(ds, text::KIND_ANY, &[]),
            };
            let mut parts = vec![kind.into_owned()];
            if field.required {
                parts.push(say(ds, text::REQUIRED, &[]).into_owned());
            }
            match (field.min, field.max) {
                (Some(min), Some(max)) => parts
                    .push(say(ds, text::RANGE, &[("min", &min.to_string()), ("max", &max.to_string())]).into_owned()),
                (Some(min), None) => parts.push(say(ds, text::AT_LEAST, &[("min", &min.to_string())]).into_owned()),
                (None, Some(max)) => parts.push(say(ds, text::AT_MOST, &[("max", &max.to_string())]).into_owned()),
                (None, None) => {}
            }
            if !field.options.is_empty() {
                parts.push(field.options.join(", "));
            }
            if let Some(doc) = &field.doc {
                parts.push(doc.clone());
            }
            parts.join(" · ")
        }
    }
}

/// Whether the cursor sits right after a call map's `#{ `, where its fields are offered at once.
fn after_map_open(source: &str, cursor: usize) -> bool {
    let before: String = source.chars().take(cursor).collect();
    before.trim_end().ends_with("#{") || before.trim_end().ends_with(',')
}

/// The document paths `get("` offers: the open character's node and its keys.
fn paths_of(scripting: &Scripting, ctx: Option<&PanelCtx<'_>>) -> Vec<String> {
    let Some(character) = ctx.and_then(PanelCtx::view::<EditFocus>).and_then(EditFocus::character) else {
        return Vec::new();
    };
    let root = format!("characters/{}", character.as_str());
    let mut out = vec![root.clone()];
    if let Ok(path) = omuzaki_doc_api::DocPath::parse(&root)
        && let Ok(Some(Value::Object(node))) = scripting.runner().get(&path, &Grants::person())
    {
        out.extend(node.keys().map(|key| format!("{root}/{key}")));
    }
    out
}

/// The editor's cursor, in characters.
fn cursor_of(ctx: &egui::Context, id: Id) -> Option<usize> {
    TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0)
}

/// The byte index of character `at` in `text`.
fn byte_of(text: &str, at: usize) -> usize {
    text.char_indices().nth(at).map_or(text.len(), |(byte, _)| byte)
}

/// The character index of 1-based `line` and `column`.
fn char_at(source: &str, line: u32, column: u32) -> Option<usize> {
    let range = highlight::problem_range(source, line, column)?;
    Some(source[..range.start].chars().count())
}

/// The Console as a registered panel.
#[derive(Debug, Default)]
pub struct ConsolePanel(pub Console);

impl Panel for ConsolePanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match ctx.view::<Scripting>() {
            Some(scripting) => super::padded(ui, |ui| self.0.ui(ui, scripting, ctx.actor(), Some(ctx))),
            None => no_scripting(ui),
        }
    }
}

/// What a Script panel shows in a studio started without scripting.
pub fn no_scripting(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let (headline, line) = (say(&ds, text::NO_SCRIPTING, &[]), say(&ds, text::NO_SCRIPTING_HOW, &[]));
    EmptyState::new(icons::CODE, &headline, &line).show(ui);
}

/// The template a call is inserted with: `ops::area::verb(#{ required: <empty of its kind>, … });`.
pub fn call_template(op: &complete::Callable) -> String {
    let fields: Vec<String> = complete::schema_fields(&op.info)
        .into_iter()
        .filter(|(_, f)| f.required)
        .map(|(name, field)| {
            let empty = match field.kind {
                FieldKind::Number => "0.0".to_owned(),
                FieldKind::Integer => "0".to_owned(),
                FieldKind::Text => field.options.first().map_or_else(|| "\"\"".to_owned(), |o| format!("\"{o}\"")),
                FieldKind::Bool => "false".to_owned(),
                FieldKind::List => "[]".to_owned(),
                FieldKind::Map => "#{}".to_owned(),
                FieldKind::Any => "()".to_owned(),
            };
            format!("{name}: {empty}")
        })
        .collect();
    format!("ops::{}::{}(#{{ {} }});\n", op.area, op.verb, fields.join(", "))
}
