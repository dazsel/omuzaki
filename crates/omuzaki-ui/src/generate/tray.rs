//! The Generated tray (#1010, CN-03, `connectors.md` section 7): what the generators made, waiting for a person to
//! accept it before anything uses it.
//!
//! Each candidate is a tile: an image shows its pixels from the store, audio, a mesh or motion show their kind. Under
//! it, the connector, the model, the plan and the cost, and the **rights badge in words and an icon**, never colour
//! alone ("Can be published", "Can't be published: unknown licence"). Pending tiles offer Accept, Reject (with an
//! optional reason) and Make another (a new seed); rejected ones stay, dimmed, under their filter. Choosing tiles
//! offers Accept and Reject for all of them, and Compare puts two side by side before deciding.
//!
//! The form at the top starts a run with a ready image generator, through the cost card ([`Confirm`]).
//!
//! Every action is an op run as the person (`connect.accept`, `connect.reject`, `connect.regenerate`,
//! `connect.generate`); the tray reads `connect.tray_list` and `connect.list` once a second and after each action.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Align2, Rect, RichText, Ui, vec2};
use omuzaki_connect::ops::list::Listed;
use omuzaki_connect::ops::tray::{TrayRow, TrayState};
use omuzaki_connect_api::{CandidateId, Capability, ConnectorId, MediaKind};
use omuzaki_ops_api::{Actor, OpError, Runner};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::json;

use super::confirm::{CardOutput, Confirm};
use super::{Generation, ready_for, rights, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::space;
use crate::ds::widgets::{Button, Choice, Dropdown, EmptyState, Segment, Segmented, TextField, Thumb, ThumbnailTile};
use crate::panels::text::say;
use crate::settings::connectors::run;
use crate::shell::requests::{ShellRequest, ShellRequests};
use crate::theatre::pictures::{Picture, Pictures, Source, Sources};

/// Seconds between reads.
const REFRESH_S: f64 = 1.0;
/// A tile's width.
const TILE: f32 = 196.0;
/// The size of a run from the form.
const SIDE: u32 = 512;

/// Which candidates show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    /// Waiting for a decision.
    #[default]
    Pending,
    /// Accepted.
    Accepted,
    /// Rejected.
    Rejected,
    /// All of them.
    All,
}

impl Filter {
    fn admits(self, state: TrayState) -> bool {
        match self {
            Self::Pending => state == TrayState::Pending,
            Self::Accepted => state == TrayState::Accepted,
            Self::Rejected => state == TrayState::Rejected,
            Self::All => true,
        }
    }
}

/// What the person asked for this frame.
enum Action {
    Accept(Vec<CandidateId>),
    Reject(Vec<CandidateId>, Option<String>),
    Regenerate(CandidateId),
    Make,
}

/// The tray.
#[derive(Default)]
pub struct Tray {
    rows: Option<Result<Vec<TrayRow>, String>>,
    listed: Vec<Listed>,
    read_at: Option<f64>,
    filter: Filter,
    chosen: BTreeSet<CandidateId>,
    comparing: bool,
    /// The candidate whose reason field is open, and the reason.
    rejecting: Option<(CandidateId, String)>,
    pictures: Pictures,
    refused: Option<String>,
    /// The form: the generator, the prompt and how many.
    with: Option<ConnectorId>,
    prompt: String,
    count: u8,
    confirm: Confirm,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").field("filter", &self.filter).field("chosen", &self.chosen).finish_non_exhaustive()
    }
}

impl Tray {
    /// The candidates as last read.
    pub fn rows(&self) -> &[TrayRow] {
        self.rows.as_ref().and_then(|r| r.as_ref().ok()).map_or(&[], Vec::as_slice)
    }

    /// The run card.
    pub fn confirm(&self) -> &Confirm {
        &self.confirm
    }

    /// Draws the tray; `requests` takes "open the Generators page".
    pub fn ui(&mut self, ui: &mut Ui, generation: &Generation, actor: &Actor, requests: Option<&ShellRequests>) {
        let ds = ds(ui.ctx());
        let now = ui.input(|i| i.time);
        let runner = &generation.runner;
        if self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(runner.as_ref(), actor, now);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_S));
        let mut actions = Vec::new();
        self.form(ui, &ds, &mut actions);
        let status =
            self.with.as_ref().and_then(|id| self.listed.iter().find(|l| l.id == *id)).map(|l| l.status.clone());
        match self.confirm.show(ui, runner, actor, status.as_ref()) {
            CardOutput::OpenSettings => open_settings(requests),
            CardOutput::Finished => self.read_at = None,
            CardOutput::Nothing => {}
        }
        ui.add_space(space::S8);
        let label = say(&ds, text::SHOW, &[]);
        let words = [text::PENDING, text::ACCEPTED, text::REJECTED, text::ALL_STATES].map(|w| say(&ds, w, &[]));
        let segments = [
            Segment::new(Filter::Pending, &words[0]),
            Segment::new(Filter::Accepted, &words[1]),
            Segment::new(Filter::Rejected, &words[2]),
            Segment::new(Filter::All, &words[3]),
        ];
        ui.add(Segmented::new(&label, &mut self.filter, &segments).id_salt("tray.filter"));
        if let Some(reason) = &self.refused {
            ui.label(RichText::new(say(&ds, text::REFUSED, &[("reason", reason)])).color(ds.theme.danger));
        }
        ui.add_space(space::S6);
        let rows: Vec<TrayRow> = match &self.rows {
            None => Vec::new(),
            Some(Err(reason)) => {
                let headline = say(&ds, text::NO_TRAY, &[]);
                EmptyState::new(icons::SPARKLE, &headline, reason).show(ui);
                return;
            }
            Some(Ok(rows)) => rows.iter().filter(|r| self.filter.admits(r.state)).cloned().collect(),
        };
        self.chosen.retain(|id| rows.iter().any(|r| r.id == *id));
        self.batch(ui, &ds, &mut actions);
        let store = generation.store.as_ref();
        egui::ScrollArea::vertical().id_salt("tray.scroll").auto_shrink([false, false]).show(ui, |ui| {
            if rows.is_empty() {
                let headline = say(&ds, text::TRAY_EMPTY, &[]);
                let line = say(&ds, text::TRAY_EMPTY_HOW, &[]);
                EmptyState::new(icons::SPARKLE, &headline, &line).show(ui);
                return;
            }
            if self.comparing && self.chosen.len() == 2 {
                self.compare(ui, &ds, &rows, store, &mut actions);
                return;
            }
            let grid = egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true);
            ui.with_layout(grid, |ui| {
                ui.spacing_mut().item_spacing = vec2(space::S12, space::S12);
                for row in &rows {
                    self.tile(ui, &ds, row, store, &mut actions);
                }
            });
        });
        for action in actions {
            self.act(ui, runner, actor, action, now);
        }
    }

    /// The form that starts a run.
    fn form(&mut self, ui: &mut Ui, ds: &Ds, actions: &mut Vec<Action>) {
        let ready = ready_for(&self.listed, Capability::Image);
        if ready.is_empty() {
            ui.label(RichText::new(say(ds, text::MAKE_NONE, &[])).color(ds.theme.text2));
            return;
        }
        if self.with.as_ref().is_none_or(|id| !ready.iter().any(|l| l.id == *id)) {
            self.with = ready.first().map(|l| l.id.clone());
        }
        if self.count == 0 {
            self.count = 1;
        }
        ui.horizontal_wrapped(|ui| {
            let mut at = ready.iter().position(|l| Some(&l.id) == self.with.as_ref()).unwrap_or(0);
            let choices: Vec<Choice<'_, usize>> =
                ready.iter().enumerate().map(|(n, l)| Choice::new(n, l.name.as_str())).collect();
            let label = say(ds, text::MAKE_WITH, &[]);
            if ui.add(Dropdown::new(&label, &mut at, &choices).id_salt("tray.with")).changed() {
                self.with = ready.get(at).map(|l| l.id.clone());
            }
            let prompt = say(ds, text::PROMPT, &[]);
            ui.add(TextField::new(&prompt, &mut self.prompt).hint(&prompt).width(260.0).id_salt("tray.prompt"));
            let counts: Vec<String> = (1..=4u8).map(|n| n.to_string()).collect();
            let count_choices: Vec<Choice<'_, u8>> =
                (1..=4u8).zip(&counts).map(|(n, word)| Choice::new(n, word)).collect();
            let count = say(ds, text::COUNT, &[]);
            ui.add(Dropdown::new(&count, &mut self.count, &count_choices).id_salt("tray.count"));
            let make = say(ds, text::MAKE, &[]);
            let busy = self.confirm.running() || self.confirm.asking();
            let why = say(ds, text::PROMPT, &[]);
            let button = Button::new(&make).primary().icon(icons::SPARKLE).op("connect.generate");
            let button = button.disabled_if(self.prompt.trim().is_empty() || busy, &why);
            if ui.add(button).clicked() {
                actions.push(Action::Make);
            }
        });
    }

    /// Accept and Reject for the chosen tiles, and Compare for two.
    fn batch(&mut self, ui: &mut Ui, ds: &Ds, actions: &mut Vec<Action>) {
        if self.chosen.is_empty() {
            return;
        }
        let count = self.chosen.len().to_string();
        ui.horizontal(|ui| {
            let accept = say(ds, text::ACCEPT_CHOSEN, &[("count", &count)]);
            if ui.add(Button::new(&accept).primary().op("connect.accept")).clicked() {
                actions.push(Action::Accept(self.chosen.iter().cloned().collect()));
            }
            let reject = say(ds, text::REJECT_CHOSEN, &[("count", &count)]);
            if ui.add(Button::new(&reject).op("connect.reject")).clicked() {
                actions.push(Action::Reject(self.chosen.iter().cloned().collect(), None));
            }
            let (compare, close) = (say(ds, text::COMPARE, &[]), say(ds, text::COMPARE_CLOSE, &[]));
            let how = say(ds, text::COMPARE_HOW, &[]);
            if self.comparing {
                if ui.add(Button::new(&close).ghost()).clicked() {
                    self.comparing = false;
                }
            } else if ui
                .add(Button::new(&compare).icon(icons::COLUMNS).disabled_if(self.chosen.len() != 2, &how))
                .clicked()
            {
                self.comparing = true;
            }
        });
        ui.add_space(space::S6);
    }

    /// One candidate's tile.
    fn tile(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        row: &TrayRow,
        store: Option<&Arc<dyn omuzaki_store_api::Store>>,
        actions: &mut Vec<Action>,
    ) {
        let name = tile_name(ds, row);
        let picture = self.picture(ui, store, row);
        let layout = egui::Layout::top_down(egui::Align::Min);
        ui.allocate_ui_with_layout(vec2(TILE, 0.0), layout, |ui| {
            ui.set_width(TILE);
            if row.state == TrayState::Rejected {
                ui.multiply_opacity(0.55);
            }
            let thumb = match (&picture, row.kind) {
                (Some(picture), _) => Thumb::Ready(picture.texture.id()),
                (None, MediaKind::Image) => Thumb::Loading,
                (None, _) => Thumb::Missing,
            };
            let pending = row.state == TrayState::Pending;
            let chosen = self.chosen.contains(&row.id);
            // A pending tile is chosen by clicking it: a named button with a selected state, for batch decisions
            // and Compare.
            let clicked = if row.kind == MediaKind::Image {
                let tile = ThumbnailTile::new(&name, thumb).width(TILE).selected(chosen);
                tile.id_salt(("tray", row.id.as_str())).show(ui).response.clicked()
            } else {
                kind_tile(ui, ds, row.kind, &name, chosen)
            };
            if clicked && pending {
                if chosen {
                    self.chosen.remove(&row.id);
                } else {
                    self.chosen.insert(row.id.clone());
                }
            }
            details(ui, ds, row);
            if pending {
                self.decide(ui, ds, row, actions);
            }
        });
    }

    /// Accept, Reject with its reason field, and Make another.
    fn decide(&mut self, ui: &mut Ui, ds: &Ds, row: &TrayRow, actions: &mut Vec<Action>) {
        if let Some((id, mut reason)) = self.rejecting.take() {
            if id == row.id {
                let why = say(ds, text::REJECT_WHY, &[]);
                ui.add(TextField::new(&why, &mut reason).hint(&why).id_salt(("tray.reason", row.id.as_str())));
                let mut keep = true;
                ui.horizontal(|ui| {
                    let confirm = say(ds, text::REJECT_CONFIRM, &[]);
                    if ui.add(Button::new(&confirm).danger().op("connect.reject")).clicked() {
                        let reason = reason.trim().to_owned();
                        actions.push(Action::Reject(vec![row.id.clone()], (!reason.is_empty()).then_some(reason)));
                    }
                    let cancel = say(ds, text::CANCEL, &[]);
                    if ui.add(Button::new(&cancel).ghost()).clicked() {
                        keep = false;
                    }
                });
                if keep {
                    self.rejecting = Some((id, reason));
                }
                return;
            }
            self.rejecting = Some((id, reason));
        }
        ui.horizontal_wrapped(|ui| {
            let accept = say(ds, text::ACCEPT, &[]);
            if ui.add(Button::new(&accept).primary().op("connect.accept")).clicked() {
                actions.push(Action::Accept(vec![row.id.clone()]));
            }
            let reject = say(ds, text::REJECT, &[]);
            if ui.add(Button::new(&reject).op("connect.reject")).clicked() {
                self.rejecting = Some((row.id.clone(), String::new()));
            }
            let again = say(ds, text::REGENERATE, &[]);
            if ui.add(Button::new(&again).ghost().op("connect.regenerate")).clicked() {
                actions.push(Action::Regenerate(row.id.clone()));
            }
        });
    }

    /// The two chosen candidates side by side, each with its details and Accept.
    fn compare(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        rows: &[TrayRow],
        store: Option<&Arc<dyn omuzaki_store_api::Store>>,
        actions: &mut Vec<Action>,
    ) {
        let pair: Vec<TrayRow> = rows.iter().filter(|r| self.chosen.contains(&r.id)).cloned().collect();
        let side = ((ui.available_width() - space::S12) / 2.0).clamp(160.0, 640.0);
        ui.horizontal_top(|ui| {
            for row in &pair {
                let picture = self.picture(ui, store, row);
                ui.vertical(|ui| {
                    ui.set_width(side);
                    let name = tile_name(ds, row);
                    let (rect, response) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 6.0, ds.theme.bg2);
                    if let Some(picture) = &picture {
                        #[allow(clippy::cast_precision_loss, reason = "a picture's side, at most 1024")]
                        let [w, h] = picture.size.map(|v| v as f32);
                        let scale = (side / w.max(1.0)).min(side / h.max(1.0));
                        let shown = Rect::from_center_size(rect.center(), vec2(w * scale, h * scale));
                        egui::Image::new((picture.texture.id(), shown.size())).paint_at(ui, shown);
                    } else {
                        let kind = kind_word(ds, row.kind);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            kind,
                            ds.text.body.clone(),
                            ds.theme.text2,
                        );
                    }
                    response.on_hover_text(&name);
                    details(ui, ds, row);
                    let accept = say(ds, text::ACCEPT, &[]);
                    if ui.add(Button::new(&accept).primary().op("connect.accept")).clicked() {
                        actions.push(Action::Accept(vec![row.id.clone()]));
                    }
                });
            }
        });
    }

    /// `row`'s picture, once loaded, when it is an image and the app has a store.
    fn picture(
        &mut self,
        ui: &Ui,
        store: Option<&Arc<dyn omuzaki_store_api::Store>>,
        row: &TrayRow,
    ) -> Option<Picture> {
        if row.kind != MediaKind::Image {
            return None;
        }
        let sources = Sources { steps: None, renders: store };
        self.pictures.get(ui.ctx(), sources, &Source::Render(row.asset))
    }

    fn act(&mut self, ui: &Ui, runner: &Arc<dyn Runner>, actor: &Actor, action: Action, now: f64) {
        let result: Result<(), OpError> = match action {
            Action::Accept(ids) => ids
                .iter()
                .try_for_each(|id| run(runner.as_ref(), actor, "connect.accept", json!({ "candidate": id })).map(drop)),
            Action::Reject(ids, reason) => {
                self.rejecting = None;
                ids.iter().try_for_each(|id| {
                    run(runner.as_ref(), actor, "connect.reject", json!({ "candidate": id, "reason": reason }))
                        .map(drop)
                })
            }
            Action::Regenerate(id) => {
                regenerate(runner, actor, &id);
                Ok(())
            }
            Action::Make => {
                if let Some(with) = self.with.clone() {
                    let request = json!({
                        "capability": "image",
                        "prompt": self.prompt.trim(),
                        "width": SIDE,
                        "height": SIDE,
                        "count": self.count.max(1),
                    });
                    let status = self.listed.iter().find(|l| l.id == with).map(|l| l.status.clone());
                    self.confirm.start(ui, runner, actor, with, request, status.as_ref());
                }
                Ok(())
            }
        };
        self.refused = result.err().map(|e| e.message);
        self.chosen.clear();
        self.comparing = false;
        self.read(runner.as_ref(), actor, now);
    }

    /// Reads the tray and the generators.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        let rows = run(runner, actor, "connect.tray_list", json!({})).and_then(|v| {
            serde_json::from_value::<Vec<TrayRow>>(v).map_err(|e| OpError::internal(format!("connect.tray_list: {e}")))
        });
        // Newest first.
        self.rows = Some(
            rows.map(|mut rows| {
                rows.sort_by(|a, b| {
                    b.created_at.0.total_cmp(&a.created_at.0).then_with(|| b.id.as_str().cmp(a.id.as_str()))
                });
                rows
            })
            .map_err(|e| e.message),
        );
        self.listed = run(runner, actor, "connect.list", json!({}))
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Listed>>(v).ok())
            .unwrap_or_default();
    }
}

/// Starts `connect.regenerate` with a new seed on a thread of its own: it is a run like any other. Its candidates land
/// in the tray, which reads them on its next refresh.
fn regenerate(runner: &Arc<dyn Runner>, actor: &Actor, id: &CandidateId) {
    let (runner, actor, args) = (Arc::clone(runner), actor.clone(), json!({ "candidate": id, "seed": "new" }));
    let spawned = std::thread::Builder::new().name("connect-regenerate".into()).spawn(move || {
        if let Err(error) = run(runner.as_ref(), &actor, "connect.regenerate", args) {
            tracing::warn!(%error, "connect.regenerate failed");
        }
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "connect.regenerate did not start");
    }
}

/// The name a tile is known by: its prompt and its connector.
fn tile_name(ds: &Ds, row: &TrayRow) -> String {
    let untitled = say(ds, text::NO_PROMPT, &[]);
    let prompt = row.prompt.as_deref().filter(|p| !p.trim().is_empty()).unwrap_or(&untitled);
    let prompt: String = if prompt.chars().count() > 60 {
        prompt.chars().take(59).chain(['\u{2026}']).collect()
    } else {
        prompt.to_owned()
    };
    say(ds, text::TILE_NAME, &[("prompt", &prompt), ("connector", row.connector.as_str()), ("id", row.id.as_str())])
        .into_owned()
}

/// A kind with no picture, in words.
fn kind_word(ds: &Ds, kind: MediaKind) -> String {
    let word = match kind {
        MediaKind::Audio => text::KIND_AUDIO,
        MediaKind::Mesh => text::KIND_MESH,
        MediaKind::Motion => text::KIND_MOTION,
        MediaKind::Video => text::KIND_VIDEO,
        _ => text::KIND_TEXT,
    };
    say(ds, word, &[]).into_owned()
}

/// A tile for a file with no picture: its kind's icon and name, chosen by clicking like a picture's tile. True when
/// clicked.
fn kind_tile(ui: &mut Ui, ds: &Ds, kind: MediaKind, name: &str, chosen: bool) -> bool {
    let icon = match kind {
        MediaKind::Audio => icons::WAVEFORM,
        MediaKind::Mesh => icons::CUBE,
        MediaKind::Motion => icons::PERSON_SIMPLE_RUN,
        _ => icons::FILE,
    };
    let (rect, response) = ui.allocate_exact_size(vec2(TILE, TILE * 0.75), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, ds.theme.bg2);
    if chosen {
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(2.0, ds.theme.accent), egui::StrokeKind::Inside);
    }
    let centre = rect.center();
    painter.text(centre - vec2(0.0, 10.0), Align2::CENTER_CENTER, icon, ds.text.icon(28.0, false), ds.theme.text2);
    let word = kind_word(ds, kind);
    painter.text(centre + vec2(0.0, 22.0), Align2::CENTER_CENTER, word, ds.text.small.clone(), ds.theme.text2);
    crate::ds::paint::focus_ring(painter, &ds.theme, &response, rect, 6, crate::ds::paint::RingPlacement::Inside);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, chosen, name));
    ui.add(egui::Label::new(RichText::new(name).font(ds.text.small.clone()).color(ds.theme.text)).truncate());
    response.clicked()
}

/// The chips under a tile and its rights badge.
fn details(ui: &mut Ui, ds: &Ds, row: &TrayRow) {
    let mut chips = vec![row.connector.as_str().to_owned()];
    if let Some(model) = &row.model {
        chips.push(say(ds, text::MODEL, &[("model", model)]).into_owned());
    }
    if let Some(plan) = &row.plan {
        chips.push(say(ds, text::PLAN_CHIP, &[("plan", plan)]).into_owned());
    }
    chips.push(say(ds, text::COST, &[("usd", &omuzaki_connect_api::usd(row.usd_micros))]).into_owned());
    ui.label(RichText::new(chips.join(" \u{b7} ")).font(ds.text.small.clone()).color(ds.theme.text2));
    let (icon, line, colour) = match rights(ds, row.commercial, row.private_test) {
        Ok(line) => (icons::CHECK_CIRCLE, line, ds.theme.ok),
        Err(line) => (icons::PROHIBIT, line, ds.theme.warn),
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S4;
        ui.label(RichText::new(icon).font(ds.text.icon(14.0, false)).color(colour));
        ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text));
    });
    let licence = say(ds, text::TILE_LICENCE, &[("licence", &row.licence)]);
    ui.label(RichText::new(licence).font(ds.text.small.clone()).color(ds.theme.text3));
    if let Some(reason) = &row.reason {
        let line = say(ds, text::REJECTED_BECAUSE, &[("reason", reason)]);
        ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
    }
}

/// Asks the shell for the Generators page.
fn open_settings(requests: Option<&ShellRequests>) {
    if let Some(requests) = requests {
        requests.push(ShellRequest::Settings(
            crate::settings::Page::Connectors,
            Some(crate::settings::connectors::Connector::Generators),
        ));
    }
}

/// [`Tray`] as a registered panel.
#[derive(Debug, Default)]
pub struct TrayPanel(pub Tray);

impl Panel for TrayPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        if let Some(generation) = ctx.view::<Generation>() {
            self.0.ui(ui, generation, ctx.actor(), ctx.view::<ShellRequests>());
        } else {
            let ds = ds(ui.ctx());
            let (headline, line) = (say(&ds, text::NO_TRAY, &[]), say(&ds, text::NO_TRAY_HOW, &[]));
            EmptyState::new(icons::SPARKLE, &headline, &line).show(ui);
        }
    }
}
