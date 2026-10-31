//! The merge view (`docs/design/branches.md` 7.4 and 7.8; #445).
//!
//! Opening it runs `branch.merge` as a dry run: the preview, unit by unit (a part, the face, the palette, a scene).
//! A unit both sides changed in the same place, or whose result doesn't hold together, needs a choice: its card shows
//! the character as each side has it at one camera, the parameters with their base, mine and theirs values, and Keep
//! mine, Take theirs, Keep both (parts) or Choose per parameter. Nothing is chosen for the person, and Merge waits until
//! every card has a choice. The other units merge cleanly; unticking one leaves it out ("Merge selected"), which makes
//! the merge a pick that the next merge offers again.
//!
//! Merge runs `branch.merge` once, with the choices as `resolve`, the unticked units as `exclude` and the previewed
//! version as `expect`: one transaction, one undo step, in History like any edit. If the folder branch changed since
//! the preview the merge is refused and the view offers to preview again.
//!
//! The result is drawn as the choices change: each change of choices runs the dry run again with them, and its patch
//! applied to the folder branch's tree is what Merge would make. The common base is drawn beside mine and theirs on
//! request.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use egui::{Context, Id, Key, Modifiers, RichText, TextureHandle, TextureOptions, Ui, vec2};
use omuzaki_char_api::CharId;
use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::{BranchInfo, PointId};
use omuzaki_ops_api::{ErrorCode, Grants, Mode, Request, Runner};
use serde_json::json;

use super::compare::key_of;
use super::pictures::Picture;
use super::{Branching, Camera, PICTURE, Shown, person};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, Section};
use crate::shell::text::{Word, say};

/// The op the view previews and applies.
pub const OP: &str = "branch.merge";

/// One parameter of a unit, as the preview lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// The document path.
    pub path: String,
    /// Its value at the common base.
    pub base: Option<Value>,
    /// Its value here.
    pub mine: Option<Value>,
    /// Its value on the branch merged in.
    pub theirs: Option<Value>,
}

/// One unit of the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// Its key (`characters/kit/parts/hair`), which `resolve` and `exclude` name.
    pub key: String,
    /// Its name.
    pub title: String,
    /// `part`, `face`, `palette`, `scene`…
    pub kind: String,
    /// It needs a choice: both sides changed it in the same place, or its result doesn't hold together.
    pub needs_choice: bool,
    /// It brings something of theirs (a unit only mine changed has nothing to merge).
    pub offers: bool,
    /// Its parameters.
    pub changes: Vec<Change>,
    /// What the preview says about it, if anything.
    pub note: Option<String>,
}

impl Unit {
    /// The character it belongs to, if any.
    pub fn character(&self) -> Option<CharId> {
        let mut segments = self.key.split('/');
        (segments.next() == Some("characters")).then(|| segments.next().and_then(|id| CharId::new(id).ok())).flatten()
    }
}

/// A choice for a unit that needs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep the folder branch's unit.
    Mine,
    /// Take the other branch's unit.
    Theirs,
    /// Keep mine and add theirs as a disabled alternative part.
    Both,
    /// One side per conflicting parameter.
    PerParameter,
}

/// The dry run's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The units, in key order.
    pub units: Vec<Unit>,
    /// The folder branch's document version it was made at.
    pub version: Value,
    /// Its one-line summary.
    pub summary: String,
    /// Rights notes, if any.
    pub rights: Vec<String>,
    /// The common base point, when there is one.
    pub base: Option<PointId>,
}

/// Where the view stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// The person is choosing.
    Choosing,
    /// The merge applied.
    Merged,
    /// The merge was undone.
    Undone,
    /// The folder branch changed since the preview: preview again.
    Stale(String),
    /// The merge was refused for another reason.
    Failed(String),
}

/// The merge view's state.
pub struct MergeView {
    from: BranchInfo,
    into: String,
    at: Option<PointId>,
    preview: Result<Preview, String>,
    choices: BTreeMap<String, Choice>,
    params: BTreeMap<String, BTreeMap<String, bool>>,
    left_out: BTreeSet<String>,
    phase: Phase,
    mine: Result<Arc<Value>, String>,
    theirs: Result<Arc<Value>, String>,
    /// The base's tree, once asked for.
    base: Option<Result<Arc<Value>, String>>,
    show_base: bool,
    /// What Merge would make with the choices as they stand, and the arguments it was made with.
    result: Option<(Value, Result<Arc<Value>, String>)>,
    textures: BTreeMap<String, TextureHandle>,
}

impl std::fmt::Debug for MergeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeView").field("from", &self.from.name).field("phase", &self.phase).finish_non_exhaustive()
    }
}

impl MergeView {
    /// Previews merging `from` (at `at`, a reviewed point, or its head) into the folder branch `into`.
    pub fn open(
        from: BranchInfo,
        at: Option<PointId>,
        into: String,
        branching: &Branching,
        runner: &dyn Runner,
    ) -> Self {
        let mine = Shown::Now.tree(runner, branching.trees.as_ref());
        let theirs = Shown::Point(at.unwrap_or(from.head)).tree(runner, branching.trees.as_ref());
        let mut view = Self {
            preview: Err(String::new()),
            from,
            into,
            at,
            choices: BTreeMap::new(),
            params: BTreeMap::new(),
            left_out: BTreeSet::new(),
            phase: Phase::Choosing,
            mine,
            theirs,
            base: None,
            show_base: false,
            result: None,
            textures: BTreeMap::new(),
        };
        view.preview(runner);
        view
    }

    /// The preview, or why it couldn't be made.
    ///
    /// # Errors
    /// The dry run's refusal, in words.
    pub fn previewed(&self) -> Result<&Preview, &str> {
        self.preview.as_ref().map_err(String::as_str)
    }

    /// Where the view stands.
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Chooses `choice` for the unit `key`.
    pub fn choose(&mut self, key: &str, choice: Choice) {
        self.choices.insert(key.to_owned(), choice);
    }

    /// Leaves the clean unit `key` out, or takes it again.
    pub fn leave_out(&mut self, key: &str, out: bool) {
        if out {
            self.left_out.insert(key.to_owned());
        } else {
            self.left_out.remove(key);
        }
    }

    /// What Merge would make with the choices as they stand: the folder branch's tree with the dry run's patch.
    pub fn result(&self) -> Option<&Result<Arc<Value>, String>> {
        self.result.as_ref().map(|(_, tree)| tree)
    }

    /// Makes the result again when the choices changed since it was made.
    fn follow_choices(&mut self, runner: &dyn Runner) {
        let args = self.args();
        if self.preview.is_err() || self.result.as_ref().is_some_and(|(made, _)| *made == args) {
            return;
        }
        let tree = match (&self.mine, run(runner, args.clone(), Mode::DryRun)) {
            (Ok(mine), Ok(plan)) => {
                let mut tree = mine.as_ref().clone();
                omuzaki_doc_api::apply_to(&mut tree, &plan.patch).map(|()| Arc::new(tree)).map_err(|e| e.to_string())
            }
            (Err(why), _) => Err(why.clone()),
            (_, Err(why)) => Err(why),
        };
        self.result = Some((args, tree));
    }

    /// Whether every unit that needs a choice has one.
    pub fn ready(&self) -> bool {
        self.preview
            .as_ref()
            .is_ok_and(|p| p.units.iter().filter(|u| u.needs_choice).all(|u| self.choices.contains_key(&u.key)))
    }

    /// The arguments Merge sends.
    pub fn args(&self) -> Value {
        let mut resolve = serde_json::Map::new();
        for (key, choice) in &self.choices {
            let value = match choice {
                Choice::Mine => json!("mine"),
                Choice::Theirs => json!("theirs"),
                Choice::Both => json!("both"),
                Choice::PerParameter => {
                    let picks: serde_json::Map<String, Value> = self
                        .params
                        .get(key)
                        .into_iter()
                        .flatten()
                        .map(|(path, theirs)| (path.clone(), json!(if *theirs { "theirs" } else { "mine" })))
                        .collect();
                    json!({ "paths": picks })
                }
            };
            resolve.insert(key.clone(), value);
        }
        let mut args = json!({
            "from": self.from.name,
            "resolve": resolve,
            "exclude": self.left_out.iter().collect::<Vec<_>>(),
        });
        if let Some(at) = self.at {
            args["at"] = json!(at);
        }
        if let Ok(preview) = &self.preview {
            args["expect"] = preview.version.clone();
        }
        args
    }

    fn preview(&mut self, runner: &dyn Runner) {
        let mut args = json!({ "from": self.from.name });
        if let Some(at) = self.at {
            args["at"] = json!(at);
        }
        self.preview = run(runner, args, Mode::DryRun)
            .and_then(|plan| read(&plan.output.unwrap_or(Value::Null)).ok_or_else(String::new));
        self.result = None;
        self.choices.clear();
        self.params.clear();
        self.left_out.clear();
        self.phase = Phase::Choosing;
    }

    /// Merges with the choices made; the phase says how it went.
    pub fn merge(&mut self, runner: &dyn Runner) {
        match run(runner, self.args(), Mode::Apply) {
            Ok(_) => self.phase = Phase::Merged,
            Err(why) if why.starts_with("conflict:") && why.contains("changed while") => self.phase = Phase::Stale(why),
            Err(why) => self.phase = Phase::Failed(why),
        }
    }

    /// Draws the view; false once the person closed it.
    pub fn show(&mut self, ctx: &Context, branching: &Branching, runner: &dyn Runner) -> bool {
        if self.phase == Phase::Choosing {
            self.follow_choices(runner);
        }
        if self.show_base && self.base.is_none() {
            let base = self.preview.as_ref().ok().and_then(|p| p.base);
            self.base = base.map(|point| branching.trees.tree(&point));
        }
        let ds = ds(ctx);
        let mut open = true;
        let mut close = false;
        egui::Window::new(say(ctx, Word::BranchMergeTitle, &[]))
            .id(Id::new("branch_merge"))
            .open(&mut open)
            .collapsible(false)
            .default_size([900.0, 680.0])
            .show(ctx, |ui| {
                let header = say(
                    ctx,
                    Word::BranchMergeHeader,
                    &[("from", self.from.name.as_str()), ("seq", &self.from.seq.to_string()), ("into", &self.into)],
                );
                ui.label(RichText::new(header).strong());
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(560.0);
                        egui::ScrollArea::vertical().id_salt("branch_merge_body").max_height(560.0).show(ui, |ui| {
                            self.body(ui, &ds, branching);
                        });
                    });
                    ui.vertical(|ui| self.result_column(ui, &ds, branching));
                });
                ui.add_space(6.0);
                close = self.footer(ui, &ds, runner);
            });
        let escape = !ctx.text_edit_focused() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        open && !close && !escape
    }

    fn body(&mut self, ui: &mut Ui, ds: &Ds, branching: &Branching) {
        let ctx = ui.ctx().clone();
        let preview = match &self.preview {
            Ok(preview) => preview.clone(),
            Err(why) => {
                // An empty reason is an answer the view can't read.
                let line = if why.is_empty() {
                    say(&ctx, Word::BranchMergeUnreadable, &[])
                } else {
                    say(&ctx, Word::BranchMergeFailed, &[("why", why)])
                };
                ui.label(RichText::new(line).color(ds.theme.danger));
                return;
            }
        };
        ui.label(RichText::new(&preview.summary).color(ds.theme.text2));
        for note in &preview.rights {
            ui.label(RichText::new(note).color(ds.theme.warn));
        }
        let choosing: Vec<&Unit> = preview.units.iter().filter(|u| u.needs_choice).collect();
        let clean: Vec<&Unit> = preview.units.iter().filter(|u| !u.needs_choice && u.offers).collect();
        if choosing.is_empty() && clean.is_empty() {
            let line = say(&ctx, Word::BranchMergeNothing, &[("from", self.from.name.as_str()), ("into", &self.into)]);
            ui.label(RichText::new(line).color(ds.theme.text2));
            return;
        }

        if !choosing.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(say(&ctx, Word::BranchMergeNeedsChoice, &[])).strong());
            for unit in choosing {
                self.card(ui, ds, unit, branching);
            }
        }
        if !clean.is_empty() {
            ui.add_space(8.0);
            Section::new(&say(&ctx, Word::BranchMergeClean, &[]))
                .id_salt("branch_merge_clean")
                .default_open(false)
                .show(ui, |ui| {
                    for unit in clean {
                        let mut taken = !self.left_out.contains(&unit.key);
                        if ui.checkbox(&mut taken, &unit.title).changed() {
                            self.leave_out(&unit.key, !taken);
                        }
                    }
                });
        }
    }

    /// Beside the cards: what Merge would make with the choices as they stand.
    fn result_column(&mut self, ui: &mut Ui, ds: &Ds, branching: &Branching) {
        let Ok(preview) = &self.preview else { return };
        let character = preview.units.iter().filter(|u| u.needs_choice || u.offers).find_map(Unit::character);
        if let (Some(character), Phase::Choosing) = (character, &self.phase) {
            let line = say(ui.ctx(), Word::BranchMergeResult, &[]);
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
            self.picture(ui, ds, branching, &character, Side::Result);
        }
    }

    /// A unit that needs a choice: both sides' pictures, its parameters and the choices.
    fn card(&mut self, ui: &mut Ui, ds: &Ds, unit: &Unit, branching: &Branching) {
        let ctx = ui.ctx().clone();
        egui::Frame::new().fill(ds.theme.bg2).inner_margin(8.0).corner_radius(6.0).show(ui, |ui| {
            ui.label(RichText::new(&unit.title).strong());
            if let Some(note) = &unit.note {
                ui.label(RichText::new(note).color(ds.theme.warn));
            }
            if let Some(character) = unit.character() {
                ui.horizontal(|ui| {
                    let mut sides = vec![(Word::BranchMergeMine, Side::Mine), (Word::BranchMergeTheirs, Side::Theirs)];
                    if self.show_base {
                        sides.insert(0, (Word::BranchMergeBase, Side::Base));
                    }
                    for (word, side) in sides {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(say(&ctx, word, &[])).font(ds.text.small.clone()).color(ds.theme.text2),
                            );
                            self.picture(ui, ds, branching, &character, side);
                        });
                    }
                });
                let word = if self.show_base { Word::BranchMergeHideBase } else { Word::BranchMergeShowBase };
                if ui.add(Button::new(&say(&ctx, word, &[])).ghost()).clicked() {
                    self.show_base = !self.show_base;
                }
            }
            let per_parameter = self.choices.get(&unit.key) == Some(&Choice::PerParameter);
            egui::Grid::new(("branch_merge_rows", &unit.key)).striped(true).show(ui, |ui| {
                ui.label("");
                for word in [Word::BranchMergeBase, Word::BranchMergeMine, Word::BranchMergeTheirs] {
                    ui.label(RichText::new(say(&ctx, word, &[])).font(ds.text.small.clone()).color(ds.theme.text3));
                }
                ui.end_row();
                for change in &unit.changes {
                    let short_path =
                        change.path.strip_prefix(&unit.key).unwrap_or(&change.path).trim_start_matches('/');
                    ui.label(RichText::new(short_path).font(ds.text.mono.clone()));
                    ui.label(super::short(change.base.as_ref()));
                    if per_parameter {
                        let picks = self.params.entry(unit.key.clone()).or_default();
                        let theirs = picks.entry(change.path.clone()).or_insert(false);
                        ui.radio_value(theirs, false, super::short(change.mine.as_ref()));
                        ui.radio_value(theirs, true, super::short(change.theirs.as_ref()));
                    } else {
                        ui.label(super::short(change.mine.as_ref()));
                        ui.label(super::short(change.theirs.as_ref()));
                    }
                    ui.end_row();
                }
            });
            ui.horizontal_wrapped(|ui| {
                let mut choices =
                    vec![(Choice::Mine, Word::BranchMergeKeepMine), (Choice::Theirs, Word::BranchMergeTakeTheirs)];
                if unit.kind == "part" {
                    choices.push((Choice::Both, Word::BranchMergeKeepBoth));
                }
                choices.push((Choice::PerParameter, Word::BranchMergePerParameter));
                for (choice, word) in choices {
                    let chosen = self.choices.get(&unit.key) == Some(&choice);
                    let label = say(&ctx, word, &[]);
                    let button =
                        if chosen { Button::new(&label).primary().icon(icons::CHECK) } else { Button::new(&label) };
                    if ui.add(button).clicked() {
                        self.choose(&unit.key, choice);
                    }
                }
            });
        });
        ui.add_space(6.0);
    }

    /// One side's picture of `character`, at the default camera.
    fn picture(&mut self, ui: &mut Ui, ds: &Ds, branching: &Branching, character: &CharId, side: Side) {
        let ctx = ui.ctx().clone();
        let frame = vec2(PICTURE[0] as f32 * 0.6, PICTURE[1] as f32 * 0.6);
        let (rect, _) = ui.allocate_exact_size(frame, egui::Sense::hover());
        ui.painter().rect_filled(rect, 4.0, ds.theme.bg1);
        let base = self.preview.as_ref().ok().and_then(|p| p.base);
        let (shown, tree) = match side {
            Side::Mine => (Shown::Now, Some(&self.mine)),
            Side::Theirs => (Shown::Point(self.at.unwrap_or(self.from.head)), Some(&self.theirs)),
            Side::Base => (base.map_or(Shown::Now, Shown::Point), self.base.as_ref()),
            Side::Result => (Shown::Now, self.result.as_ref().map(|(_, tree)| tree)),
        };
        let Some(Ok(tree)) = tree else { return };
        let Some(pictures) = &branching.pictures else { return };
        if tree.get("characters").and_then(|c| c.get(character.as_str())).is_none() {
            let line = say(&ctx, Word::BranchCompareMissing, &[("character", character.as_str())]);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, line, ds.text.small.clone(), ds.theme.text2);
            return;
        }
        let camera = Camera::default();
        let key = format!("{}|{}", key_of(&shown, &Ok(Arc::clone(tree))), camera.key(character));
        if let Picture::Ready(image) = pictures.picture(&ctx, &key, tree, &camera.shot(character)) {
            let texture = self
                .textures
                .entry(key.clone())
                .or_insert_with(|| ctx.load_texture(format!("merge-{key}"), (*image).clone(), TextureOptions::LINEAR));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
        }
    }

    /// Merge, or what happened; true when the person closes the view.
    fn footer(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner) -> bool {
        let ctx = ui.ctx().clone();
        let mut close = false;
        match self.phase.clone() {
            Phase::Choosing => {
                let nothing =
                    self.preview.as_ref().map_or(true, |p| p.units.iter().all(|u| !u.offers && !u.needs_choice));
                ui.horizontal(|ui| {
                    let word =
                        if self.left_out.is_empty() { Word::BranchMergeApply } else { Word::BranchMergeApplySelected };
                    let label = say(&ctx, word, &[]);
                    let wait = say(&ctx, Word::BranchMergeChooseFirst, &[]);
                    let button = Button::new(&label)
                        .primary()
                        .icon(icons::GIT_MERGE)
                        .disabled_if(!self.ready() || nothing, &wait);
                    if ui.add(button).clicked() {
                        self.merge(runner);
                    }
                    close = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
                });
                if !self.ready() && !nothing {
                    ui.label(
                        RichText::new(say(&ctx, Word::BranchMergeChooseFirst, &[]))
                            .font(ds.text.small.clone())
                            .color(ds.theme.text2),
                    );
                }
            }
            Phase::Merged => {
                let line = say(&ctx, Word::BranchMergeDone, &[("from", self.from.name.as_str()), ("into", &self.into)]);
                ui.label(RichText::new(line).color(ds.theme.ok));
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new(&say(&ctx, Word::BranchMergeUndo, &[])).icon(icons::ARROW_COUNTER_CLOCKWISE))
                        .clicked()
                    {
                        match runner.undo(&person(), &Grants::person()) {
                            Ok(_) => self.phase = Phase::Undone,
                            Err(e) => self.phase = Phase::Failed(e.message),
                        }
                    }
                    close = ui.add(Button::new(&say(&ctx, Word::BranchClose, &[])).primary()).clicked();
                });
            }
            Phase::Undone => {
                ui.label(RichText::new(say(&ctx, Word::BranchMergeUndone, &[])).color(ds.theme.text2));
                close = ui.add(Button::new(&say(&ctx, Word::BranchClose, &[]))).clicked();
            }
            Phase::Stale(why) | Phase::Failed(why) => {
                ui.label(RichText::new(say(&ctx, Word::BranchMergeFailed, &[("why", &why)])).color(ds.theme.danger));
                ui.horizontal(|ui| {
                    if ui.add(Button::new(&say(&ctx, Word::BranchMergeAgain, &[])).primary()).clicked() {
                        self.mine = Shown::Now.tree(runner, &NoTrees);
                        self.preview(runner);
                    }
                    close = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
                });
            }
        }
        close
    }
}

/// Which state a picture in the view shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// The folder branch now.
    Mine,
    /// The branch merged in, at the point merged.
    Theirs,
    /// Their common base.
    Base,
    /// What Merge would make.
    Result,
}

/// Reads no points: for reading now again.
struct NoTrees;

impl super::PointTrees for NoTrees {
    fn tree(&self, point: &PointId) -> Result<Arc<Value>, String> {
        Err(format!("point {point} isn't read here"))
    }
}

/// Runs `branch.merge` as the person; its plan, or the refusal as "code: message (hint)".
fn run(runner: &dyn Runner, args: Value, mode: Mode) -> Result<omuzaki_ops_api::Plan, String> {
    let request = Request {
        op: OP.to_owned(),
        args,
        mode,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    match runner.run(request) {
        Ok(outcome) => Ok(outcome.plan),
        Err(error) => {
            let code = match error.code {
                ErrorCode::Conflict => "conflict",
                _ => "refused",
            };
            Err(match &error.hint {
                Some(hint) => format!("{code}: {}. {hint}", error.message),
                None => format!("{code}: {}", error.message),
            })
        }
    }
}

/// The dry run's output as a [`Preview`]; `None` when it isn't what `branch.merge` answers.
pub fn read(output: &Value) -> Option<Preview> {
    let units = output.get("units").and_then(Value::as_array)?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let units = units
        .iter()
        .map(|unit| {
            let changes: Vec<Change> = unit
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|c| Change {
                    path: text(c, "path"),
                    base: c.get("base").filter(|v| !v.is_null()).cloned(),
                    mine: c.get("mine").filter(|v| !v.is_null()).cloned(),
                    theirs: c.get("theirs").filter(|v| !v.is_null()).cloned(),
                })
                .collect();
            let status = text(unit, "status");
            let taken = text(unit, "taken");
            Unit {
                key: text(unit, "key"),
                title: text(unit, "title"),
                kind: text(unit, "kind"),
                needs_choice: status == "invalid" || taken == "none",
                offers: changes.iter().any(|c| c.theirs != c.base),
                changes,
                note: unit.get("note").and_then(Value::as_str).map(str::to_owned),
            }
        })
        .collect();
    let rights = output
        .get("rights")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| r.as_str().map_or_else(|| r.to_string(), str::to_owned))
        .collect();
    Some(Preview {
        units,
        version: output.get("version").cloned().unwrap_or(Value::Null),
        summary: text(output, "summary"),
        rights,
        base: output.get("base").and_then(Value::as_str).and_then(|b| b.parse().ok()),
    })
}
