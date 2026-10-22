//! The Models page of the Settings window (spec ML-13, ADR 0016, #688): per slot, what runs and why, the shipped model
//! and the models the person added, each with its declared licence, where it stands and its smoke test; the "not from
//! Omuzaki" notice to accept; a switch to any ready model and back to the shipped one; removing an added model behind
//! a confirmation; and adding one by dropping its file on the slot.
//!
//! Like the settings page it holds nothing of its own: it reads `model.list` (when it opens, after each of its own
//! changes, and once a second, so an agent's switch shows) and every change is a `model.*` op run as the person. Adding
//! is a job that copies, probes and smoke-tests the file in a sandboxed worker, so it runs on a thread of its own while
//! the page shows its row; each dropped file's folder is the root the person grants for it (ADR 0013).

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Context, RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType, vec2};
use omuzaki_ai::ops::list::{Listed, Listing};
use omuzaki_ai_api::{Better, DISCLAIMER, EvalReport, ModelStatus, Origin, SlotStatus};
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use serde_json::{Value, json};

use super::text::{Word, say, title};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Banner, Button, EmptyState, GateChip, JobRow, JobState, StatusChip};

/// Seconds between reads while the page is open.
const REFRESH_S: f64 = 1.0;

/// A model being added: its slot, its file's name, and how its job stands.
struct Adding {
    slot: String,
    name: String,
    state: AddState,
}

/// An add's job: running, with where its answer arrives, or failed, with why. A finished add leaves the list and shows
/// as its model's card.
enum AddState {
    Running(Receiver<Result<Value, OpError>>),
    Failed(String),
}

/// What the person asked for this frame.
enum Action {
    Use { slot: String, model: Option<String> },
    Accept(String),
    AskRemove(String),
    Remove(String),
    CancelRemove,
    Add { slot: String, files: Vec<String> },
}

/// The Models page's state, kept while the Settings window is open.
#[derive(Default)]
pub struct ModelsPage {
    listing: Option<Listing>,
    read_at: Option<f64>,
    /// Why `model.list` failed (no registry in this build, for one).
    unreadable: Option<String>,
    /// Why the last change was refused.
    refused: Option<String>,
    /// The model whose removal waits for the person's confirmation.
    removing: Option<String>,
    adding: Vec<Adding>,
}

impl ModelsPage {
    /// Whether a model is still being added.
    pub fn adding(&self) -> bool {
        self.adding.iter().any(|add| matches!(add.state, AddState::Running(_)))
    }

    /// Waits for every add in flight to finish and takes its answer, as a later frame would. For tests: the page itself
    /// never blocks.
    pub fn finish_adding(&mut self) {
        self.take_answers(|rx| Some(rx.recv().unwrap_or_else(|_| Err(OpError::internal("the add stopped")))));
        self.read_at = None;
    }

    /// Draws the page; `now` is egui's clock.
    pub fn show(&mut self, ui: &mut Ui, runner: &Arc<dyn Runner>, actor: &Actor, now: f64) {
        let ctx = ui.ctx().clone();
        if self.poll() || self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(runner.as_ref(), actor, now);
        }
        if self.adding() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(reason) = &self.refused {
            ui.label(RichText::new(say(&ctx, Word::NotSaved, &[("reason", reason)])).color(ds(&ctx).theme.danger));
        }
        let Some(listing) = &self.listing else {
            let reason = self.unreadable.clone().unwrap_or_default();
            let headline = say(&ctx, Word::ModelsUnavailable, &[]);
            EmptyState::new(icons::GEAR, &headline, &say(&ctx, Word::UnavailableLine, &[("reason", &reason)])).show(ui);
            return;
        };
        if listing.slots.is_empty() {
            let headline = say(&ctx, Word::ModelsNone, &[]);
            EmptyState::new(icons::GEAR, &headline, &say(&ctx, Word::ModelsNoneLine, &[])).show(ui);
            return;
        }
        let mut actions = Vec::new();
        ScrollArea::vertical().id_salt("settings.models.scroll").auto_shrink([false, false]).show(ui, |ui| {
            for (index, slot) in listing.slots.iter().enumerate() {
                if index > 0 {
                    ui.add_space(space::S12);
                }
                draw_slot(ui, slot, listing, &self.adding, self.removing.as_deref(), &mut actions);
            }
        });
        for action in actions {
            self.act(runner, actor, action, now);
        }
    }

    /// Takes the answers of finished adds; true when one arrived, so the list is read again.
    fn poll(&mut self) -> bool {
        self.take_answers(|rx| match rx.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(OpError::internal("the add stopped"))),
        })
    }

    /// Takes each running add's answer from `answer` (`None`: not yet): one that worked leaves the list, since its
    /// model's card takes its place; one that failed stays, saying why. True when an answer arrived.
    fn take_answers(
        &mut self,
        answer: impl Fn(&Receiver<Result<Value, OpError>>) -> Option<Result<Value, OpError>>,
    ) -> bool {
        let mut arrived = false;
        self.adding.retain_mut(|add| {
            let AddState::Running(rx) = &add.state else { return true };
            match answer(rx) {
                None => true,
                Some(Ok(_)) => {
                    arrived = true;
                    false
                }
                Some(Err(error)) => {
                    arrived = true;
                    add.state = AddState::Failed(error.message);
                    true
                }
            }
        });
        arrived
    }

    fn act(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, action: Action, now: f64) {
        let result = match action {
            Action::Use { slot, model } => {
                run(runner.as_ref(), actor, Grants::person(), "model.select", json!({ "slot": slot, "model": model }))
            }
            Action::Accept(model) => {
                run(runner.as_ref(), actor, Grants::person(), "model.acknowledge", json!({ "model": model }))
            }
            Action::AskRemove(model) => {
                self.removing = Some(model);
                return;
            }
            Action::CancelRemove => {
                self.removing = None;
                return;
            }
            Action::Remove(model) => {
                self.removing = None;
                run(runner.as_ref(), actor, Grants::person(), "model.remove", json!({ "model": model }))
            }
            Action::Add { slot, files } => {
                self.add(runner, actor, slot, &files);
                return;
            }
        };
        self.refused = result.err().map(|error| error.message);
        self.read(runner.as_ref(), actor, now);
    }

    /// Starts `model.add` for the dropped `files` on a thread of its own: a `model.toml` among them is the manifest,
    /// the first other file the model's main file and the rest its other files.
    fn add(&mut self, runner: &Arc<dyn Runner>, actor: &Actor, slot: String, files: &[String]) {
        let is_manifest = |f: &&String| Path::new(f.as_str()).file_name().is_some_and(|n| n == "model.toml");
        let manifest = files.iter().find(is_manifest).cloned();
        let mut model = files.iter().filter(|f| !is_manifest(f)).cloned();
        let Some(path) = model.next() else { return };
        let more: Vec<String> = model.collect();
        let name = Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        // The person picked these files by dropping them, so they share them (ADR 0013). The fence opens a file through
        // the root folder it lies in, so the root is each file's folder until a root can be a single file.
        let grants = files
            .iter()
            .filter_map(|f| Path::new(f).parent())
            .fold(Grants::person(), |grants, folder| grants.with_fs_root(folder.display().to_string()));
        let args = json!({ "slot": slot, "path": path, "more": more, "manifest": manifest });
        let (tx, rx) = channel();
        let (runner, actor) = (Arc::clone(runner), actor.clone());
        let spawned = std::thread::Builder::new().name("model-add".into()).spawn(move || {
            // The page may have closed by the time the job ends; nobody is listening then, and that is fine.
            let _ = tx.send(run(runner.as_ref(), &actor, grants, "model.add", args));
        });
        self.adding.retain(|add| add.slot != slot);
        let state = spawned.map_or_else(|e| AddState::Failed(e.to_string()), |_| AddState::Running(rx));
        self.adding.push(Adding { slot, name, state });
    }

    /// Reads `model.list`.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        let answer = run(runner, actor, Grants::person(), "model.list", json!({}));
        match answer.and_then(|v| {
            serde_json::from_value::<Listing>(v).map_err(|e| OpError::internal(format!("model.list's answer: {e}")))
        }) {
            Ok(listing) => {
                self.listing = Some(listing);
                self.unreadable = None;
            }
            Err(error) => {
                self.listing = None;
                self.unreadable = Some(error.message);
            }
        }
    }
}

/// Runs `op` as the person with `grants` and returns its answer.
fn run(runner: &dyn Runner, actor: &Actor, grants: Grants, op: &str, args: Value) -> Result<Value, OpError> {
    let outcome = runner.run(Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants,
        cause: None,
        tx: None,
        base: None,
    })?;
    Ok(outcome.plan.output.unwrap_or(Value::Null))
}

/// One slot: what it does, what runs, its models, the adds in flight for it, and the drop target.
fn draw_slot(
    ui: &mut Ui,
    slot: &SlotStatus,
    listing: &Listing,
    adding: &[Adding],
    removing: Option<&str>,
    actions: &mut Vec<Action>,
) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let slot_ref = slot.info.slot.to_string();
    ui.label(RichText::new(title(&ctx, &slot.info.summary)).font(ds.text.section.clone()));
    let models: Vec<&Listed> = listing.models.iter().filter(|m| m.entry.slot == slot.info.slot).collect();
    let running = slot.active.as_ref().and_then(|active| models.iter().find(|m| &m.entry.model == active));
    let line = match running {
        Some(model) => say(&ctx, Word::Running, &[("name", &model.entry.name)]),
        None => say(&ctx, Word::NothingRuns, &[("without", &title(&ctx, &slot.info.without_model))]),
    };
    ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
    if let Some(reason) = &slot.fallback {
        ui.add(StatusChip::new(Status::Warn, &say(&ctx, Word::FellBack, &[("reason", reason)])));
    }
    ui.add_space(space::S4);
    for model in &models {
        draw_model(ui, model, removing, actions);
        ui.add_space(space::S4);
    }
    if slot.selected.is_some() {
        let back = say(&ctx, Word::UseShipped, &[]);
        if ui.add(Button::new(&back).op("model.select")).clicked() {
            actions.push(Action::Use { slot: slot_ref.clone(), model: None });
        }
    }
    for add in adding.iter().filter(|add| add.slot == slot_ref) {
        let heading = say(&ctx, Word::Adding, &[("name", &add.name)]);
        match &add.state {
            AddState::Running(_) => {
                JobRow::new(&heading, JobState::Running { progress: None }).id_salt(("model.add", &add.slot)).show(ui);
            }
            AddState::Failed(reason) => {
                ui.add(StatusChip::new(
                    Status::Danger,
                    &say(&ctx, Word::AddFailed, &[("name", &add.name), ("reason", reason)]),
                ));
            }
        }
    }
    if let Some(files) = drop_files(ui, &say(&ctx, Word::Drop, &[])) {
        actions.push(Action::Add { slot: slot_ref, files });
    }
}

/// One model's card: its name and version, where it came from, its licence and size, where it stands, its smoke test,
/// the warnings it carries, and what the person can do with it.
fn draw_model(ui: &mut Ui, model: &Listed, removing: Option<&str>, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let entry = &model.entry;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, ds.theme.line))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(entry.name.as_str()).font(ds.text.strong.clone()));
                ui.label(
                    RichText::new(entry.model.version.to_string()).font(ds.text.mono.clone()).color(ds.theme.text2),
                );
                if entry.active {
                    ui.add(StatusChip::new(Status::Ok, &say(&ctx, Word::InUse, &[])));
                }
            });
            let origin = match entry.origin {
                Origin::Shipped => Word::Shipped,
                Origin::User => Word::Yours,
                Origin::Catalogue => Word::Catalogue,
            };
            let facts = [say(&ctx, origin, &[]), size_text(&ctx, entry.size)].join(" · ");
            ui.label(RichText::new(facts).font(ds.text.small.clone()).color(ds.theme.text2));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = space::S6;
                match licence_text(entry) {
                    Some(licence) => {
                        ui.label(
                            RichText::new(say(&ctx, Word::Licence, &[("licence", &licence)]))
                                .font(ds.text.small.clone())
                                .color(ds.theme.text2),
                        );
                    }
                    None => {
                        ui.add(StatusChip::new(Status::Warn, &say(&ctx, Word::LicenceUnknown, &[])));
                    }
                }
                // "In use" already says a running model is ready.
                if !(entry.active && entry.status == ModelStatus::Ready) {
                    let (status, words) = status_words(&ctx, &entry.status);
                    ui.add(StatusChip::new(status, &words));
                }
            });
            if let Some(eval) = &entry.eval {
                draw_smoke(ui, eval);
            }
            if let Some(warning) = &model.warning {
                Banner::new(Status::Warn, &title(&ctx, warning)).show(ui);
            }
            if entry.status == ModelStatus::NeedsAcknowledgement {
                ui.add_space(space::S4);
                ui.label(RichText::new(say(&ctx, Word::NotFromUs, &[])).font(ds.text.strong.clone()));
                ui.add(egui::Label::new(RichText::new(title(&ctx, DISCLAIMER)).font(ds.text.small.clone())).wrap());
                let accept = say(&ctx, Word::Accept, &[]);
                if ui.add(Button::new(&accept).primary().op("model.acknowledge")).clicked() {
                    actions.push(Action::Accept(model.key.clone()));
                }
            }
            if removing == Some(model.key.as_str()) {
                ui.add_space(space::S4);
                ui.label(
                    RichText::new(say(&ctx, Word::RemoveAsk, &[("name", &entry.name)])).font(ds.text.strong.clone()),
                );
                ui.label(
                    RichText::new(say(&ctx, Word::RemoveLine, &[])).font(ds.text.small.clone()).color(ds.theme.text2),
                );
                ui.horizontal(|ui| {
                    let remove = say(&ctx, Word::Remove, &[]);
                    if ui.add(Button::new(&remove).danger().op("model.remove")).clicked() {
                        actions.push(Action::Remove(model.key.clone()));
                    }
                    if ui.add(Button::new(&say(&ctx, Word::Cancel, &[]))).clicked() {
                        actions.push(Action::CancelRemove);
                    }
                });
                return;
            }
            let usable = entry.status == ModelStatus::Ready && !entry.active;
            let removable = entry.origin == Origin::User;
            if usable || removable {
                ui.horizontal(|ui| {
                    if usable && ui.add(Button::new(&say(&ctx, Word::Use, &[])).op("model.select")).clicked() {
                        let slot = entry.slot.to_string();
                        actions.push(Action::Use { slot, model: Some(model.key.clone()) });
                    }
                    if removable && ui.add(Button::new(&say(&ctx, Word::Remove, &[])).ghost()).clicked() {
                        actions.push(Action::AskRemove(model.key.clone()));
                    }
                });
            }
        });
}

/// A model's latest evaluation: passed or which checks failed, then each check's number against its limit.
fn draw_smoke(ui: &mut Ui, eval: &EvalReport) {
    let ctx = ui.ctx().clone();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S6;
        if eval.passed() {
            ui.add(StatusChip::new(Status::Ok, &say(&ctx, Word::SmokePassed, &[])));
        } else {
            let failed: Vec<&str> = eval.failures().map(|c| c.metric.name.as_str()).collect();
            let checks = failed.join(", ");
            ui.add(StatusChip::new(Status::Warn, &say(&ctx, Word::SmokeFailed, &[("checks", &checks)])));
        }
        for check in &eval.checks {
            let value = format!("{:.3}", check.metric.value);
            if check.passes() {
                ui.add(GateChip::passed(&check.metric.name, &value));
            } else {
                let floor = check.floor.map(|f| format!("{f:.3}")).unwrap_or_default();
                let chip = GateChip::failed(&check.metric.name, &value, &floor);
                ui.add(if check.metric.better == Better::Lower { chip.above() } else { chip });
            }
        }
    });
}

/// The licence as declared, in one line; `None` when nothing was.
fn licence_text(entry: &omuzaki_ai_api::ModelEntry) -> Option<String> {
    let licence = &entry.licence;
    licence.spdx.clone().or_else(|| licence.text.clone()).or_else(|| licence.url.clone())
}

/// Where a model stands, as a chip's status and words.
fn status_words(ctx: &Context, status: &ModelStatus) -> (Status, String) {
    match status {
        ModelStatus::Ready => (Status::Ok, say(ctx, Word::Ready, &[])),
        ModelStatus::NotInstalled => (Status::Info, say(ctx, Word::NotDownloaded, &[])),
        ModelStatus::NeedsAcknowledgement => (Status::Warn, say(ctx, Word::NeedsAcceptance, &[])),
        ModelStatus::NeedsConsent => (Status::Warn, say(ctx, Word::NeedsConsent, &[])),
        ModelStatus::Disabled { reason } => (Status::Danger, say(ctx, Word::Disabled, &[("reason", reason)])),
        _ => (Status::Danger, say(ctx, Word::Withdrawn, &[])),
    }
}

/// A size in bytes as people read it: kB below a megabyte, MB below a gigabyte, GB above, one decimal under ten.
fn size_text(ctx: &Context, bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a size is shown to one decimal")]
    let bytes = bytes as f64;
    let (amount, word) = if bytes >= 1e9 {
        (bytes / 1e9, Word::SizeGb)
    } else if bytes >= 1e6 {
        (bytes / 1e6, Word::SizeMb)
    } else {
        (bytes / 1e3, Word::SizeKb)
    };
    let size = if amount < 10.0 { format!("{amount:.1}") } else { format!("{amount:.0}") };
    say(ctx, word, &[("size", &size)])
}

/// A target that takes dropped files; their paths when some are dropped while the pointer is over it.
fn drop_files(ui: &mut Ui, label: &str) -> Option<Vec<String>> {
    let ds = ds(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) && response.contains_pointer();
    let stroke = if hovering { ds.theme.accent } else { ds.theme.line };
    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, ds.text.small.clone(), ds.theme.text2);
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, label), |_| {});
    if !response.contains_pointer() {
        return None;
    }
    let files: Vec<String> = ui.ctx().input(|i| {
        i.raw.dropped_files.iter().map(|file| file.path().display().to_string()).filter(|p| !p.is_empty()).collect()
    });
    (!files.is_empty()).then_some(files)
}
