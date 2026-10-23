//! The History panel (`shell.md` section 9): one list for the whole project, newest on top.
//!
//! - **Rows** are a fixed 56 pt, so only the visible ones are built and 50,000 records cost what 50 do: the step in the
//!   op's own terms (`part.set_param head › cranium/half_width = 0.610`), who made it, how long ago, and "undone"
//!   while it is. The filtered list is worked out when the log or a filter changes, not every frame.
//! - **Filters:** by who (everyone, you, agents, the app), by area (the op's first word: `face`, `part`) and by text.
//! - **Details:** a row chosen with a click, or Enter, opens a popover beside it with why it was made (the prompt or rule), every value it
//!   changed with before and after, and its actions: **Adjust** reopens the op's form in the palette with its
//!   arguments (Blender's Adjust Last Operation) and **Copy as MCP call** copies it as an agent would send it.
//! - **Preview:** opening a row's details also shows the document as it was right after that step in the viewport
//!   ([`Preview`]), without changing it: a "Viewing" line with **Back to now** says so, and Esc or closing the row
//!   goes back. The past state is worked out from the log's patches: every later step that is in effect is reversed,
//!   newest first, and every undone step up to it is applied again.
//! - **Pictures:** when the app takes them ([`HistoryPictures`]), each row starts with the character as it looked
//!   right after the step, loaded only for the rows on screen ([`super::pictures`]).
//! - **Keyboard:** ↑ and ↓ move through the rows, Enter opens or closes a row's details, Esc closes them.
//! - **Undo conflicts:** an undo the runner refuses because a later change touches the same values leaves a card
//!   saying so at the top of the list until it is dismissed or a step goes through.
//!
//! - **Compare:** with a row's details open, **Compare with…** then a second row (or Ctrl+click, or Ctrl+Enter on
//!   another row) opens the two states side by side with a heatmap and the values that differ ([`super::compare`]).
//!
//! **Restore this state** and **Name this version** run app core's `history.restore` and `history.name` (#443); a
//! build without them shows both off, saying why.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use egui::{Id, Key, Modifiers, RichText, ScrollArea, Sense, Ui, UiBuilder, vec2};
use omuzaki_doc_api::{DocPath, PatchError, Value, apply_to};
use omuzaki_ops_api::{Actor, Grants, Mode, OpRecord, Request, Runner, TxId, tool_name};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::Region;

use super::compare::Compare;
use super::pictures::{Atlas, HistoryPictures};
use super::requests::{ShellRequest, ShellRequests};
use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::ds;
use crate::ds::widgets::{ActorChip, ActorKind, Button, Choice, Dropdown, EmptyState, Segment, Segmented, TextField};
use crate::panels::edit::EditFocus;

omuzaki_ui_api::register_panel! { id: "history", title: "History", icon: icons::CLOCK, region: Region::Right,
min_size: [288.0, 160.0], order: 30, build: |_| Box::new(History::default()) }

/// A row's height, in points.
pub const ROW: f32 = 56.0;
/// The most changed values a row's details list.
const DETAIL_CHANGES: usize = 12;
/// The most characters of a value shown in the details.
const VALUE_CHARS: usize = 48;

/// The op log as the shell last read it: the view model the History panel reads from its context
/// ([`PanelCtx::view`]), kept current from the runner's events.
#[derive(Clone, Debug, Default)]
pub struct HistoryLog {
    /// The op log, oldest first; empty for a new project.
    pub records: Vec<OpRecord>,
    /// The transactions undone and not redone, as the runner's events told them.
    pub undone: HashSet<TxId>,
    /// When the studio first heard of each record, by `seq`, for its age. A record read from an earlier session's
    /// log has none: the runner's clock starts with the app, so its time says nothing about now.
    pub seen: HashMap<u64, Instant>,
    /// Why the log could not be read, when it could not.
    pub error: Option<String>,
    /// Why the person's last undo or redo was refused for a conflict, until it is dismissed or a step goes through.
    pub conflict: Option<String>,
}

/// The document as it was right after the step with `seq`, worked out from `current` and the log: every later step in
/// effect is reversed, newest first, and every undone step up to it is applied again, oldest first.
///
/// # Errors
/// A patch that does not fit the tree it is applied to: the log does not reach back to that step whole.
pub fn state_at(mut current: Value, log: &HistoryLog, seq: u64) -> Result<Value, PatchError> {
    for record in log.records.iter().rev().filter(|r| r.seq > seq && !log.undone.contains(&r.tx)) {
        apply_to(&mut current, &record.patch.inverse())?;
    }
    for record in log.records.iter().filter(|r| r.seq <= seq && log.undone.contains(&r.tx)) {
        apply_to(&mut current, &record.patch)?;
    }
    Ok(current)
}

/// A past state History shows in the viewport, by the step it follows.
#[derive(Clone, Debug)]
pub struct Viewing {
    /// The step's place in the log.
    pub seq: u64,
    /// The step, as its row names it.
    pub label: String,
    /// The whole document as it was right after the step.
    pub doc: Arc<Value>,
}

/// The past state on show, if any: shared by History, which sets it, and the viewport, which draws it in place of the
/// document. Clones share one state. The document itself never changes.
#[derive(Clone, Debug, Default)]
pub struct Preview(Arc<Mutex<Option<Viewing>>>);

impl Preview {
    /// Shows `viewing` from the next frame.
    pub fn view(&self, viewing: Viewing) {
        *self.lock() = Some(viewing);
    }

    /// Goes back to the document as it is.
    pub fn back(&self) {
        *self.lock() = None;
    }

    /// The step on show, by `seq`.
    pub fn seq(&self) -> Option<u64> {
        self.lock().as_ref().map(|v| v.seq)
    }

    /// Calls `f` with the past state on show, if any.
    pub fn with<R>(&self, f: impl FnOnce(Option<&Viewing>) -> R) -> R {
        f(self.lock().as_ref())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Viewing>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl HistoryLog {
    /// The step `actor`'s Ctrl+Z would undo: their newest step that is not undone.
    pub fn next_undo(&self, actor: &Actor) -> Option<&OpRecord> {
        self.records.iter().rev().find(|r| r.actor == *actor && !self.undone.contains(&r.tx))
    }

    /// The step `actor`'s Ctrl+Shift+Z would redo: the oldest of their newest run of undone steps.
    pub fn next_redo(&self, actor: &Actor) -> Option<&OpRecord> {
        let mut redo = None;
        for record in self.records.iter().rev().filter(|r| r.actor == *actor) {
            if !self.undone.contains(&record.tx) {
                break;
            }
            redo = Some(record);
        }
        redo
    }
}

/// Whose steps the list shows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
enum Who {
    #[default]
    Everyone,
    You,
    Agents,
    App,
}

impl Who {
    fn admits(self, actor: &Actor, you: &Actor) -> bool {
        match self {
            Self::Everyone => true,
            Self::You => actor == you,
            Self::Agents => matches!(actor, Actor::Agent { .. }),
            Self::App => matches!(actor, Actor::System | Actor::Plugin { .. }),
        }
    }
}

/// What the filtered list was worked out for, so it is worked out again only when one of these changes.
#[derive(Clone, Debug, Default, PartialEq)]
struct ShownFor {
    len: usize,
    last: u64,
    who: Who,
    area: usize,
    text: String,
}

/// The History panel's state.
#[derive(Default)]
pub struct History {
    text: String,
    who: Who,
    /// The chosen area: 0 is every area, `n` is `areas[n - 1]`.
    area: usize,
    /// The areas the log has steps in, sorted.
    areas: Vec<String>,
    /// The rows that pass the filters, newest first, as indices into the log.
    rows: Vec<usize>,
    shown_for: Option<ShownFor>,
    /// The record whose details are open, by `seq`.
    open: Option<u64>,
    /// The row that has the keyboard, by `seq`.
    focus: Option<u64>,
    /// A row to give the keyboard and scroll to on the next frame, by its place in `rows`.
    move_to: Option<usize>,
    /// The shell's request queue, for Adjust, Undo and Redo.
    requests: Option<ShellRequests>,
    /// The past state on show in the viewport.
    preview: Option<Preview>,
    /// Why the step last opened can't be shown, by `seq`.
    no_preview: Option<(u64, String)>,
    /// The rows the last frame drew, and the height of the list's view.
    drawn: std::ops::Range<usize>,
    view: f32,
    /// Where each step's picture comes from, when the app takes them, and the ones loaded.
    pictures: Option<HistoryPictures>,
    atlas: Atlas,
    /// The step chosen with "Compare with…", waiting for the row to compare it with, by `seq`.
    picking: Option<u64>,
    /// The two steps being compared.
    compare: Option<Compare>,
}

impl Panel for History {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let log = ctx.view::<HistoryLog>();
        let Some(log) = log.filter(|log| !log.records.is_empty()) else {
            return empty(ui, log);
        };
        self.requests = ctx.view::<ShellRequests>().cloned();
        self.preview = ctx.view::<Preview>().cloned();
        self.pictures = ctx.view::<HistoryPictures>().cloned();
        undo_bar(ui, self.requests.as_ref());
        conflict_card(ui, log, self.requests.as_ref());
        self.viewing_line(ui);
        self.picking_line(ui, log);
        self.filters(ui, log, ctx.actor());
        ui.add_space(4.0);
        ui.scope(|ui| self.list(ui, log, ctx.runner(), ctx.actor()));
        // The compare window takes Esc first, then a pick waiting for its second row, then the details.
        if let Some(compare) = &mut self.compare
            && !compare.show(ui.ctx(), self.pictures.as_ref(), ctx.view::<EditFocus>())
        {
            self.compare = None;
        } else if self.picking.is_some() && ui.ctx().input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.picking = None;
        } else if self.open.is_some() && ui.ctx().input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.set_open(None, log, ctx.runner());
        }
        // A step no longer in the log (a new project) takes its preview with it.
        if let (Some(preview), Some(seq)) = (&self.preview, self.preview.as_ref().and_then(Preview::seq))
            && !log.records.iter().any(|r| r.seq == seq)
        {
            preview.back();
        }
    }
}

impl History {
    /// The keys a row takes while it has the keyboard: ↑ and ↓ move, Enter opens or closes its details (or ends a
    /// pick), Ctrl+Enter compares it with the open or picked step.
    #[allow(clippy::too_many_arguments, reason = "the row's own facts, passed down from the list")]
    fn row_keys(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        record: &OpRecord,
        place: usize,
        open: bool,
        log: &HistoryLog,
        runner: &dyn Runner,
    ) {
        // The rows take ↑ and ↓ themselves, so egui's own arrow navigation never skips a row or leaves the list.
        let filter = egui::EventFilter { vertical_arrows: true, escape: open, ..egui::EventFilter::default() };
        ctx.memory_mut(|m| m.set_focus_lock_filter(id, filter));
        self.focus = Some(record.seq);
        let (down, up, enter, with) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::COMMAND, Key::Enter),
            )
        });
        if with && let Some(first) = self.picking.or(self.open) {
            self.compare_steps(first, record.seq, log, runner);
        }
        let enter = enter && !self.take_pick(record.seq, log, runner);
        if down && place + 1 < self.rows.len() {
            self.move_to = Some(place + 1);
        }
        if up && place > 0 {
            self.move_to = Some(place - 1);
        }
        if enter {
            self.set_open((!open).then_some(record.seq), log, runner);
            // The "Viewing" line above the list pushes it down: keep the opened row, and so its details, in view.
            self.move_to = self.move_to.or(Some(place));
        }
        if self.move_to.is_some() || enter {
            ctx.request_repaint();
        }
    }

    /// A click on a row: with Ctrl, compares it with the open step; while a pick waits, ends it; otherwise opens or
    /// closes its details.
    fn row_clicked(
        &mut self,
        ctx: &egui::Context,
        record: &OpRecord,
        place: usize,
        open: bool,
        log: &HistoryLog,
        runner: &dyn Runner,
    ) {
        let ctrl = ctx.input(|i| i.modifiers.command);
        match self.open.filter(|first| ctrl && *first != record.seq) {
            Some(first) => self.compare_steps(first, record.seq, log, runner),
            None if self.take_pick(record.seq, log, runner) => {}
            None => {
                self.set_open((!open).then_some(record.seq), log, runner);
                self.move_to = Some(place);
            }
        }
    }

    /// The two steps being compared, while the compare window is open.
    pub fn compare(&self) -> Option<&Compare> {
        self.compare.as_ref()
    }

    /// Opens the compare window over the steps `a` and `b`.
    fn compare_steps(&mut self, a: u64, b: u64, log: &HistoryLog, runner: &dyn Runner) {
        self.picking = None;
        let find = |seq: u64| log.records.iter().find(|r| r.seq == seq);
        if let (Some(a), Some(b)) = (find(a), find(b))
            && a.seq != b.seq
        {
            self.compare = Some(Compare::open(a, b, log, runner));
            self.set_open(None, log, runner);
        }
    }

    /// When "Compare with…" is waiting, compares its step with `seq` and says so.
    fn take_pick(&mut self, seq: u64, log: &HistoryLog, runner: &dyn Runner) -> bool {
        match self.picking.filter(|first| *first != seq) {
            Some(first) => {
                self.compare_steps(first, seq, log, runner);
                true
            }
            None => false,
        }
    }

    /// While "Compare with…" waits for its second row: which step, and how to cancel.
    fn picking_line(&self, ui: &mut Ui, log: &HistoryLog) {
        let Some(record) = self.picking.and_then(|seq| log.records.iter().find(|r| r.seq == seq)) else { return };
        let ctx = ui.ctx().clone();
        let line = say(&ctx, Word::HistoryComparePick, &[("label", &step_label(record))]);
        ui.add(egui::Label::new(RichText::new(line).color(ds(&ctx).theme.accent)).wrap());
    }

    /// How many steps' pictures are loaded as textures: at most the rows drawn lately, never the whole log.
    pub fn pictures_kept(&self) -> usize {
        self.atlas.kept()
    }

    /// Opens the details of the step `seq` (or closes them, with `None`) and shows the state after it in the viewport.
    fn set_open(&mut self, seq: Option<u64>, log: &HistoryLog, runner: &dyn Runner) {
        self.open = seq;
        self.no_preview = None;
        let Some(preview) = &self.preview else { return };
        let Some(record) = seq.and_then(|seq| log.records.iter().find(|r| r.seq == seq)) else {
            preview.back();
            return;
        };
        let current = runner.get(&DocPath::root(), &Grants::person()).map(Option::unwrap_or_default);
        match current.map_err(|e| e.message).and_then(|doc| state_at(doc, log, record.seq).map_err(|e| e.to_string())) {
            Ok(doc) => preview.view(Viewing { seq: record.seq, label: step_label(record), doc: Arc::new(doc) }),
            Err(why) => {
                preview.back();
                self.no_preview = Some((record.seq, why));
            }
        }
    }

    /// While a past state is on show: what it is, and the way back.
    fn viewing_line(&mut self, ui: &mut Ui) {
        let Some(preview) = self.preview.clone() else { return };
        let Some(label) = preview.with(|v| v.map(|v| v.label.clone())) else { return };
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        ui.horizontal_wrapped(|ui| {
            let line = say(&ctx, Word::HistoryViewing, &[("label", &label)]);
            ui.add(egui::Label::new(RichText::new(line).color(ds.theme.text)).truncate());
            if ui.add(Button::new(&say(&ctx, Word::HistoryBack, &[])).icon(icons::ARROW_U_UP_LEFT)).clicked() {
                preview.back();
                self.open = None;
            }
        });
    }

    /// The filter row: text, who and area. Works the filtered list out again when the log or a filter changed.
    fn filters(&mut self, ui: &mut Ui, log: &HistoryLog, you: &Actor) {
        let ctx = ui.ctx().clone();
        let filter = say(&ctx, Word::HistoryFilter, &[]);
        ui.add(TextField::new(&filter, &mut self.text).search().id_salt("history_filter"));
        let words = [
            say(&ctx, Word::HistoryEveryone, &[]),
            say(&ctx, Word::HistoryYou, &[]),
            say(&ctx, Word::HistoryAgents, &[]),
            say(&ctx, Word::HistorySystem, &[]),
        ];
        let segments = [
            Segment::new(Who::Everyone, &words[0]),
            Segment::new(Who::You, &words[1]),
            Segment::new(Who::Agents, &words[2]),
            Segment::new(Who::App, &words[3]),
        ];
        let who_label = say(&ctx, Word::HistoryWho, &[]);
        ui.add(Segmented::new(&who_label, &mut self.who, &segments).stretch().id_salt("history_who"));
        let all = say(&ctx, Word::HistoryAllAreas, &[]);
        let mut choices = vec![Choice::new(0, all.as_str())];
        choices.extend(self.areas.iter().enumerate().map(|(i, area)| Choice::new(i + 1, area.as_str())));
        let area_label = say(&ctx, Word::HistoryArea, &[]);
        let mut area = self.area;
        ui.add(Dropdown::new(&area_label, &mut area, &choices).stretch().id_salt("history_area"));
        self.area = area;
        self.refilter(log, you);
    }

    fn refilter(&mut self, log: &HistoryLog, you: &Actor) {
        let key = ShownFor {
            len: log.records.len(),
            last: log.records.last().map_or(0, |r| r.seq),
            who: self.who,
            area: self.area,
            text: self.text.trim().to_lowercase(),
        };
        if self.shown_for.as_ref() == Some(&key) {
            return;
        }
        let mut areas: Vec<String> = log.records.iter().map(|r| area_of(&r.op).to_owned()).collect();
        areas.sort();
        areas.dedup();
        // Keep the chosen area by name when the list of areas changes under it.
        let chosen = self.area.checked_sub(1).and_then(|i| self.areas.get(i)).cloned();
        self.area = chosen.and_then(|name| areas.iter().position(|a| *a == name)).map_or(0, |i| i + 1);
        self.areas = areas;
        let area = self.area.checked_sub(1).and_then(|i| self.areas.get(i));
        self.rows = log
            .records
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, r)| key.who.admits(&r.actor, you))
            .filter(|(_, r)| area.is_none_or(|a| area_of(&r.op) == a))
            .filter(|(_, r)| key.text.is_empty() || searchable(r).to_lowercase().contains(&key.text))
            .map(|(i, _)| i)
            .collect();
        self.shown_for = Some(ShownFor { area: self.area, ..key });
        self.move_to = None;
    }

    /// The rows, virtualised: only the visible ones are laid out.
    fn list(&mut self, ui: &mut Ui, log: &HistoryLog, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        if self.rows.is_empty() {
            let line = say(&ctx, Word::HistoryNoMatch, &[]);
            ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
            return;
        }
        let mut area = ScrollArea::vertical().id_salt("history_rows").auto_shrink([false, true]);
        if let Some(target) = self.move_to {
            // Bring the row into view, scrolling as little as it takes, as a list does under the arrow keys.
            // The last row drawn may be cut by the bottom edge, so a row is in view when it is before that one.
            let viewport = self.view;
            if target <= self.drawn.start && target > 0 {
                area = area.vertical_scroll_offset(target as f32 * ROW);
            } else if target + 1 >= self.drawn.end {
                area = area.vertical_scroll_offset(((target + 1) as f32 * ROW - viewport).max(0.0));
            }
        }
        let rows = self.rows.len();
        // `show_rows` places rows a row height plus the item spacing apart: with no spacing, a row is exactly `ROW`.
        ui.spacing_mut().item_spacing.y = 0.0;
        let output = area.show_rows(ui, ROW, rows, |ui, range| {
            self.drawn = range.clone();
            for place in range {
                let Some(record) = self.rows.get(place).and_then(|&i| log.records.get(i)) else { continue };
                self.row(ui, log, record, place, runner, actor);
            }
        });
        self.view = output.inner_rect.height();
    }

    /// One row: the step on top, who and when below. The row is one Tab stop; ↑ and ↓ move between rows.
    fn row(
        &mut self,
        ui: &mut Ui,
        log: &HistoryLog,
        record: &OpRecord,
        place: usize,
        runner: &dyn Runner,
        actor: &Actor,
    ) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let id = Id::new(("history_row", record.seq));
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        let response = ui.interact(rect, id, Sense::click());
        let undone = log.undone.contains(&record.tx);
        let open = self.open == Some(record.seq);
        let label = step_label(record);
        let who = who(&ctx, &record.actor);
        let age = log.seen.get(&record.seq).map(|at| age(&ctx, at.elapsed().as_secs()));
        let spoken = match (&age, undone) {
            (Some(age), true) => format!("{who}: {label}, {age}, {}", say(&ctx, Word::HistoryUndone, &[])),
            (Some(age), false) => format!("{who}: {label}, {age}"),
            (None, true) => format!("{who}: {label}, {}", say(&ctx, Word::HistoryUndone, &[])),
            (None, false) => format!("{who}: {label}"),
        };
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, open, &spoken));
        if self.move_to == Some(place) {
            response.request_focus();
            // The panel's body may scroll as a whole as well as the list: bring the row into view in both.
            response.scroll_to_me(None);
            self.move_to = None;
        }
        if response.has_focus() {
            self.row_keys(&ctx, id, record, place, open, log, runner);
        }
        if response.clicked() {
            self.row_clicked(&ctx, record, place, open, log, runner);
        }
        if open && ui.is_rect_visible(rect) {
            let no_preview =
                self.no_preview.as_ref().filter(|(seq, _)| *seq == record.seq).map(|(_, why)| why.as_str());
            let compare = Cell::new(false);
            let place = Place { requests: self.requests.as_ref(), runner, actor, no_preview, compare: &compare };
            popover(&ctx, rect, record, log, &place);
            if compare.get() {
                self.picking = Some(record.seq);
            }
        }
        let viewing = self.preview.as_ref().and_then(Preview::seq) == Some(record.seq);
        if ui.is_rect_visible(rect) {
            let fill = if open {
                Some(ds.theme.bg3)
            } else if response.hovered() || response.has_focus() {
                Some(ds.theme.bg2)
            } else {
                None
            };
            if let Some(fill) = fill {
                ui.painter().rect_filled(rect, 4.0, fill);
            }
            paint::focus_ring(ui.painter(), &ds.theme, &response, rect, 4, RingPlacement::Inside);
            let mut inner = rect.shrink2(vec2(8.0, 6.0));
            if let Some(pictures) = &self.pictures {
                let side = inner.height();
                let square = egui::Rect::from_min_size(inner.min, vec2(side, side));
                self.atlas.draw(ui, pictures, record.tx, log.seen.get(&record.seq).copied(), square);
                inner.min.x += side + 8.0;
            }
            let colour = if undone { ds.theme.text3 } else { ds.theme.text };
            ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add(egui::Label::new(RichText::new(&label).font(ds.text.mono.clone()).color(colour)).truncate());
                ui.horizontal(|ui| {
                    ui.add(ActorChip::new(&who, actor_kind(&record.actor)));
                    let quiet = |text: &str| RichText::new(text).font(ds.text.small.clone()).color(ds.theme.text3);
                    if let Some(age) = &age {
                        ui.label(quiet(age));
                    }
                    if undone {
                        ui.label(quiet(&say(&ctx, Word::HistoryUndone, &[])));
                    }
                    if viewing {
                        let chip = say(&ctx, Word::HistoryViewingChip, &[]);
                        ui.label(RichText::new(chip).font(ds.text.small.clone()).color(ds.theme.accent));
                    }
                });
            });
        }
    }
}

/// What a step's details need from the panel: the request queue, the runner and actor for Restore and Name, and why
/// the step can't be shown, if it can't.
struct Place<'a> {
    requests: Option<&'a ShellRequests>,
    runner: &'a dyn Runner,
    actor: &'a Actor,
    no_preview: Option<&'a str>,
    /// Set when "Compare with…" was pressed.
    compare: &'a Cell<bool>,
}

/// The width of a step's details, in points.
const POPOVER: f32 = 360.0;

/// A step's details in a popover beside its row (on the side with room, the left for the right dock): why, what
/// changed, and its actions.
fn popover(ctx: &egui::Context, row: egui::Rect, record: &OpRecord, log: &HistoryLog, place: &Place<'_>) {
    let screen = ctx.content_rect();
    let (pivot, at) = if row.left() - POPOVER - 8.0 >= screen.left() {
        (egui::Align2::RIGHT_TOP, row.left_top() - vec2(8.0, 0.0))
    } else {
        (egui::Align2::LEFT_TOP, row.right_top() + vec2(8.0, 0.0))
    };
    let ds = ds(ctx);
    egui::Area::new(Id::new(("history_details", record.seq)))
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(at)
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(ds.theme.bg2).show(ui, |ui| {
                ui.set_width(POPOVER.min(screen.width() - 32.0));
                details_ui(ui, record, log, place);
            });
        });
}

/// A record's details: why, what changed, and its actions.
fn details_ui(ui: &mut Ui, record: &OpRecord, log: &HistoryLog, place: &Place<'_>) {
    let requests = place.requests;
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    ScrollArea::vertical().id_salt("history_details").max_height(420.0).show(ui, |ui| {
        ui.label(RichText::new(record.op.as_str()).font(ds.text.mono.clone()).color(ds.theme.text));
        let who = who(&ctx, &record.actor);
        let line = match record.cause.as_ref().and_then(|c| c.text.as_deref()) {
            Some(cause) => say(&ctx, Word::HistoryByBecause, &[("who", &who), ("cause", cause)]),
            None => say(&ctx, Word::HistoryBy, &[("who", &who)]),
        };
        ui.label(RichText::new(line).color(ds.theme.text2));
        if log.undone.contains(&record.tx) {
            ui.label(RichText::new(say(&ctx, Word::HistoryUndoneLine, &[])).color(ds.theme.text2));
        }
        if let Some(why) = place.no_preview {
            ui.label(RichText::new(say(&ctx, Word::HistoryNoPreview, &[("why", why)])).color(ds.theme.text2));
        }
        ui.add_space(4.0);
        let changes = &record.patch.changes;
        for change in changes.iter().take(DETAIL_CHANGES) {
            let line =
                format!("{}: {} \u{2192} {}", change.path, shown(change.before.as_ref()), shown(change.after.as_ref()));
            ui.add(egui::Label::new(RichText::new(line).font(ds.text.mono.clone()).color(ds.theme.text2)).wrap());
        }
        if changes.len() > DETAIL_CHANGES {
            let more = (changes.len() - DETAIL_CHANGES).to_string();
            ui.label(RichText::new(say(&ctx, Word::HistoryMoreChanges, &[("count", &more)])).color(ds.theme.text3));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let adjust = say(&ctx, Word::HistoryAdjust, &[]);
            let copy = say(&ctx, Word::HistoryCopyMcp, &[]);
            if let Some(requests) = requests
                && ui.add(Button::new(&adjust).icon(icons::SLIDERS_HORIZONTAL)).clicked()
            {
                requests.push(ShellRequest::Adjust { op: record.op.clone(), args: record.args.clone() });
            }
            if ui.add(Button::new(&copy).ghost()).clicked() {
                let call = serde_json::json!({"name": tool_name(&record.op), "arguments": record.args});
                ctx.copy_text(serde_json::to_string_pretty(&call).unwrap_or_else(|_| call.to_string()));
            }
            let compare = say(&ctx, Word::HistoryCompare, &[]);
            if ui.add(Button::new(&compare).ghost().icon(icons::GIT_DIFF)).clicked() {
                place.compare.set(true);
            }
        });
        ui.label(
            RichText::new(say(&ctx, Word::HistoryCompareHint, &[])).font(ds.text.small.clone()).color(ds.theme.text3),
        );
        ui.horizontal_wrapped(|ui| {
            let to = serde_json::json!({"tx": record.tx});
            let restore =
                (OP_RESTORE, Word::HistoryRestore, icons::ARROW_COUNTER_CLOCKWISE, serde_json::json!({"to": to}));
            let label = step_label(record);
            let name = (OP_NAME, Word::HistoryName, icons::FLAG, serde_json::json!({"at": to, "name": label}));
            for (op, word, icon, args) in [restore, name] {
                let missing = (!place.runner.ops().iter().any(|info| info.name == op))
                    .then(|| say(&ctx, Word::HistoryNotInBuild, &[("op", op)]));
                let text = say(&ctx, word, &[]);
                let button = Button::new(&text).icon(icon).ghost();
                let button = match &missing {
                    Some(why) => button.disabled(why),
                    None => button,
                };
                if ui.add(button).clicked() {
                    run(place.runner, place.actor, op, args);
                }
            }
        });
    });
}

/// A value as the details show it: compact JSON, cut to [`VALUE_CHARS`]; "nothing" when absent.
fn shown(value: Option<&Value>) -> String {
    let Some(value) = value else { return "\u{2205}".to_owned() };
    let text = match value {
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), |f| format!("{f:.3}")),
        other => other.to_string(),
    };
    if text.chars().count() > VALUE_CHARS {
        let cut: String = text.chars().take(VALUE_CHARS).collect();
        format!("{cut}\u{2026}")
    } else {
        text
    }
}

/// App core's op that restores a past state as one undoable step (#443).
const OP_RESTORE: &str = "history.restore";
/// App core's op that names a past state as a pinned version (#443).
const OP_NAME: &str = "history.name";

/// Runs `op` for the person; a refusal is logged, and its effect (or none) reaches the list through the events.
fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    if let Err(error) = runner.run(request) {
        tracing::warn!(%error, op, "a history action failed");
    }
}

/// Undo and Redo for the person, through the shell, so the status bar and the conflict card hear of the result.
fn undo_bar(ui: &mut Ui, requests: Option<&ShellRequests>) {
    let egui_ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        let (undo, redo) = (say(&egui_ctx, Word::HistoryUndo, &[]), say(&egui_ctx, Word::HistoryRedo, &[]));
        if ui.add(Button::new(&undo).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE).shortcut_hint("Ctrl Z")).clicked()
            && let Some(requests) = requests
        {
            requests.push(ShellRequest::Undo);
        }
        if ui.add(Button::new(&redo).ghost().icon(icons::ARROW_CLOCKWISE).shortcut_hint("Ctrl Shift Z")).clicked()
            && let Some(requests) = requests
        {
            requests.push(ShellRequest::Redo);
        }
    });
}

/// The card an undo the runner refused for a conflict leaves, until it is dismissed or a step goes through.
fn conflict_card(ui: &mut Ui, log: &HistoryLog, requests: Option<&ShellRequests>) {
    let Some(message) = &log.conflict else { return };
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    egui::Frame::new()
        .fill(ds.theme.bg2)
        .stroke(egui::Stroke::new(1.0, ds.theme.warn))
        .corner_radius(4.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::WARNING).font(ds.text.icon(14.0, false)).color(ds.theme.warn));
                ui.label(RichText::new(say(&ctx, Word::HistoryConflict, &[])).strong());
            });
            ui.label(RichText::new(say(&ctx, Word::HistoryConflictLine, &[])).color(ds.theme.text2));
            ui.add(egui::Label::new(RichText::new(message).font(ds.text.small.clone()).color(ds.theme.text3)).wrap());
            if ui.add(Button::new(&say(&ctx, Word::HistoryDismiss, &[])).ghost()).clicked()
                && let Some(requests) = requests
            {
                requests.push(ShellRequest::DismissConflict);
            }
        });
    ui.add_space(4.0);
}

/// The panel with no log: the reason it cannot be read, or the empty state.
fn empty(ui: &mut Ui, log: Option<&HistoryLog>) {
    let ctx = ui.ctx().clone();
    if let Some(why) = log.and_then(|log| log.error.as_deref()) {
        let line = say(&ctx, Word::HistoryUnavailable, &[("why", why)]);
        ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
        return;
    }
    let (headline, line) = (say(&ctx, Word::HistoryHeadline, &[]), say(&ctx, Word::HistoryLine, &[]));
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(icons::CLOCK, &headline, &line).show(ui);
    });
}

/// The area an op belongs to: the first word of its name (`face` of `face.set_profile`).
fn area_of(op: &str) -> &str {
    op.split('.').next().unwrap_or(op)
}

/// The text the filter matches: the step, who made it and its arguments' text (a new name, a profile).
fn searchable(record: &OpRecord) -> String {
    let mut text = step_label(record);
    if let Actor::Agent { id, .. } | Actor::Plugin { id } | Actor::Person { name: id } = &record.actor {
        text.push(' ');
        text.push_str(id);
    }
    if let Value::Object(args) = &record.args {
        for value in args.values().filter_map(Value::as_str) {
            text.push(' ');
            text.push_str(value);
        }
    }
    text
}

/// How long ago, in the largest whole unit: "now", "4 s", "3 m", "2 h", "5 d".
fn age(ctx: &egui::Context, seconds: u64) -> String {
    let (word, count) = match seconds {
        0..5 => return say(ctx, Word::HistoryNow, &[]),
        5..60 => (Word::HistorySeconds, seconds),
        60..3_600 => (Word::HistoryMinutes, seconds / 60),
        3_600..86_400 => (Word::HistoryHours, seconds / 3_600),
        _ => (Word::HistoryDays, seconds / 86_400),
    };
    say(ctx, word, &[("count", &count.to_string())])
}

/// What kind of actor ran a step, for its chip.
pub fn actor_kind(actor: &Actor) -> ActorKind {
    match actor {
        Actor::Person { .. } => ActorKind::Person,
        Actor::Agent { .. } => ActorKind::Agent,
        Actor::Plugin { .. } => ActorKind::Plugin,
        Actor::System => ActorKind::System,
    }
}

/// Who ran a step, as History names them.
pub fn who(ctx: &egui::Context, actor: &Actor) -> String {
    match actor {
        Actor::Person { name } if name == "you" => say(ctx, Word::HistoryYou, &[]),
        Actor::Person { name } => name.clone(),
        Actor::Agent { id, .. } | Actor::Plugin { id } => id.clone(),
        Actor::System => say(ctx, Word::HistorySystem, &[]),
    }
}

/// What a step did, in the op's own terms: its name, then the part and parameter it set when it names them
/// (`part.set_param head › cranium/half_width = 0.610`).
pub fn step_label(record: &OpRecord) -> String {
    let arg = |key: &str| record.args.get(key);
    let text = |key: &str| arg(key).and_then(|v| v.as_str());
    let mut line = record.op.clone();
    if let Some(part) = text("part") {
        line.push(' ');
        line.push_str(part);
    }
    if let Some(path) = text("path") {
        line.push_str(" \u{203a} ");
        line.push_str(path);
    }
    if let Some(value) = arg("value") {
        let shown = value.as_f64().map_or_else(|| value.to_string(), |n| format!("{n:.3}"));
        line.push_str(" = ");
        line.push_str(&shown);
    }
    line
}
