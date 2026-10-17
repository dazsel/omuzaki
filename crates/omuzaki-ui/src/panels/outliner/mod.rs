//! The outliner: the open character's part tree (`shell.md` section 6).
//!
//! The tree is read from the document when it changes ([`DocWatch`]), never per frame, and flattened into the visible
//! rows once per change of the tree, the collapsed set or the filter ([`rows::Rows`]); a frame draws only the rows on
//! screen with `ScrollArea::show_rows`. Every change to the document is an op run through the runner:
//! `part.set_enabled` (the eye), `part.reorder` (Move up and down, and a drag among siblings), `part.duplicate`
//! (Ctrl+D) and `part.remove` (Delete; several at once are one transaction, so one undo step). The selection is the
//! shared [`EditFocus`], which the inspector and the viewport follow.
//!
//! **A drag** has one life cycle; every input that can end it is in the table.
//!
//! | State | Input | Next state |
//! |---|---|---|
//! | idle | a drag starts on a row | dragging that part (kept by id: the tree can be read again under the pointer) |
//! | dragging | the pointer is over a visible sibling row | that row shows the drop line (found before the row is painted) |
//! | dragging | the pointer is over anything else (another level, the gap under the rows, outside the list or the window) | no target, no line |
//! | dragging | the button is released with a target | idle; the part is moved with `part.reorder` (one transaction) |
//! | dragging | the button is released with no target, or the pointer is no longer down | idle; nothing is run |
//! | dragging | Esc | idle; nothing is run, and the release that follows does nothing |
//! | dragging | another character is opened, or the dragged part is gone when it is dropped | idle; nothing is run |
//!
//! A drop is applied by [`reorder`]: the layers that give the order, applied in an order the build rules accept at
//! every step, as one transaction.
//!
//! **Keyboard** (when a row has focus; one row is the Tab stop): Up and Down move (Shift extends), Left collapses or
//! goes to the parent, Right expands or goes to the first child, Home and End, Enter sends the keyboard to the
//! inspector, Delete, Ctrl+D, Ctrl+F or typing starts the filter, Esc cancels a drag. Every drag has a menu equivalent
//! in the row's context menu (WCAG 2.5.7).

mod reorder;
pub mod rows;
pub mod tree;

use std::collections::BTreeMap;

use egui::{EventFilter, Key, Modifiers, Rect, ScrollArea, Ui};
use omuzaki_char_api::{CharId, CharacterDef, PartId};
use omuzaki_ops_api::{Actor, Event, OpError};
use serde_json::json;

use self::rows::{Collapsed, Row, Rows};
use self::tree::Tree;
use super::edit::EditFocus;
use super::io::{DocWatch, Io};
use super::text::{self, say};
use crate::ds::icons;
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, DropLine, EmptyState, Expand, RowHit, RowIcon, TextField, TreeRow, TreeRowOutput};

/// What the outliner's rows are keyed by (with the part id): a row's widget id, so focus can be moved to it.
const ROW: &str = "panels.outliner.row";
/// The filter field's salt.
const FILTER: &str = "panels.outliner.filter";

/// The outliner's id, as the shell registers it.
pub const ID: &str = "outliner";

/// How a click or a key changes the selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pick {
    /// Select this part alone.
    Only,
    /// Add it, or take it out (Ctrl).
    Toggle,
    /// Select every row from the anchor to it (Shift).
    Range,
}

/// One thing the person asked for this frame, applied after the rows are drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Select(usize, Pick),
    SetCollapsed(usize, bool),
    SetEnabled(usize, bool),
    Move(usize, Step),
    Duplicate(usize),
    DeleteSelected,
    Delete(usize),
    Frame(usize),
    Drop { source: usize, target: usize, line: DropLine },
    ToInspector,
    StartFilter,
}

/// A move among siblings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Up,
    Down,
}

/// A drag in progress: the part dragged (by id, because the tree can be read again under the pointer) and where it
/// would land. The target is the row under the pointer *this frame*, found before the rows are painted.
#[derive(Clone, Debug)]
struct Drag {
    source: PartId,
    target: Option<(usize, DropLine)>,
}

/// The outliner panel.
#[derive(Debug)]
pub struct Outliner {
    focus: EditFocus,
    watch: DocWatch,
    /// The character the tree was read from, and the focus generation that was current then.
    character: Option<CharId>,
    tree: Tree,
    /// The document the tree was read from: the build rules a reorder is checked against.
    def: Option<CharacterDef>,
    /// Counts tree builds, for the row cache.
    built: u64,
    collapsed: Collapsed,
    rows: Rows,
    filter: String,
    /// The row that is the Tab stop and that the arrow keys move.
    cursor: Option<PartId>,
    /// Where a Shift range starts.
    anchor: Option<PartId>,
    /// Parts an agent touched since the person last selected them, with the agent's id.
    touched: BTreeMap<PartId, String>,
    drag: Option<Drag>,
    /// The cursor row had keyboard focus last frame.
    keyboard: bool,
    /// Move keyboard focus to the cursor row (and scroll to it) this frame.
    follow: bool,
    /// Move keyboard focus to the filter this frame.
    to_filter: bool,
    /// The scroll position and height of the rows last frame, to scroll the cursor into view.
    view: (f32, f32),
    /// Why the last op was refused, or why the tree can't be read.
    problem: Option<String>,
}

impl Outliner {
    /// An outliner over `focus`.
    pub fn new(focus: EditFocus) -> Self {
        Self {
            focus,
            watch: DocWatch::default(),
            character: None,
            tree: Tree::default(),
            def: None,
            built: 0,
            collapsed: Collapsed::default(),
            rows: Rows::default(),
            filter: String::new(),
            cursor: None,
            anchor: None,
            touched: BTreeMap::new(),
            drag: None,
            keyboard: false,
            follow: false,
            to_filter: false,
            view: (0.0, 0.0),
            problem: None,
        }
    }

    /// The tree as last read.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Whether the panel can work: a character is open.
    pub fn available(&self) -> bool {
        self.focus.character().is_some()
    }

    /// Draws the outliner for one frame.
    pub fn ui(&mut self, ui: &mut Ui, io: Io<'_>) {
        let ds = ds(ui.ctx());
        let Some(character) = self.focus.character() else {
            EmptyState::new(
                icons::TREE_STRUCTURE,
                &say(&ds, text::NO_CHARACTER, &[]),
                &say(&ds, text::OPEN_CHARACTER, &[]),
            )
            .show(ui);
            return;
        };
        self.refresh(&character, io);
        self.note_agents(&character, io.events);
        self.filter_field(ui, &ds);
        if let Some(problem) = &self.problem {
            ui.label(egui::RichText::new(problem.as_str()).color(ds.theme.danger));
        }
        if self.tree.is_empty() {
            EmptyState::new(icons::TREE_STRUCTURE, &say(&ds, text::NO_PARTS, &[]), &say(&ds, text::ADD_PART, &[]))
                .show(ui);
            return;
        }
        let action = self.draw_rows(ui, &ds);
        if let Some(action) = action {
            self.perform(action, &character, io, &ds);
        }
    }

    /// Reads the tree again when the document or the open character changed.
    fn refresh(&mut self, character: &CharId, io: Io<'_>) {
        let other = self.character.as_ref() != Some(character);
        if !self.watch.stale(io.events) && !other {
            return;
        }
        match io.character(character) {
            Ok(Some(def)) => {
                self.tree = Tree::of(&def);
                self.def = Some(def);
                self.problem = None;
            }
            Ok(None) => (self.tree, self.def) = (Tree::default(), None),
            Err(e) => {
                (self.tree, self.def) = (Tree::default(), None);
                self.problem = Some(e.message);
            }
        }
        self.built += 1;
        self.watch.built();
        if other {
            self.character = Some(character.clone());
            (self.cursor, self.anchor, self.drag) = (None, None, None);
            self.touched.clear();
        }
        if self.cursor.as_ref().is_some_and(|c| self.tree.find(c).is_none()) {
            self.cursor = self.focus.primary().filter(|p| self.tree.find(p).is_some());
        }
    }

    /// Marks the parts an agent changed this frame (the violet dot), until the person selects them.
    fn note_agents(&mut self, character: &CharId, events: &[Event]) {
        for event in events {
            let Event::OpApplied { record, .. } = event else { continue };
            let Actor::Agent { id: agent, .. } = &record.actor else { continue };
            for change in &record.patch.changes {
                if let ["characters", c, "parts", part, ..] =
                    change.path.segments().iter().map(String::as_str).collect::<Vec<_>>().as_slice()
                    && *c == character.as_str()
                    && let Ok(part) = PartId::new(*part)
                {
                    self.touched.insert(part, agent.clone());
                }
            }
        }
    }

    fn filter_field(&mut self, ui: &mut Ui, ds: &Ds) {
        let label = say(ds, text::FILTER, &[]);
        let field = TextField::new(&label, &mut self.filter).search().id_salt(FILTER);
        let response = ui.add(field);
        if std::mem::take(&mut self.to_filter) {
            response.request_focus();
        }
        // Down from the filter goes to the first row; egui's own arrow-key focus moves are locked out so it is ours.
        if response.has_focus() {
            let filter = EventFilter { vertical_arrows: true, ..EventFilter::default() };
            ui.memory_mut(|m| m.set_focus_lock_filter(response.id, filter));
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                self.follow = true;
            }
        }
    }

    /// Draws the visible rows and reads the pointer and the keyboard. Returns what the person asked for.
    fn draw_rows(&mut self, ui: &mut Ui, ds: &Ds) -> Option<Action> {
        self.rows.get(&self.tree, self.built, &self.collapsed, &self.filter);
        let rows = self.rows.as_slice();
        if rows.is_empty() {
            ui.label(say(ds, text::NO_MATCH, &[("filter", &self.filter)]).as_ref());
            return None;
        }
        let row_of = |part: &PartId| self.tree.find(part).and_then(|node| self.rows.position_of(node));
        let cursor_row = self.cursor.as_ref().and_then(row_of).unwrap_or(0);
        let mut action = if self.keyboard { keys(ui, &self.tree, rows, cursor_row, self.focus.count()) } else { None };
        if let Some(Action::Select(node, _)) = action {
            self.follow = true;
            self.cursor = Some(self.tree.nodes()[node].id.clone());
        }
        let follow = std::mem::take(&mut self.follow);
        let row_of = |part: &PartId| self.tree.find(part).and_then(|node| self.rows.position_of(node));
        let target_row = self.cursor.as_ref().and_then(row_of).unwrap_or(cursor_row);
        let height = ds.metrics.tree_row;
        let mut area = ScrollArea::vertical().id_salt("panels.outliner.rows").auto_shrink([false, false]);
        if follow {
            #[allow(clippy::cast_precision_loss, reason = "a row index is far below 2^24")]
            let top = target_row as f32 * height;
            let (offset, view) = self.view;
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if top + height > offset + view {
                area = area.vertical_scroll_offset(top + height - view);
            }
        }
        let invalid = say(ds, text::PART_INVALID, &[]);
        // The drop target is what the pointer is over this frame: a release outside every row has none, and cancels.
        if let Some(d) = self.drag.as_mut() {
            d.target = None;
        }
        let (tree, focus, touched, drag) = (&self.tree, &self.focus, &self.touched, &mut self.drag);
        let (mut keyboard, mut refocus) = (false, false);
        let output = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            area.show_rows(ui, height, rows.len(), |ui, range| {
                for at in range {
                    let row = rows[at];
                    let Some(node) = tree.node(row.node) else { continue };
                    let tab_stop = at == target_row;
                    // The drop line is found from the pointer before the row is painted: the row paints it.
                    let slot = Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), height));
                    let line = drop_line(ui, slot, tree, row.node, drag);
                    let drawn = draw_row(ui, ds, node, row, focus, touched, &invalid, tab_stop, line);
                    // A row that kept the keyboard after the cursor moved on (a copy was selected, a part was
                    // removed) hands it to the cursor row next frame.
                    if !tab_stop && drawn.response.has_focus() {
                        keyboard = true;
                        refocus = true;
                    }
                    if tab_stop {
                        keyboard |= drawn.response.has_focus();
                        if follow {
                            drawn.response.request_focus();
                            keyboard = true;
                        }
                        if keyboard {
                            let filter = EventFilter {
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                ..EventFilter::default()
                            };
                            ui.memory_mut(|m| m.set_focus_lock_filter(drawn.response.id, filter));
                        }
                    }
                    action = action.or_else(|| row_action(&drawn, row, node.enabled));
                    if drawn.response.drag_started() {
                        *drag = Some(Drag { source: node.id.clone(), target: None });
                    }
                    if let Some(menu) = context_menu(&drawn.response, ds, row.node, node.enabled) {
                        action = Some(menu);
                    }
                }
            })
        });
        let inner = output.inner;
        self.view = (inner.state.offset.y, inner.inner_rect.height());
        self.keyboard = keyboard;
        self.follow |= refocus;
        if action == Some(Action::StartFilter) {
            // Letters typed while a row had focus start the filter with them.
            ui.input(|i| {
                for event in &i.events {
                    if let egui::Event::Text(t) = event {
                        self.filter.push_str(t);
                    }
                }
            });
        }
        action.or_else(|| self.finish_drag(ui))
    }

    /// Ends a drag: a release over a sibling drops there, Esc or a release anywhere else cancels it.
    fn finish_drag(&mut self, ui: &Ui) -> Option<Action> {
        let drag = self.drag.clone()?;
        let (released, escaped) =
            ui.input(|i| (i.pointer.any_released() || !i.pointer.any_down(), i.key_pressed(Key::Escape)));
        if escaped {
            self.drag = None;
            return None;
        }
        if !released {
            return None;
        }
        self.drag = None;
        // The part is found again by id: the tree may have been read anew since the drag began.
        let source = self.tree.find(&drag.source)?;
        let (target, line) = drag.target?;
        Some(Action::Drop { source, target, line })
    }

    /// Does what the person asked for.
    fn perform(&mut self, action: Action, character: &CharId, io: Io<'_>, ds: &Ds) {
        // An action taken from the keyboard leaves the keyboard on the cursor row, wherever the cursor went.
        if self.keyboard && action != Action::StartFilter {
            self.follow = true;
        }
        let id_of = |tree: &Tree, node: usize| tree.node(node).map(|n| n.id.clone());
        let result = match action {
            Action::Select(node, pick) => {
                self.select(node, pick);
                // The row clicked takes the keyboard, so the keys act on what was just picked.
                self.follow = true;
                Ok(())
            }
            Action::SetCollapsed(node, collapsed) => {
                if let Some(id) = id_of(&self.tree, node) {
                    self.collapsed.set(&id, collapsed);
                }
                Ok(())
            }
            Action::SetEnabled(node, enabled) => {
                self.part_op(io, character, node, "part.set_enabled", json!({"enabled": enabled}))
            }
            Action::Move(node, step) => self.step(io, character, node, step),
            Action::Drop { source, target, line } => self.drop(io, character, source, target, line),
            Action::Duplicate(node) => self.duplicate(io, character, node),
            Action::Delete(node) => self.delete(io, character, &[node]),
            Action::DeleteSelected => {
                let nodes: Vec<usize> = self.focus.selected().iter().filter_map(|p| self.tree.find(p)).collect();
                self.delete(io, character, &nodes)
            }
            Action::Frame(node) => {
                self.select(node, Pick::Only);
                self.focus.frame_selection();
                Ok(())
            }
            Action::ToInspector => {
                self.focus.send_to_inspector();
                Ok(())
            }
            Action::StartFilter => {
                self.to_filter = true;
                Ok(())
            }
        };
        match result {
            Ok(()) => {}
            Err(e) => self.problem = Some(say(ds, text::REFUSED, &[("reason", &e.message)]).into_owned()),
        }
    }

    fn select(&mut self, node: usize, pick: Pick) {
        let Some(id) = self.tree.node(node).map(|n| n.id.clone()) else { return };
        self.touched.remove(&id);
        match pick {
            Pick::Only => {
                self.focus.select(id.clone());
                self.anchor = Some(id.clone());
            }
            Pick::Toggle => {
                self.focus.toggle(&id);
                self.anchor = Some(id.clone());
            }
            Pick::Range => {
                let rows = self.rows.get(&self.tree, self.built, &self.collapsed, &self.filter);
                let at = |p: &PartId| rows.iter().position(|r| &self.tree.nodes()[r.node].id == p);
                let to = at(&id).unwrap_or(0);
                let from = self.anchor.as_ref().and_then(at).unwrap_or(to);
                let (lo, hi) = (from.min(to), from.max(to));
                let mut parts: Vec<PartId> =
                    rows[lo..=hi].iter().map(|r| self.tree.nodes()[r.node].id.clone()).collect();
                // The part clicked last is the primary one.
                if to < from {
                    parts.reverse();
                }
                self.focus.set(parts);
            }
        }
        self.cursor = Some(id);
    }

    /// Runs `op` on part `node` with `extra` arguments added to the character and the part.
    fn part_op(
        &mut self,
        io: Io<'_>,
        character: &CharId,
        node: usize,
        op: &str,
        extra: serde_json::Value,
    ) -> Result<(), OpError> {
        let Some(part) = self.tree.node(node).map(|n| n.id.clone()) else { return Ok(()) };
        let mut args = json!({"character": character, "part": part});
        if let (Some(args), serde_json::Value::Object(extra)) = (args.as_object_mut(), extra) {
            args.extend(extra);
        }
        io.run(op, args, None)?;
        self.problem = None;
        self.watch.force();
        Ok(())
    }

    /// Moves part `node` one place among its siblings by giving it a layer just past its neighbour's.
    fn step(&mut self, io: Io<'_>, character: &CharId, node: usize, step: Step) -> Result<(), OpError> {
        let siblings = self.tree.siblings(node);
        let Some(at) = siblings.iter().position(|&s| s == node) else { return Ok(()) };
        let neighbour = match step {
            Step::Up => at.checked_sub(1).and_then(|i| siblings.get(i)),
            Step::Down => siblings.get(at + 1),
        };
        let Some(&neighbour) = neighbour else { return Ok(()) };
        let line = if step == Step::Up { DropLine::Above } else { DropLine::Below };
        self.drop(io, character, node, neighbour, line)
    }

    /// Puts `source` just before or after its sibling `target` in build order.
    fn drop(
        &mut self,
        io: Io<'_>,
        character: &CharId,
        source: usize,
        target: usize,
        line: DropLine,
    ) -> Result<(), OpError> {
        if source == target || !same_parent(&self.tree, source, target) || line == DropLine::None {
            return Ok(());
        }
        // The siblings as they will be, then the layer changes that make that the build order ([`reorder`]).
        let mut order: Vec<usize> = self.tree.siblings(source).iter().copied().filter(|&s| s != source).collect();
        let Some(at) = order.iter().position(|&s| s == target) else { return Ok(()) };
        let moved = if line == DropLine::Above { at } else { at + 1 };
        order.insert(moved, source);
        let Some(def) = &self.def else { return Ok(()) };
        let changes = reorder::plan(def, &self.tree, &order, moved)?;
        let tx = (changes.len() > 1).then(|| io.runner.begin("Reorder part", io.actor));
        for &(node, layer) in &changes {
            let Some(id) = self.tree.node(node).map(|n| n.id.clone()) else { continue };
            let args = json!({"character": character, "part": id, "layer": layer});
            if let Err(e) = io.run("part.reorder", args, tx) {
                if let Some(tx) = tx {
                    io.runner.rollback(tx, io.actor)?;
                }
                return Err(e);
            }
        }
        if let Some(tx) = tx {
            io.runner.commit(tx, io.actor)?;
        }
        self.problem = None;
        self.watch.force();
        Ok(())
    }

    /// Copies part `node` under the first free id of the form `<id>-copy`, `<id>-copy-2`, …
    fn duplicate(&mut self, io: Io<'_>, character: &CharId, node: usize) -> Result<(), OpError> {
        let Some(id) = self.tree.node(node).map(|n| n.id.clone()) else { return Ok(()) };
        let free = (1..=self.tree.len() + 1)
            .map(|n| if n == 1 { format!("{id}-copy") } else { format!("{id}-copy-{n}") })
            .filter_map(|name| PartId::new(name).ok())
            .find(|candidate| self.tree.find(candidate).is_none());
        let Some(new_id) = free else {
            return Err(OpError::invalid("new_id", format!("no free id for a copy of '{id}'")));
        };
        self.part_op(io, character, node, "part.duplicate", json!({"new_id": new_id}))?;
        self.focus.select(new_id.clone());
        self.cursor = Some(new_id);
        Ok(())
    }

    /// Removes `nodes`, children before their parents, as one transaction: one undo step however many there are.
    fn delete(&mut self, io: Io<'_>, character: &CharId, nodes: &[usize]) -> Result<(), OpError> {
        let mut parts: Vec<(usize, PartId)> =
            nodes.iter().filter_map(|&n| self.tree.node(n).map(|node| (node.depth, node.id.clone()))).collect();
        parts.sort_by_key(|p| std::cmp::Reverse(p.0));
        let tx = (parts.len() > 1).then(|| io.runner.begin("Delete parts", io.actor));
        for (_, part) in &parts {
            if let Err(e) = io.run("part.remove", json!({"character": character, "part": part}), tx) {
                if let Some(tx) = tx {
                    io.runner.rollback(tx, io.actor)?;
                }
                return Err(e);
            }
        }
        if let Some(tx) = tx {
            io.runner.commit(tx, io.actor)?;
        }
        self.focus.clear();
        self.problem = None;
        self.watch.force();
        Ok(())
    }
}

/// Draws one row.
#[allow(clippy::too_many_arguments, reason = "one row's inputs, borrowed apart from the panel")]
fn draw_row(
    ui: &mut Ui,
    ds: &Ds,
    node: &tree::Node,
    row: Row,
    focus: &EditFocus,
    touched: &BTreeMap<PartId, String>,
    invalid: &str,
    tab_stop: bool,
    line: DropLine,
) -> TreeRowOutput {
    let mut icons: [RowIcon<'_>; 3] = [RowIcon::Visible(node.enabled); 3];
    let mut count = 1;
    if let Some(by) = touched.get(&node.id) {
        icons[count] = RowIcon::AgentTouched { by };
        count += 1;
    }
    if node.invalid {
        icons[count] = RowIcon::GateWarning { text: invalid };
        count += 1;
    }
    let mut tree_row = TreeRow::new((ROW, node.id.as_str()), node.id.as_str(), node.icon)
        .level(node.depth)
        .expand(row.expand)
        .selected(focus.is_selected(&node.id), true)
        .tab_stop(tab_stop)
        .icons(&icons[..count])
        .drop_line(line);
    if let Some(range) = row.matched {
        tree_row = tree_row.matched(range);
    }
    let out = tree_row.show(ui);
    // The row draws its state icons with tooltips; a screen reader hears them as the row's description. The closure
    // runs only while AccessKit is on, so a frame without it allocates nothing here.
    ui.ctx().accesskit_node_builder(out.response.id, |access| {
        let names: Vec<String> = icons[..count]
            .iter()
            .map(|icon| match icon {
                RowIcon::Visible(true) => ds.text(Msg::RowVisible).into_owned(),
                RowIcon::Visible(false) => ds.text(Msg::RowHidden).into_owned(),
                RowIcon::AgentTouched { by } => format!("{} ({by})", ds.text(Msg::RowAgentTouched)),
                RowIcon::GateWarning { text } => format!("{}: {text}", ds.text(Msg::RowGateWarning)),
                RowIcon::Locked { owner } => format!("{} ({owner})", ds.text(Msg::RowLocked)),
            })
            .collect();
        access.set_description(names.join(&ds.text(Msg::ListSeparator)));
    });
    out
}

/// What a click on a row asks for.
fn row_action(drawn: &TreeRowOutput, row: Row, enabled: bool) -> Option<Action> {
    Some(match drawn.hit? {
        RowHit::Chevron => Action::SetCollapsed(row.node, row.expand == Expand::Open),
        RowHit::Icon(0) => Action::SetEnabled(row.node, !enabled),
        RowHit::Icon(_) | RowHit::Row => {
            let mods = drawn.response.ctx.input(|i| i.modifiers);
            let pick = if mods.shift {
                Pick::Range
            } else if mods.command {
                Pick::Toggle
            } else {
                Pick::Only
            };
            if drawn.response.double_clicked() {
                return Some(Action::Frame(row.node));
            }
            Action::Select(row.node, pick)
        }
    })
}

/// While a drag is going on, the drop line this row takes: the pointer must be over the row, over the part of it the
/// list shows (a row cut by the list's edge is not a target outside it), and the row a sibling of the dragged part.
/// `slot` is where the row is about to be laid out. The target is recorded for [`Outliner::finish_drag`]; a frame in
/// which no row takes it leaves none, so a release there cancels.
fn drop_line(ui: &Ui, slot: Rect, tree: &Tree, node: usize, drag: &mut Option<Drag>) -> DropLine {
    let Some(d) = drag.as_mut() else { return DropLine::None };
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let Some(pointer) = pointer.filter(|p| slot.contains(*p) && ui.clip_rect().contains(*p)) else {
        return DropLine::None;
    };
    let Some(source) = tree.find(&d.source) else { return DropLine::None };
    if source == node || !same_parent(tree, source, node) {
        return DropLine::None;
    }
    let line = if pointer.y < slot.center().y { DropLine::Above } else { DropLine::Below };
    d.target = Some((node, line));
    line
}

/// Whether parts `a` and `b` are siblings.
fn same_parent(tree: &Tree, a: usize, b: usize) -> bool {
    matches!((tree.node(a), tree.node(b)), (Some(x), Some(y)) if x.parent == y.parent)
}

/// The row's context menu: the menu equivalent of every drag and key.
fn context_menu(response: &egui::Response, ds: &Ds, node: usize, enabled: bool) -> Option<Action> {
    let mut picked = None;
    response.context_menu(|ui| {
        let mut item = |ui: &mut Ui, label: &str, action: Action| {
            if ui.add(Button::new(&say(ds, label, &[])).ghost()).clicked() {
                picked = Some(action);
                ui.close();
            }
        };
        item(ui, text::FRAME, Action::Frame(node));
        // `shell.md` section 6 lists Rename (F2); no op renames a part yet, so it is shown, disabled, with the reason.
        ui.add(Button::new(&say(ds, text::RENAME, &[])).ghost().disabled(&say(ds, text::RENAME_NOT_YET, &[])));
        item(ui, if enabled { text::DISABLE } else { text::ENABLE }, Action::SetEnabled(node, !enabled));
        item(ui, text::MOVE_UP, Action::Move(node, Step::Up));
        item(ui, text::MOVE_DOWN, Action::Move(node, Step::Down));
        item(ui, text::DUPLICATE, Action::Duplicate(node));
        item(ui, text::DELETE, Action::Delete(node));
    });
    picked
}

/// The keys of the focused tree (`shell.md` section 6).
fn keys(ui: &Ui, tree: &Tree, rows: &[Row], at: usize, selected: usize) -> Option<Action> {
    let row = *rows.get(at)?;
    let node = tree.node(row.node)?;
    let pressed = |mods: Modifiers, key: Key| ui.input_mut(|i| i.consume_key(mods, key));
    let to = |i: usize, pick: Pick| rows.get(i).map(|r| Action::Select(r.node, pick));
    let shift = ui.input(|i| i.modifiers.shift);
    let pick = if shift { Pick::Range } else { Pick::Only };
    let mods = if shift { Modifiers::SHIFT } else { Modifiers::NONE };
    if pressed(mods, Key::ArrowDown) {
        return to((at + 1).min(rows.len() - 1), pick);
    }
    if pressed(mods, Key::ArrowUp) {
        return to(at.saturating_sub(1), pick);
    }
    if pressed(mods, Key::Home) {
        return to(0, pick);
    }
    if pressed(mods, Key::End) {
        return to(rows.len() - 1, pick);
    }
    if pressed(Modifiers::NONE, Key::ArrowLeft) {
        return match (row.expand, node.parent) {
            (Expand::Open, _) => Some(Action::SetCollapsed(row.node, true)),
            (_, Some(parent)) => Some(Action::Select(parent, Pick::Only)),
            _ => None,
        };
    }
    if pressed(Modifiers::NONE, Key::ArrowRight) {
        return match row.expand {
            Expand::Closed => Some(Action::SetCollapsed(row.node, false)),
            Expand::Open => to(at + 1, Pick::Only),
            Expand::Leaf => None,
        };
    }
    if pressed(Modifiers::NONE, Key::Enter) {
        return Some(Action::ToInspector);
    }
    if pressed(Modifiers::NONE, Key::Delete) {
        return Some(if selected > 0 { Action::DeleteSelected } else { Action::Delete(row.node) });
    }
    if pressed(Modifiers::COMMAND, Key::D) {
        return Some(Action::Duplicate(row.node));
    }
    if pressed(Modifiers::COMMAND, Key::F) {
        return Some(Action::StartFilter);
    }
    // Typing a letter starts the filter with it.
    let typed = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if !t.trim().is_empty())));
    typed.then_some(Action::StartFilter)
}

/// The outliner as the shell shows it, over the shell's [`EditFocus`].
struct OutlinerPanel(Outliner);

impl omuzaki_ui_api::panel::Panel for OutlinerPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) {
        self.0.ui(ui, Io::from_ctx(ctx));
    }

    fn available(&self, _ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) -> bool {
        self.0.available()
    }
}

omuzaki_ui_api::register_panel! {
    id: "outliner", title: "Outliner", icon: icons::TREE_STRUCTURE,
    region: omuzaki_ui_api::registry::Region::Left, min_size: [240.0, 160.0], order: 10,
    empty: omuzaki_ui_api::panel::EmptyState::new(text::NO_CHARACTER, text::OPEN_CHARACTER),
    build: |ctx| Box::new(OutlinerPanel(Outliner::new(EditFocus::from_ctx(ctx)))),
}
