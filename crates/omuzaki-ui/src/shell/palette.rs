//! The command palette (`shell.md` section 8): Ctrl+K finds every registered op, panel and workspace, and runs an op
//! with its arguments in a form generated from the op's own schema, so the palette can run anything an agent can
//! over MCP, under the same name.
//!
//! - **Index.** Built when the palette opens, from `Runner::ops()` and the shell's registry, so it is never stale and
//!   costs nothing while closed. Typing filters it in place; nothing is allocated per keystroke beyond the ranking.
//! - **Ranking.** Exact name, then prefix, then a word prefix (after `.`, `_` or a space), then a substring, then the
//!   letters in order (fuzzy); ties go to the shorter name, then the name. `>` narrows to ops, `/` to panels and
//!   workspaces. The ops run last come first among equals, so with nothing typed the list starts with them; the
//!   recents are kept across openings and, in the studio, in the user's config folder.
//! - **Selection.** An op argument called `character` or `part` starts as the open character and the selected part,
//!   so "set a parameter of this" is one Enter away.
//! - **Running.** Enter runs (one undo step, as the person), Shift+Enter is a dry run that changes nothing and says what
//!   would change. An op with no arguments to fill runs at once; one with arguments opens its form first. A query shows
//!   its answer in place. Errors show the op's own message and hint, as an agent reads them.
//! - **Live preview.** While an edit's form is open, its arguments are dry-run 120 ms after they stop changing, and the
//!   form says what the run would change before anything does.
//! - **Same as MCP.** Under the form: the MCP tool the op is, and the scope it needs. Ctrl+Shift+C (or the button)
//!   copies the call an agent would make, `{"name": "face_set_profile", "arguments": {..}}`, with the tool named by
//!   [`omuzaki_ops_api::tool_name`] as the MCP server names it; "Copy as CLI" copies the same run for `omuzaki run`.

use std::path::PathBuf;

use egui::{Align2, Context, Id, Key, Modifiers, RichText, ScrollArea, Ui, vec2};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Grants, Mode, OpInfo, OpKind, Request, Runner, tool_name};

use super::text::{Word, say, title};
use crate::ds::icons;
use crate::ds::schema_form::{Form, FormValue, SchemaForm};
use crate::ds::theme::ds;
use crate::ds::tokens::Status;
use crate::ds::widgets::listbox::{self, ListItem, Listbox};
use crate::ds::widgets::{Button, StatusChip, TextField};
use crate::panels::edit::EditFocus;

/// The palette's width, in points.
const WIDTH: f32 = 640.0;
/// How far below the top of the window it opens.
const TOP: f32 = 72.0;
/// The most lines of a query's answer shown in place.
const ANSWER_LINES: usize = 40;
/// How many ops the palette remembers as recent.
const RECENTS: usize = 8;
/// How long an edit's arguments must stay the same before the form previews them with a dry run, in seconds.
const PREVIEW_AFTER: f64 = 0.12;

/// What an entry does when chosen.
#[derive(Clone, Debug)]
enum Target {
    /// Runs the op at this index of the ops list.
    Op(usize),
    /// Shows a registered panel.
    Panel(&'static str),
    /// Switches to the workspace at this index of the tab bar.
    Workspace(usize),
    /// Makes a character from the query's words with the describe op at this index, and opens it.
    Describe(usize),
}

/// The op that makes a character from words; the palette offers it when the query reads like a description.
const DESCRIBE_OP: &str = "character.create_from_description";
/// The fewest words a query needs before the palette offers to make a character from it.
const DESCRIBE_WORDS: usize = 3;

/// One thing the palette can find.
#[derive(Clone, Debug)]
struct Entry {
    /// What it is called: an op's dotted name, a panel's or workspace's title.
    name: String,
    /// The name in lower case, which the query matches.
    key: String,
    /// The quiet text after it: an op's summary, "Panel", "Workspace".
    detail: String,
    target: Target,
}

/// What the shell does for a chosen panel or workspace entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Show the panel with this id.
    ShowPanel(&'static str),
    /// Switch to the workspace at this tab index.
    Workspace(usize),
}

/// A panel or workspace the palette can find, as the shell lists them.
#[derive(Clone, Copy, Debug)]
pub struct Place {
    /// The registered id (a panel's) or tab index (a workspace's).
    pub target: PlaceTarget,
    /// The registered English title.
    pub title: &'static str,
}

/// Where a [`Place`] leads.
#[derive(Clone, Copy, Debug)]
pub enum PlaceTarget {
    /// A panel, by id.
    Panel(&'static str),
    /// A workspace, by tab index.
    Workspace(usize),
}

/// The op whose arguments are being filled in.
struct Args {
    op: usize,
    form: Form,
    value: FormValue,
    preview: Preview,
}

/// An edit's live preview: the arguments waiting to be dry-run, and the last dry run's result.
#[derive(Default)]
struct Preview {
    /// Arguments that changed, and when; they are dry-run once they have stayed the same for [`PREVIEW_AFTER`].
    waiting: Option<(Value, f64)>,
    /// The arguments last dry-run, and what the dry run said.
    shown: Option<(Value, Said)>,
}

/// What the last run said.
#[derive(Clone, Debug)]
enum Said {
    /// It worked: a headline, and a query's answer or an edit's summary.
    Done { headline: String, body: Option<String>, changes: usize },
    /// It failed: the op's message and hint.
    Failed { message: String, hint: Option<String> },
}

/// The command palette's state. The shell owns one and draws it over everything else while it is open.
#[derive(Default)]
pub struct Palette {
    open: bool,
    query: String,
    ops: Vec<OpInfo>,
    entries: Vec<Entry>,
    /// The entries that match the query, best first, as indices into `entries`.
    shown: Vec<usize>,
    /// The query `shown` was ranked for.
    ranked_for: Option<String>,
    active: Option<usize>,
    args: Option<Args>,
    said: Option<Said>,
    /// The text field takes the keyboard on the next frame.
    focus_input: bool,
    /// The ops run last, most recent first; kept when the palette closes.
    recents: Vec<String>,
    /// Where the recents are kept between runs of the studio, when anywhere.
    recents_file: Option<PathBuf>,
    /// The frame the palette last did something in. egui can run a frame's pass twice (a discarded pass) with the
    /// same input, and a key or click must act once: the second pass sees this and lets it go.
    acted_in: Option<u64>,
}

impl Palette {
    /// Whether the palette is open.
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Keeps the recents in `file` (a JSON list of op names): reads what is there now, and writes it after each run.
    /// A missing or unreadable file starts the list empty.
    pub fn keep_recents_in(&mut self, file: PathBuf) {
        let names =
            std::fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok());
        self.recents = names.unwrap_or_default().into_iter().take(RECENTS).collect();
        self.recents_file = Some(file);
    }

    /// The ops run last, most recent first.
    pub fn recents(&self) -> &[String] {
        &self.recents
    }

    /// Opens the palette over `runner`'s ops and the shell's `places`, with the query empty.
    pub fn open(&mut self, runner: &dyn Runner, places: &[Place], ctx: &Context) {
        self.ops = runner.ops();
        self.ops.sort_by_key(|op| op.name);
        let mut entries: Vec<Entry> = self
            .ops
            .iter()
            .enumerate()
            .map(|(i, op)| Entry {
                name: op.name.to_owned(),
                key: op.name.to_lowercase(),
                detail: op.summary.to_owned(),
                target: Target::Op(i),
            })
            .collect();
        for place in places {
            let name = title(ctx, place.title).into_owned();
            let (detail, target) = match place.target {
                PlaceTarget::Panel(id) => (say(ctx, Word::PalettePanel, &[]), Target::Panel(id)),
                PlaceTarget::Workspace(n) => (say(ctx, Word::PaletteWorkspace, &[]), Target::Workspace(n)),
            };
            entries.push(Entry { key: name.to_lowercase(), name, detail, target });
        }
        // Found by the query's shape, never by its letters: `rank` puts it first when the query reads like a
        // description, with the query as its detail.
        if let Some(op) = self.ops.iter().position(|info| info.name == DESCRIBE_OP) {
            let name = say(ctx, Word::PaletteDescribe, &[]);
            entries.push(Entry { name, key: String::new(), detail: String::new(), target: Target::Describe(op) });
        }
        *self = Self {
            open: true,
            ops: std::mem::take(&mut self.ops),
            entries,
            focus_input: true,
            recents: std::mem::take(&mut self.recents),
            recents_file: self.recents_file.take(),
            acted_in: self.acted_in,
            ..Self::default()
        };
    }

    /// Opens the palette on `op`'s form, filled with `args` (History's Adjust): the op as it ran, to change and run
    /// again. Opens on the search when no such op is registered.
    pub fn adjust(&mut self, runner: &dyn Runner, places: &[Place], ctx: &Context, op: &str, args: &Value) {
        self.open(runner, places, ctx);
        let Some(index) = self.ops.iter().position(|info| info.name == op) else { return };
        let Some(info) = self.ops.get(index) else { return };
        let form = Form::from_schema(&info.schema);
        let value = form.value_from_json(args);
        self.args = Some(Args { op: index, form, value, preview: Preview::default() });
        self.focus_input = false;
    }

    /// Closes the palette and forgets what was typed (the recents stay).
    pub fn close(&mut self) {
        *self = Self {
            recents: std::mem::take(&mut self.recents),
            recents_file: self.recents_file.take(),
            acted_in: self.acted_in,
            ..Self::default()
        };
    }

    /// Puts `op` first among the recents, and saves them when they have a file.
    fn remember(&mut self, op: &str) {
        self.recents.retain(|name| name != op);
        self.recents.insert(0, op.to_owned());
        self.recents.truncate(RECENTS);
        let Some(file) = &self.recents_file else { return };
        let saved = serde_json::to_string(&self.recents).map_err(std::io::Error::other).and_then(|text| {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(file, text)
        });
        if let Err(error) = saved {
            tracing::warn!(%error, "the palette's recent ops were not saved");
        }
    }

    /// How recently `entry` ran: its place among the recents, or after all of them.
    fn recency(&self, entry: &Entry) -> usize {
        match entry.target {
            Target::Op(_) => self.recents.iter().position(|name| *name == entry.name).unwrap_or(RECENTS),
            Target::Panel(_) | Target::Workspace(_) | Target::Describe(_) => RECENTS,
        }
    }

    /// Draws the palette when it is open, and carries out what is chosen: an op runs through `runner` as `actor`; a
    /// panel or workspace comes back as a [`Command`] for the shell.
    pub fn ui(
        &mut self,
        ctx: &Context,
        runner: &dyn Runner,
        actor: &Actor,
        focus: Option<&EditFocus>,
    ) -> Option<Command> {
        if !self.open {
            return None;
        }
        let frame = ctx.cumulative_frame_nr();
        let acted_before = self.acted_in == Some(frame);
        if acted_before {
            // A pass run again: what its keys asked for is done.
            ctx.input_mut(|i| {
                for (modifiers, key) in ACTION_KEYS {
                    i.consume_key(modifiers, key);
                }
            });
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.acted_in = Some(frame);
            if self.args.take().is_none() {
                self.close();
            }
            self.said = None;
            self.focus_input = true;
            return None;
        }
        self.rank();
        let mut command = None;
        let ds = ds(ctx);
        let width = WIDTH.min(ctx.content_rect().width() - 32.0);
        let label = say(ctx, Word::SearchOrRun, &[]);
        egui::Area::new(Id::new("shell_palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, TOP))
            .show(ctx, |ui| {
                // The palette is modal: Tab and clicks stay in it, never reaching the panels behind.
                ui.ctx().memory_mut(|m| m.set_modal_layer(ui.layer_id()));
                egui::Frame::popup(ui.style()).fill(ds.theme.bg2).show(ui, |ui| {
                    ui.set_width(width);
                    if let Some(args) = self.args.take() {
                        self.args = self.args_ui(ui, args, runner, actor);
                    } else {
                        command = self.search_ui(ui, &label, runner, actor, focus);
                    }
                    self.said_ui(ui);
                });
            });
        if command.is_some() {
            self.close();
        }
        command
    }

    /// Ranks the entries for the query, when it changed.
    fn rank(&mut self) {
        if self.ranked_for.as_deref() == Some(self.query.as_str()) {
            return;
        }
        let (only, text) = match self.query.chars().next() {
            Some('>') => (Some(true), &self.query[1..]),
            Some('/') => (Some(false), &self.query[1..]),
            _ => (None, self.query.as_str()),
        };
        let needle = text.trim().to_lowercase();
        let mut scored: Vec<(u8, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !matches!(e.target, Target::Describe(_)))
            .filter(|(_, e)| only.is_none_or(|ops| matches!(e.target, Target::Op(_)) == ops))
            .filter_map(|(i, e)| score(&e.key, &needle).map(|s| (s, i)))
            .collect();
        scored.sort_by(|(sa, a), (sb, b)| {
            let (ea, eb) = (&self.entries[*a], &self.entries[*b]);
            sa.cmp(sb)
                .then(self.recency(ea).cmp(&self.recency(eb)))
                .then(ea.key.len().cmp(&eb.key.len()))
                .then(ea.key.cmp(&eb.key))
        });
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        let describing = only.is_none() && text.split_whitespace().count() >= DESCRIBE_WORDS;
        if let Some(i) =
            self.entries.iter().position(|e| matches!(e.target, Target::Describe(_))).filter(|_| describing)
        {
            text.trim().clone_into(&mut self.entries[i].detail);
            self.shown.insert(0, i);
        }
        self.active = (!self.shown.is_empty()).then_some(0);
        self.ranked_for = Some(self.query.clone());
    }

    fn search_ui(
        &mut self,
        ui: &mut Ui,
        label: &str,
        runner: &dyn Runner,
        actor: &Actor,
        focus: Option<&EditFocus>,
    ) -> Option<Command> {
        let ctx = ui.ctx().clone();
        // The keys first, before the text field can act on them: the field keeps the keyboard, the list follows.
        if let Some(movement) = listbox::pressed_move(&ctx, false) {
            let rows = self.rows();
            self.active = listbox::apply(&rows, self.active, movement);
        }
        let (enter, dry) = enter_keys(&ctx);
        let (tab, copy) = ctx.input_mut(|i| {
            (i.consume_key(Modifiers::NONE, Key::Tab), i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::C))
        });
        let hint = say(&ctx, Word::PaletteHint, &[]);
        let field = ui.add(TextField::new(label, &mut self.query).hint(&hint).search().id_salt("shell_palette_query"));
        if std::mem::take(&mut self.focus_input) {
            field.request_focus();
        }
        ui.add_space(4.0);
        let mut chosen = None;
        if self.shown.is_empty() {
            let line = say(&ctx, Word::PaletteNothing, &[("query", self.query.trim())]);
            ui.label(RichText::new(line).color(ds(&ctx).theme.text2));
        } else {
            let rows = self.rows();
            let list_label = say(&ctx, Word::PaletteResults, &[]);
            let out = Listbox::new(Id::new("shell_palette_results"), &list_label, &rows).active(self.active).show(ui);
            if let Some(hovered) = out.hovered {
                self.active = Some(hovered);
            }
            chosen = out.clicked.map(|row| (row, Mode::Apply));
        }
        let active_op =
            self.active.and_then(|row| self.shown.get(row)).and_then(|&i| match self.entries.get(i)?.target {
                Target::Op(op) | Target::Describe(op) => Some(op),
                Target::Panel(_) | Target::Workspace(_) => None,
            });
        if let Some(op) = active_op.filter(|_| (tab || copy) && act(&mut self.acted_in, &ctx)) {
            if tab {
                self.open_form(op, focus);
                return None;
            }
            if copy {
                let args = self.ops.get(op).map(|info| prefill(info, focus));
                self.copy(&ctx, op, args.as_ref(), Copy::Mcp);
                return None;
            }
        }
        if enter || dry {
            chosen = self.active.map(|row| (row, if dry { Mode::DryRun } else { Mode::Apply }));
        }
        let (row, mode) = chosen?;
        if !act(&mut self.acted_in, &ctx) {
            return None;
        }
        let entry = self.shown.get(row).and_then(|&i| self.entries.get(i))?;
        match entry.target {
            Target::Panel(id) => Some(Command::ShowPanel(id)),
            Target::Workspace(n) => Some(Command::Workspace(n)),
            Target::Op(op) => {
                self.choose_op(&ctx, op, mode, runner, actor, focus);
                None
            }
            Target::Describe(op) => {
                let words = entry.detail.clone();
                self.describe(&ctx, op, &words, mode, runner, actor, focus);
                None
            }
        }
    }

    /// Makes a character from `words` with the describe op `op`, and, when it is made, opens it in the edit panels.
    #[allow(clippy::too_many_arguments)] // the palette's run context, as `choose_op` takes it, plus the words
    fn describe(
        &mut self,
        ctx: &Context,
        op: usize,
        words: &str,
        mode: Mode,
        runner: &dyn Runner,
        actor: &Actor,
        focus: Option<&EditFocus>,
    ) {
        let Some(info) = self.ops.get(op) else { return };
        let (said, output) =
            run_with_output(ctx, runner, actor, info, serde_json::json!({ "description": words }), mode);
        let character = output.as_ref().and_then(|o| o.get("character")).and_then(Value::as_str);
        if let (Mode::Apply, Some(focus), Some(id)) =
            (mode, focus, character.and_then(|c| omuzaki_char_api::CharId::new(c).ok()))
        {
            focus.open(Some(id));
        }
        if matches!(said, Said::Done { .. }) {
            self.remember(DESCRIBE_OP);
        }
        self.said = Some(said);
    }

    /// Puts the call of op `op` with `args` on the clipboard, as an agent or the command line would make it, and says
    /// what was copied.
    fn copy(&mut self, ctx: &Context, op: usize, args: Option<&Value>, how: Copy) {
        let Some(info) = self.ops.get(op) else { return };
        let empty = Value::Object(serde_json::Map::new());
        let args = args.unwrap_or(&empty);
        let (text, word) = match how {
            Copy::Mcp => (mcp_call(info, args), Word::PaletteCopiedMcp),
            Copy::Cli => (cli_call(info, args), Word::PaletteCopiedCli),
        };
        ctx.copy_text(text.clone());
        self.said = Some(Said::Done { headline: say(ctx, word, &[]), body: Some(text), changes: 0 });
    }

    /// The rows the results list shows, in ranked order.
    fn rows(&self) -> Vec<ListItem<'_>> {
        self.shown
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .map(|e| {
                let item = ListItem::new(&e.name).detail(&e.detail);
                if matches!(e.target, Target::Op(_)) { item.mono().icon(icons::LIGHTNING) } else { item }
            })
            .collect()
    }

    /// An op was chosen: run it if it has nothing to fill in, else open its form.
    fn choose_op(
        &mut self,
        ctx: &Context,
        op: usize,
        mode: Mode,
        runner: &dyn Runner,
        actor: &Actor,
        focus: Option<&EditFocus>,
    ) {
        let Some(info) = self.ops.get(op) else { return };
        let prefilled = prefill(info, focus);
        if required(info).iter().all(|name| prefilled.get(name.as_str()).is_some()) && optional_free(info) {
            let said = run(ctx, runner, actor, info, prefilled, mode);
            if matches!(said, Said::Done { .. }) {
                let name = info.name;
                self.remember(name);
            }
            self.said = Some(said);
            return;
        }
        self.open_form(op, focus);
    }

    /// Opens op `op`'s form, filled in from the selection.
    fn open_form(&mut self, op: usize, focus: Option<&EditFocus>) {
        let Some(info) = self.ops.get(op) else { return };
        let form = Form::from_schema(&info.schema);
        let value = form.value_from_json(&prefill(info, focus));
        self.args = Some(Args { op, form, value, preview: Preview::default() });
        self.said = None;
    }

    fn args_ui(&mut self, ui: &mut Ui, mut args: Args, runner: &dyn Runner, actor: &Actor) -> Option<Args> {
        let ctx = ui.ctx().clone();
        let (enter, dry_key) = enter_keys(&ctx);
        let copy_key = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::C));
        let info = self.ops.get(args.op)?;
        let ds = ds(&ctx);
        ui.label(RichText::new(info.name).font(ds.text.mono.clone()));
        ui.label(RichText::new(info.summary).color(ds.theme.text2));
        ui.add_space(4.0);
        ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            SchemaForm::new(&args.form, &mut args.value).id_salt(("shell_palette_args", info.name)).show(ui);
        });
        ui.add_space(4.0);
        let blocked = args.form.invalid_count(&args.value) > 0;
        if info.kind == OpKind::Edit && !blocked {
            preview(&ctx, &mut args.preview, runner, actor, info, Value::from(&args.value));
        }
        if let Some((_, said)) = &args.preview.shown {
            preview_ui(ui, said);
        }
        let tool = tool_name(info.name);
        let scope = info.scope.to_string();
        let same = say(&ctx, Word::PaletteSameAs, &[("tool", &tool), ("scope", &scope)]);
        ui.label(RichText::new(same).color(ds.theme.text2).small());
        let mut mode = None;
        let mut copy = copy_key.then_some(Copy::Mcp);
        ui.horizontal_wrapped(|ui| {
            let run_label = say(&ctx, Word::PaletteRun, &[]);
            let dry_label = say(&ctx, Word::PaletteDryRun, &[]);
            let mcp_label = say(&ctx, Word::PaletteCopyMcp, &[]);
            let cli_label = say(&ctx, Word::PaletteCopyCli, &[]);
            let fix = say(&ctx, Word::PaletteFixFields, &[]);
            let mut run_button = Button::new(&run_label).primary().shortcut_hint("Enter");
            let mut dry_button = Button::new(&dry_label).shortcut_hint("Shift Enter");
            if blocked {
                run_button = run_button.disabled(&fix);
                dry_button = dry_button.disabled(&fix);
            }
            if ui.add(run_button).clicked() {
                mode = Some(Mode::Apply);
            }
            if info.kind != OpKind::Query && ui.add(dry_button).clicked() {
                mode = Some(Mode::DryRun);
            }
            if ui.add(Button::new(&mcp_label).ghost().shortcut_hint("Ctrl Shift C")).clicked() {
                copy = Some(Copy::Mcp);
            }
            if ui.add(Button::new(&cli_label).ghost()).clicked() {
                copy = Some(Copy::Cli);
            }
        });
        if !blocked && (enter || dry_key) {
            mode = Some(if dry_key { Mode::DryRun } else { Mode::Apply });
        }
        if (copy.is_some() || mode.is_some()) && !act(&mut self.acted_in, &ctx) {
            return Some(args);
        }
        if let Some(how) = copy {
            let value = Value::from(&args.value);
            self.copy(&ctx, args.op, Some(&value), how);
            return Some(args);
        }
        if let Some(mode) = mode {
            let said = run(&ctx, runner, actor, info, Value::from(&args.value), mode);
            let worked = matches!(said, Said::Done { .. });
            // An edit that worked closes the form; a query, a dry run or a failure keeps it to adjust and run again.
            let keep = mode == Mode::DryRun || info.kind == OpKind::Query || !worked;
            if worked && mode == Mode::Apply {
                let name = info.name;
                self.remember(name);
            }
            self.said = Some(said);
            return keep.then_some(args);
        }
        Some(args)
    }

    fn said_ui(&self, ui: &mut Ui) {
        let Some(said) = &self.said else { return };
        let ds = ds(ui.ctx());
        ui.separator();
        match said {
            Said::Done { headline, body, .. } => {
                ui.label(RichText::new(headline).color(ds.theme.text));
                if let Some(body) = body {
                    ScrollArea::vertical().id_salt("shell_palette_answer").max_height(200.0).show(ui, |ui| {
                        ui.label(RichText::new(body).font(ds.text.mono.clone()).color(ds.theme.text2));
                    });
                }
            }
            Said::Failed { message, hint } => {
                ui.label(RichText::new(message).color(ds.theme.danger));
                if let Some(hint) = hint {
                    ui.label(RichText::new(hint).color(ds.theme.text2));
                }
            }
        }
    }
}

/// Whether the palette may act this frame (a pass run again may not), noting in `acted_in` that it does.
fn act(acted_in: &mut Option<u64>, ctx: &Context) -> bool {
    let frame = ctx.cumulative_frame_nr();
    let first = *acted_in != Some(frame);
    *acted_in = Some(frame);
    first
}

/// The keys the palette acts on, which a pass run again must not act on twice.
const ACTION_KEYS: [(Modifiers, Key); 5] = [
    (Modifiers::NONE, Key::Enter),
    (Modifiers::SHIFT, Key::Enter),
    (Modifiers::NONE, Key::Tab),
    (Modifiers::NONE, Key::Escape),
    (Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::C),
];

/// Whether Enter (run) or Shift+Enter (dry run) was pressed this frame, consumed so nothing else acts on it.
fn enter_keys(ctx: &Context) -> (bool, bool) {
    ctx.input_mut(|i| {
        let dry = i.consume_key(Modifiers::SHIFT, Key::Enter);
        (i.consume_key(Modifiers::NONE, Key::Enter), dry)
    })
}

/// How well `key` matches `needle`, lower is better; `None` when it does not.
fn score(key: &str, needle: &str) -> Option<u8> {
    if needle.is_empty() {
        return Some(4);
    }
    if key == needle {
        return Some(0);
    }
    if key.starts_with(needle) {
        return Some(1);
    }
    let word_start = key
        .match_indices(needle)
        .any(|(at, _)| key[..at].chars().next_back().is_some_and(|c| matches!(c, '.' | '_' | ' ' | '-')));
    if word_start {
        return Some(2);
    }
    if key.contains(needle) {
        return Some(3);
    }
    let mut letters = key.chars();
    needle.chars().filter(|c| !c.is_whitespace()).all(|c| letters.any(|k| k == c)).then_some(5)
}

/// The names of the op's required arguments.
fn required(info: &OpInfo) -> Vec<String> {
    info.schema
        .as_value()
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Whether the op has no arguments besides the required ones (so it can run without its form).
fn optional_free(info: &OpInfo) -> bool {
    let properties = info.schema.as_value().get("properties").and_then(Value::as_object);
    let required = required(info);
    properties.is_none_or(|props| props.keys().all(|k| required.contains(k)))
}

/// The op's arguments as far as the selection says them: `character` and `part` when the op takes them.
fn prefill(info: &OpInfo, focus: Option<&EditFocus>) -> Value {
    let mut args = serde_json::Map::new();
    let takes = |name: &str| info.schema.as_value().get("properties").and_then(|p| p.get(name)).is_some();
    if let Some(focus) = focus {
        if let Some(character) = focus.character().filter(|_| takes("character")) {
            args.insert("character".into(), Value::String(character.as_str().to_owned()));
        }
        if let Some(part) = focus.primary().filter(|_| takes("part")) {
            args.insert("part".into(), Value::String(part.as_str().to_owned()));
        }
    }
    Value::Object(args)
}

/// Dry-runs the form's arguments once they have stopped changing for [`PREVIEW_AFTER`], and asks for the frame that
/// will.
fn preview(ctx: &Context, preview: &mut Preview, runner: &dyn Runner, actor: &Actor, info: &OpInfo, args: Value) {
    let now = ctx.input(|i| i.time);
    let shown = preview.shown.as_ref().is_some_and(|(shown, _)| *shown == args);
    let waiting = preview.waiting.as_ref().is_some_and(|(waiting, _)| *waiting == args);
    if !shown && !waiting {
        preview.waiting = Some((args, now));
    }
    let Some((_, since)) = &preview.waiting else { return };
    let left = PREVIEW_AFTER - (now - since);
    if left > 0.0 {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        return;
    }
    if let Some((args, _)) = preview.waiting.take() {
        let said = run(ctx, runner, actor, info, args.clone(), Mode::DryRun);
        preview.shown = Some((args, said));
    }
}

/// The live preview's line: a chip, and what the dry run would change or why it would fail.
fn preview_ui(ui: &mut Ui, said: &Said) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let chip = say(&ctx, Word::PalettePreview, &[]);
    ui.horizontal_wrapped(|ui| {
        ui.add(StatusChip::new(Status::Info, &chip));
        match said {
            Said::Done { changes, .. } => {
                let count = changes.to_string();
                let line = say(&ctx, Word::PalettePreviewChanges, &[("count", &count)]);
                ui.label(RichText::new(line).color(ds.theme.text));
            }
            Said::Failed { message, .. } => {
                ui.label(RichText::new(message).color(ds.theme.danger));
            }
        }
    });
}

/// Which call "Copy as" copies.
#[derive(Clone, Copy, Debug)]
enum Copy {
    /// The MCP `tools/call` params an agent sends.
    Mcp,
    /// An `omuzaki run` command line.
    Cli,
}

/// The MCP call for `info` with `args`: the `tools/call` params an agent sends for the same run.
fn mcp_call(info: &OpInfo, args: &Value) -> String {
    let call = serde_json::json!({"name": info.tool_name(), "arguments": args});
    serde_json::to_string_pretty(&call).unwrap_or_else(|_| call.to_string())
}

/// The `omuzaki run` command for `info` with `args`, quoted for a POSIX shell.
fn cli_call(info: &OpInfo, args: &Value) -> String {
    let json = args.to_string().replace('\'', "'\\''");
    format!("omuzaki run {} --args '{json}'", info.name)
}

/// Runs `info` with `args` as `actor`, and says how it went.
fn run(ctx: &Context, runner: &dyn Runner, actor: &Actor, info: &OpInfo, args: Value, mode: Mode) -> Said {
    run_with_output(ctx, runner, actor, info, args, mode).0
}

/// [`run`], with the op's output as well.
fn run_with_output(
    ctx: &Context,
    runner: &dyn Runner,
    actor: &Actor,
    info: &OpInfo,
    args: Value,
    mode: Mode,
) -> (Said, Option<Value>) {
    let request = Request {
        op: info.name.to_owned(),
        args,
        mode: if info.kind == OpKind::Query { Mode::Apply } else { mode },
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    match runner.run(request) {
        Ok(outcome) => {
            let changes = outcome.plan.patch.changes.len();
            let count = changes.to_string();
            let named = [("op", info.name), ("count", count.as_str())];
            let headline = match (info.kind, mode) {
                (OpKind::Query, _) => info.name.to_owned(),
                (_, Mode::DryRun) => say(ctx, Word::PaletteWouldChange, &named),
                (_, Mode::Apply) => say(ctx, Word::PaletteDone, &named),
            };
            let body = match outcome.plan.output.as_ref() {
                Some(output) => {
                    let text = serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string());
                    let lines: Vec<&str> = text.lines().collect();
                    Some(if lines.len() > ANSWER_LINES {
                        let hidden = (lines.len() - ANSWER_LINES).to_string();
                        format!(
                            "{}\n{}",
                            lines[..ANSWER_LINES].join("\n"),
                            say(ctx, Word::PaletteMoreLines, &[("count", &hidden)])
                        )
                    } else {
                        text
                    })
                }
                None => None,
            };
            (Said::Done { headline, body, changes }, outcome.plan.output)
        }
        Err(error) => (Said::Failed { message: error.message, hint: error.hint }, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_beats_prefix_beats_word_beats_substring_beats_fuzzy() {
        assert_eq!(score("part.set_param", "part.set_param"), Some(0));
        assert_eq!(score("part.set_param", "part"), Some(1));
        assert_eq!(score("part.set_param", "set"), Some(2));
        assert_eq!(score("part.set_param", "aram"), Some(3));
        assert_eq!(score("part.set_param", "pspm"), Some(5));
        assert_eq!(score("part.set_param", "zz"), None);
        assert_eq!(score("anything", ""), Some(4));
    }
}
