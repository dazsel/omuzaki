//! Comparing branches side by side (`docs/design/branches.md` 6.3 and 6.4; #442).
//!
//! 2 to 6 columns, each a branch: the folder branch as it is now, any other at its newest point. Each shows the same
//! character at one camera (dragging any picture turns them all, and the pictures are taken again when the drag ends),
//! and under it what differs from the reference column, grouped by part, each part a click away from being selected.
//! A branch whose lane is open may be ahead of its newest point: its column says which version it shows, and Refresh
//! makes a point of it now. Each column switches to its branch, merges it into the folder branch, or leaves.

use std::sync::Arc;

use egui::{Context, Id, Key, Modifiers, RichText, Sense, TextureHandle, TextureOptions, Ui, vec2};
use omuzaki_char_api::CharId;
use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::{BranchId, BranchInfo, LaneState};
use omuzaki_ops_api::Runner;

use super::pictures::Picture;
use super::{Asked, Branching, Camera, Listing, PICTURE, Shown, person, subject};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, Section};
use crate::panels::edit::EditFocus;
use crate::shell::compare::{Group, changes, grouped};
use crate::shell::text::{Word, say};

/// At most this many columns.
pub const MOST: usize = 6;
/// How far a pixel of drag turns the camera, in degrees.
const TURN: f32 = 0.5;

/// One branch in the compare.
struct Column {
    branch: BranchInfo,
    shown: Shown,
    tree: Result<Arc<Value>, String>,
    /// Names the state for its pictures: the point, or now by the tree's hash.
    key: String,
    texture: Option<(String, TextureHandle)>,
}

impl std::fmt::Debug for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Column").field("branch", &self.branch.name).field("shown", &self.shown).finish_non_exhaustive()
    }
}

/// The compare window's state.
#[derive(Debug)]
pub struct BranchCompare {
    columns: Vec<Column>,
    reference: usize,
    camera: Camera,
    /// Where a drag in progress has turned the camera to; the pictures are taken again when it ends.
    turning: Option<Camera>,
}

impl BranchCompare {
    /// Compares `ids` (2 to 6 of them; the first is the reference), reading each one's state.
    pub fn open(ids: &[BranchId], listing: &Listing, branching: &Branching, runner: &dyn Runner) -> Self {
        let mut compare = Self { columns: Vec::new(), reference: 0, camera: Camera::default(), turning: None };
        for id in ids.iter().take(MOST) {
            if let Some(branch) = listing.branches.iter().find(|b| &b.id == id) {
                compare.add(branch.clone(), listing, branching, runner);
            }
        }
        compare
    }

    /// The branches shown, in column order.
    pub fn branches(&self) -> Vec<&BranchId> {
        self.columns.iter().map(|c| &c.branch.id).collect()
    }

    /// The reference column's index.
    pub const fn reference(&self) -> usize {
        self.reference
    }

    /// The camera every picture is taken at.
    pub const fn camera(&self) -> Camera {
        self.camera
    }

    /// What column `n` differs by from the reference, grouped by part; `None` for the reference itself.
    pub fn groups(&self, n: usize) -> Option<Result<Vec<Group>, String>> {
        if n == self.reference {
            return None;
        }
        let (reference, column) = (self.columns.get(self.reference)?, self.columns.get(n)?);
        Some(match (&reference.tree, &column.tree) {
            (Ok(a), Ok(b)) => {
                Ok(grouped(changes(a, b).into_iter().filter(|c| !c.path.starts_with("merges")).collect()))
            }
            (Err(why), _) | (_, Err(why)) => Err(why.clone()),
        })
    }

    fn add(&mut self, branch: BranchInfo, listing: &Listing, branching: &Branching, runner: &dyn Runner) {
        let shown = if listing.folder.as_ref() == Some(&branch.id) { Shown::Now } else { Shown::Point(branch.head) };
        let tree = shown.tree(runner, branching.trees.as_ref());
        let key = key_of(&shown, &tree);
        self.columns.push(Column { branch, shown, tree, key, texture: None });
    }

    /// Draws the window; what it asks the shell to do, and false once the person closed it.
    pub fn show(
        &mut self,
        ctx: &Context,
        branching: &Branching,
        listing: &Listing,
        runner: &dyn Runner,
        focus: Option<&EditFocus>,
    ) -> (Option<Asked>, bool) {
        let ds = ds(ctx);
        let mut open = true;
        let mut close = false;
        let mut asked = None;
        egui::Window::new(say(ctx, Word::BranchCompareTitle, &[]))
            .id(Id::new("branch_compare"))
            .open(&mut open)
            .collapsible(false)
            .default_size([880.0, 640.0])
            .show(ctx, |ui| {
                let reference = self.columns.get(self.reference).map(|c| c.branch.name.to_string()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(say(ui.ctx(), Word::BranchCompareAgainst, &[("branch", &reference)])).strong(),
                    );
                    ui.label(RichText::new(say(ui.ctx(), Word::BranchCompareOrbit, &[])).color(ds.theme.text2));
                });
                self.add_menu(ui, branching, listing, runner);
                ui.add_space(6.0);
                egui::ScrollArea::both().id_salt("branch_compare_columns").show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for n in 0..self.columns.len() {
                            ui.vertical(|ui| {
                                ui.set_width(PICTURE[0] as f32);
                                if let Some(a) = self.column(ui, &ds, n, branching, listing, runner, focus) {
                                    asked = Some(a);
                                }
                            });
                            ui.separator();
                        }
                    });
                });
                ui.add_space(6.0);
                close = ui.add(Button::new(&say(ui.ctx(), Word::BranchClose, &[])).ghost()).clicked();
            });
        let escape = !ctx.text_edit_focused() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        (asked, open && !close && !escape)
    }

    /// "Add a branch": the live branches not shown yet.
    fn add_menu(&mut self, ui: &mut Ui, branching: &Branching, listing: &Listing, runner: &dyn Runner) {
        if self.columns.len() >= MOST {
            return;
        }
        let shown: Vec<BranchId> = self.columns.iter().map(|c| c.branch.id.clone()).collect();
        let more: Vec<&BranchInfo> =
            listing.branches.iter().filter(|b| b.deleted.is_none() && !shown.contains(&b.id)).collect();
        if more.is_empty() {
            return;
        }
        let mut picked = None;
        ui.menu_button(format!("{} {}", icons::PLUS, say(ui.ctx(), Word::BranchCompareAdd, &[])), |ui| {
            for branch in more {
                if ui.button(branch.name.as_str()).clicked() {
                    picked = Some(branch.clone());
                    ui.close();
                }
            }
        });
        if let Some(branch) = picked {
            self.add(branch, listing, branching, runner);
        }
    }

    #[allow(clippy::too_many_arguments, reason = "one column of one window; the arguments are the window's")]
    fn column(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        n: usize,
        branching: &Branching,
        listing: &Listing,
        runner: &dyn Runner,
        focus: Option<&EditFocus>,
    ) -> Option<Asked> {
        let ctx = ui.ctx().clone();
        let mut asked = None;
        let folder = listing.folder.clone();
        let (name, seq, open_lane, is_folder) = {
            let column = self.columns.get(n)?;
            let open_lane = matches!(column.branch.lane, LaneState::Open { .. });
            (column.branch.name.to_string(), column.branch.seq, open_lane, folder.as_ref() == Some(&column.branch.id))
        };
        ui.label(RichText::new(&name).strong());
        if is_folder {
            ui.label(RichText::new(say(&ctx, Word::BranchHere, &[])).font(ds.text.small.clone()).color(ds.theme.text2));
        } else {
            ui.horizontal(|ui| {
                let as_of = say(&ctx, Word::BranchCompareAsOf, &[("seq", &seq.to_string())]);
                ui.label(RichText::new(as_of).font(ds.text.small.clone()).color(ds.theme.text2));
                if open_lane && ui.add(Button::new(&say(&ctx, Word::BranchCompareRefresh, &[])).ghost()).clicked() {
                    self.refresh(n, branching, runner);
                }
            });
        }
        self.picture(ui, ds, n, branching, focus);
        ui.horizontal_wrapped(|ui| {
            if n != self.reference
                && ui.add(Button::new(&say(&ctx, Word::BranchCompareReference, &[])).ghost()).clicked()
            {
                self.reference = n;
            }
            let id = self.columns.get(n).map(|c| c.branch.id.clone());
            if let (false, Some(id)) = (is_folder, id) {
                if ui.add(Button::new(&say(&ctx, Word::BranchSwitch, &[])).ghost().icon(icons::GIT_BRANCH)).clicked() {
                    match branching.branches.switch(&id, &person()) {
                        Ok(_) => asked = Some(Asked::Switched),
                        Err(e) => tracing::warn!(error = %e, "the switch from the compare was refused"),
                    }
                }
                let into = listing.folder_name();
                let merge = say(&ctx, Word::BranchMergeInto, &[("branch", &into)]);
                if ui.add(Button::new(&merge).ghost().icon(icons::GIT_MERGE)).clicked() {
                    asked = Some(Asked::Merge(id, None));
                }
            }
            if self.columns.len() > 2
                && ui.add(Button::new(&say(&ctx, Word::BranchCompareRemove, &[])).ghost().icon(icons::X)).clicked()
            {
                self.columns.remove(n);
                if self.reference >= self.columns.len() || self.reference == n {
                    self.reference = 0;
                } else if self.reference > n {
                    self.reference -= 1;
                }
            }
        });
        if n < self.columns.len() {
            self.diff(ui, ds, n, focus);
        }
        asked
    }

    /// The column's picture, at the shared camera; dragging it turns every column.
    fn picture(&mut self, ui: &mut Ui, ds: &Ds, n: usize, branching: &Branching, focus: Option<&EditFocus>) {
        let ctx = ui.ctx().clone();
        let size = vec2(PICTURE[0] as f32, PICTURE[1] as f32);
        let (rect, response) = ui.allocate_exact_size(size, Sense::drag());
        let name = self.columns.get(n).map(|c| c.branch.name.to_string()).unwrap_or_default();
        let label = say(&ctx, Word::BranchComparePicture, &[("branch", &name)]);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &label));
        ui.painter().rect_filled(rect, 4.0, ds.theme.bg2);
        if response.dragged() {
            let delta = response.drag_delta();
            let turning = self.turning.get_or_insert(self.camera);
            turning.yaw = (turning.yaw + delta.x * TURN + 180.0).rem_euclid(360.0) - 180.0;
            turning.pitch = (turning.pitch - delta.y * TURN).clamp(-60.0, 60.0);
        }
        if response.drag_stopped()
            && let Some(turned) = self.turning.take()
        {
            self.camera = turned;
        }
        let open = focus.and_then(EditFocus::character);
        let camera = self.camera;
        let Some(column) = self.columns.get_mut(n) else { return };
        let tree = match &column.tree {
            Ok(tree) => Arc::clone(tree),
            Err(why) => {
                centred(ui, rect, &say(&ctx, Word::BranchCompareUnread, &[("why", why)]), ds.theme.danger);
                return;
            }
        };
        let Some(character) = subject(&tree, open.as_ref()).or(open) else {
            centred(ui, rect, &say(&ctx, Word::BranchCompareMissing, &[("character", "")]), ds.theme.text2);
            return;
        };
        if !has(&tree, &character) {
            let line = say(&ctx, Word::BranchCompareMissing, &[("character", character.as_str())]);
            centred(ui, rect, &line, ds.theme.text2);
            return;
        }
        let Some(pictures) = &branching.pictures else { return };
        let key = format!("{}|{}", column.key, camera.key(&character));
        match pictures.picture(&ctx, &key, &tree, &camera.shot(&character)) {
            Picture::Ready(image) => {
                let fresh = column.texture.as_ref().is_none_or(|(k, _)| *k != key);
                if fresh {
                    let texture = ctx.load_texture(format!("branch-{key}"), (*image).clone(), TextureOptions::LINEAR);
                    column.texture = Some((key, texture));
                }
            }
            Picture::Waiting if column.texture.is_none() => {
                centred(ui, rect, &say(&ctx, Word::BranchCompareWaiting, &[]), ds.theme.text2);
            }
            Picture::Failed(why) => {
                centred(ui, rect, &say(&ctx, Word::BranchCompareNoPicture, &[("why", &why)]), ds.theme.text2);
                return;
            }
            Picture::Waiting => {}
        }
        if let Some((_, texture)) = &column.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), rect, uv, egui::Color32::WHITE);
        }
    }

    /// What this column differs by from the reference.
    fn diff(&self, ui: &mut Ui, ds: &Ds, n: usize, focus: Option<&EditFocus>) {
        let ctx = ui.ctx().clone();
        let Some(groups) = self.groups(n) else { return };
        let reference = self.columns.get(self.reference).map(|c| c.branch.name.to_string()).unwrap_or_default();
        let groups = match groups {
            Ok(groups) => groups,
            Err(why) => {
                ui.label(RichText::new(say(&ctx, Word::BranchCompareUnread, &[("why", &why)])).color(ds.theme.danger));
                return;
            }
        };
        if groups.is_empty() {
            ui.label(
                RichText::new(say(&ctx, Word::BranchCompareSame, &[("branch", &reference)])).color(ds.theme.text2),
            );
            return;
        }
        for group in &groups {
            let badge = say(&ctx, Word::BranchCompareChanges, &[("count", &group.changes.len().to_string())]);
            Section::new(group.title())
                .badge(&badge)
                .default_open(true)
                .id_salt(("branch_compare", n, &group.path))
                .show(ui, |ui| {
                    if let (Some((character, part)), Some(focus)) = (&group.part, focus) {
                        let select = say(&ctx, Word::BranchCompareSelect, &[("part", part.as_str())]);
                        if ui.add(Button::new(&select).ghost().icon(icons::CURSOR)).clicked() {
                            if focus.character().as_ref() != Some(character) {
                                focus.open(Some(character.clone()));
                            }
                            focus.select(part.clone());
                        }
                    }
                    for change in &group.changes {
                        let line = format!(
                            "{}: {} \u{2192} {}",
                            change.path,
                            super::short(change.before.as_ref()),
                            super::short(change.after.as_ref())
                        );
                        ui.add(
                            egui::Label::new(RichText::new(line).font(ds.text.mono.clone()).color(ds.theme.text2))
                                .wrap(),
                        );
                    }
                });
        }
    }

    /// Makes a point of column `n`'s branch now and shows it.
    fn refresh(&mut self, n: usize, branching: &Branching, runner: &dyn Runner) {
        let Some(column) = self.columns.get_mut(n) else { return };
        match branching.branches.checkpoint(&column.branch.id, "Compare", None, &person()) {
            Ok(point) => {
                column.shown = Shown::Point(point);
                column.tree = column.shown.tree(runner, branching.trees.as_ref());
                column.key = key_of(&column.shown, &column.tree);
                column.texture = None;
                if let Some(fresh) = branching.branches.list().into_iter().find(|b| b.id == column.branch.id) {
                    column.branch = fresh;
                }
            }
            Err(error) => column.tree = Err(error.to_string()),
        }
    }
}

fn has(tree: &Value, character: &CharId) -> bool {
    tree.get("characters").and_then(|c| c.get(character.as_str())).is_some()
}

/// The key a state's pictures are kept under: the point's id, or now by what its tree holds.
pub(super) fn key_of(shown: &Shown, tree: &Result<Arc<Value>, String>) -> String {
    match (shown, tree) {
        (Shown::Point(id), _) => id.to_string(),
        (Shown::Now, Ok(tree)) => {
            let bytes = serde_json::to_vec(tree.as_ref()).unwrap_or_default();
            format!("now-{}", omuzaki_store_api::ContentHash::of(&bytes))
        }
        (Shown::Now, Err(_)) => "now".to_owned(),
    }
}

fn centred(ui: &Ui, rect: egui::Rect, text: &str, colour: egui::Color32) {
    let ds = ds(ui.ctx());
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, ds.text.small.clone(), colour);
}
