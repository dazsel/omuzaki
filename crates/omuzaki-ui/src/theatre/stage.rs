//! The Theatre's stage (`shell.md` section 11 items 2 and 6): the controls strip over the live viewport.
//!
//! - **Controls** (TH-06, TH-10): the agent the strip acts on; Pause, Step (one step, then paused again) and Resume;
//!   the pause mode (after each step, after each plan item, at gates only, never) through `theatre.pause_mode`; the
//!   budget meter (money, tokens, time against their limits) with its Limits, which the session pauses at; Stop.
//! - **Live viewport** (TH-01): the character as it changes, a caption naming the newest step, a soft highlight on the
//!   parts it changed, and, while "Following" is on, the camera framing them as each step lands. Pop out moves the
//!   stage into a window of its own for a second monitor; closing that window brings it back.
//! - **Draw to direct** (TH-09): with it on, a drag over the character circles a region; each region gets a note
//!   field, and Send hands every note to the agents through `theatre.direct`, with the region, the part under it, the
//!   camera it was drawn from and the step whose picture shows what the person saw.

use std::sync::Arc;

use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, vec2};
use omuzaki_char_api::PartId;
use omuzaki_ops_api::{Budget, SessionInfo, Sessions};
use omuzaki_theatre::book::agent_id;
use omuzaki_theatre::{Book, PauseMode, Step};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::json;

use super::{Theatre, no_book, paused, problem, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::Status as Tone;
use crate::ds::widgets::{Button, Choice, Dropdown, NumberField, StatusChip, TextField, Toggle};
use crate::panels::agent::approvals::dollars;
use crate::panels::agent::{AgentFeed, Agents};
use crate::panels::edit::EditFocus;
use crate::panels::io::Io;
use crate::panels::text::say;
use crate::panels::viewport::camera::{Bounds, OrbitCamera};
use crate::panels::viewport::{Viewport, ViewportServices};
use crate::shell::history::{Preview, who};

/// The smallest region a drag leaves, as a share of the viewport: a click is not a region.
const MIN_REGION: f32 = 0.01;

/// A note being drawn, before Send.
#[derive(Clone, Debug)]
struct Draft {
    /// The region, as fractions of the viewport from its top left: x, y, width, height.
    region: [f32; 4],
    /// The part under its middle.
    part: Option<PartId>,
    /// The camera it was drawn under.
    camera: OrbitCamera,
    /// What should change there.
    text: String,
}

/// The money, token and time limits as the Limits popover edits them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Limits {
    dollars: f64,
    tokens: f64,
    minutes: f64,
}

impl Limits {
    #[allow(clippy::cast_precision_loss, reason = "a budget in micro-dollars, tokens or seconds, far below 2^52")]
    fn of(budget: Budget) -> Self {
        Self {
            dollars: budget.usd_micros as f64 / 1e6,
            tokens: budget.tokens.unwrap_or(0) as f64,
            minutes: budget.seconds.unwrap_or(0) as f64 / 60.0,
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "fields clamped to their ranges")]
    fn budget(self) -> Budget {
        let some = |v: f64| (v >= 1.0).then_some(v.round() as u64);
        Budget {
            usd_micros: (self.dollars.max(0.0) * 1e6).round() as u64,
            tokens: some(self.tokens),
            seconds: some(self.minutes * 60.0),
        }
    }
}

/// The stage.
pub struct StagePanel {
    focus: EditFocus,
    viewport: Option<Viewport>,
    /// The newest step the viewport framed, so it frames each step once.
    followed: Option<omuzaki_ops_api::TxId>,
    drafts: Vec<Draft>,
    /// Where the drag that draws a region started.
    drawing: Option<Pos2>,
    /// The Limits popover is open, with its fields.
    limits: Option<Limits>,
    seen: Option<(u64, Book)>,
    problem: Option<String>,
    note: Option<String>,
}

impl std::fmt::Debug for StagePanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagePanel").field("drafts", &self.drafts.len()).finish_non_exhaustive()
    }
}

/// What the stage reads besides the book: the shell's views, each optional.
#[derive(Clone, Copy)]
pub struct Views<'a> {
    /// The agent sessions, for the controls and the budget.
    pub sessions: Option<&'a Arc<dyn Sessions>>,
    /// The shell's copy of the sessions.
    pub feed: Option<&'a AgentFeed>,
    /// The device and builder the viewport draws with.
    pub services: Option<&'a ViewportServices>,
    /// History's past state, which the viewport shows while it is set (a rewind).
    pub preview: Option<&'a Preview>,
}

impl<'a> Views<'a> {
    /// The views from a panel's context.
    pub fn from_ctx(ctx: &PanelCtx<'a>) -> Self {
        Self {
            sessions: ctx.view::<Agents>().map(|a| &a.0),
            feed: ctx.view::<AgentFeed>(),
            services: ctx.view::<ViewportServices>(),
            preview: ctx.view::<Preview>(),
        }
    }
}

impl StagePanel {
    /// A stage over the shell's edit focus (the open character).
    pub fn new(focus: EditFocus) -> Self {
        Self {
            focus,
            viewport: None,
            followed: None,
            drafts: Vec::new(),
            drawing: None,
            limits: None,
            seen: None,
            problem: None,
            note: None,
        }
    }

    /// Where the viewport drew the character on the last frame.
    pub fn viewport_rect(&self) -> Option<Rect> {
        self.viewport.as_ref().and_then(Viewport::rect)
    }

    /// Draws the controls strip and the viewport, in the panel (or in its own window when popped out).
    pub fn show(&mut self, ui: &mut Ui, theatre: &Theatre, io: Io<'_>, views: Views<'_>) {
        let ds = ds(ui.ctx());
        let revision = theatre.book.revision();
        if self.seen.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            self.seen = Some((revision, theatre.book.snapshot()));
        }
        let Some((_, book)) = self.seen.take() else { return };
        let session = views.feed.and_then(|feed| theatre.agent(feed, &book)).cloned();
        self.controls(ui, &ds, theatre, io, views, &book, session.as_ref());
        problem(ui, &ds, self.problem.as_deref());
        if let Some(note) = &self.note {
            ui.label(RichText::new(note).color(ds.theme.text2));
        }
        self.viewport_ui(ui, &ds, theatre, io, views, &book, session.as_ref());
        self.seen = Some((revision, book));
    }

    /// The controls strip.
    #[allow(clippy::too_many_arguments, reason = "the strip: what it shows and what its controls reach")]
    fn controls(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        theatre: &Theatre,
        io: Io<'_>,
        views: Views<'_>,
        book: &Book,
        session: Option<&SessionInfo>,
    ) {
        ui.horizontal_wrapped(|ui| {
            if let Some(feed) = views.feed {
                Self::agent_picker(ui, ds, theatre, feed, session);
            }
            if let (Some(session), Some(sessions)) = (session, views.sessions) {
                self.session_controls(ui, ds, theatre, io, sessions.as_ref(), book, session);
            }
            let mut pen = theatre.looking().pen;
            if ui.add(Toggle::switch(&say(ds, text::DRAW, &[]), &mut pen).id_salt("theatre-pen")).changed() {
                theatre.look(|l| l.pen = pen);
                self.drawing = None;
            }
            let open = book.notes.iter().filter(|n| !n.taken).count();
            if open > 0 {
                let line = say(ds, text::OPEN_NOTES, &[("count", &open.to_string())]);
                ui.label(RichText::new(line).color(ds.theme.text2));
            }
            let popped = theatre.looking().popped;
            if !popped
                && ui.add(Button::new(&say(ds, text::POP_OUT, &[])).ghost().icon(icons::ARROW_SQUARE_OUT)).clicked()
            {
                theatre.look(|l| l.popped = true);
            }
        });
        if let (Some(session), Some(sessions)) = (session, views.sessions) {
            self.budget(ui, ds, sessions.as_ref(), session, io);
        }
    }

    fn agent_picker(ui: &mut Ui, ds: &Ds, theatre: &Theatre, feed: &AgentFeed, session: Option<&SessionInfo>) {
        let live: Vec<&SessionInfo> = feed.live().collect();
        if live.len() < 2 {
            if let Some(session) = session {
                ui.label(RichText::new(who(ui.ctx(), &session.actor)).font(ds.text.strong.clone()));
            }
            return;
        }
        let names: Vec<String> = live.iter().map(|s| who(ui.ctx(), &s.actor)).collect();
        let choices: Vec<Choice<'_, usize>> = names.iter().enumerate().map(|(i, n)| Choice::new(i, n)).collect();
        let mut at = session.and_then(|s| live.iter().position(|l| l.id == s.id)).unwrap_or(0);
        let label = say(ds, text::AGENT, &[]);
        if ui.add(Dropdown::new(&label, &mut at, &choices).id_salt("theatre-agent")).changed() {
            let actor = live.get(at).map(|s| s.actor.clone());
            theatre.look(|l| l.agent = actor);
        }
    }

    /// Pause or Resume, Step, the pause mode and Stop for `session`.
    #[allow(clippy::too_many_arguments, reason = "one session's controls and what they reach")]
    fn session_controls(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        theatre: &Theatre,
        io: Io<'_>,
        sessions: &dyn Sessions,
        book: &Book,
        session: &SessionInfo,
    ) {
        let person = io.actor;
        let (tone, word) = if paused(session) { (Tone::Warn, text::PAUSED) } else { (Tone::Ok, text::WORKING) };
        ui.add(StatusChip::new(tone, &say(ds, word, &[])));
        if paused(session) {
            if ui.add(Button::new(&say(ds, text::STEP, &[])).icon(icons::SKIP_FORWARD)).clicked() {
                theatre.book.pause_after_next(&session.actor);
                self.problem = sessions.resume(session.id, person).err().map(|e| e.message);
            }
            if ui.add(Button::new(&say(ds, text::RESUME, &[])).primary().icon(icons::PLAY)).clicked() {
                self.problem = sessions.resume(session.id, person).err().map(|e| e.message);
            }
        } else if ui.add(Button::new(&say(ds, text::PAUSE, &[])).icon(icons::PAUSE)).clicked() {
            self.problem = sessions.pause(session.id, person).err().map(|e| e.message);
        }
        let modes = [
            (PauseMode::EveryStep, text::MODE_EVERY_STEP),
            (PauseMode::EachItem, text::MODE_EACH_ITEM),
            (PauseMode::Gates, text::MODE_GATES),
            (PauseMode::Off, text::MODE_OFF),
        ];
        let words: Vec<String> = modes.iter().map(|(_, w)| say(ds, w, &[]).into_owned()).collect();
        let choices: Vec<Choice<'_, PauseMode>> =
            modes.iter().zip(&words).map(|((mode, _), word)| Choice::new(*mode, word)).collect();
        let mut mode = book.mode(&session.actor);
        let label = say(ds, text::PAUSE_MODE, &[]);
        if ui.add(Dropdown::new(&label, &mut mode, &choices).id_salt("theatre-mode")).changed()
            && let Some(agent) = agent_id(&session.actor)
        {
            let args = json!({ "agent": agent, "mode": mode });
            self.problem = io.run("theatre.pause_mode", args, None).err().map(|e| e.message);
        }
        if ui.add(Button::new(&say(ds, text::STOP, &[])).danger().icon(icons::STOP)).clicked() {
            let stopped = match sessions.stop(session.id, person) {
                Err(e) if e.code == omuzaki_ops_api::ErrorCode::Unsupported => {
                    sessions.pause(session.id, person).map(drop)
                }
                other => other.map(drop),
            };
            self.problem = stopped.err().map(|e| e.message);
        }
    }

    /// The budget meter (TH-10): what the session spent against its limits, and the Limits that pause it.
    fn budget(&mut self, ui: &mut Ui, ds: &Ds, sessions: &dyn Sessions, session: &SessionInfo, io: Io<'_>) {
        let (budget, spent) = (session.budget, session.spent);
        let mut parts = Vec::new();
        if budget.usd_micros > 0 || spent.usd_micros > 0 {
            let (spent, cap) = (dollars(spent.usd_micros), dollars(budget.usd_micros));
            parts.push(say(ds, text::SPENT_MONEY, &[("spent", &spent), ("cap", &cap)]).into_owned());
        }
        let tokens = spent.tokens.to_string();
        parts.push(
            match budget.tokens {
                Some(cap) => say(ds, text::SPENT_TOKENS, &[("spent", &tokens), ("cap", &cap.to_string())]),
                None => say(ds, text::SPENT_TOKENS_FREE, &[("spent", &tokens)]),
            }
            .into_owned(),
        );
        let time = clock(spent.seconds);
        parts.push(
            match budget.seconds {
                Some(cap) => say(ds, text::SPENT_TIME, &[("spent", &time), ("cap", &clock(cap))]),
                None => say(ds, text::SPENT_TIME_FREE, &[("spent", &time)]),
            }
            .into_owned(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(parts.join(" \u{b7} ")).color(ds.theme.text));
            let near = budget.tokens.is_some_and(|cap| spent.tokens * 10 >= cap * 9)
                || budget.seconds.is_some_and(|cap| spent.seconds * 10 >= cap * 9)
                || (budget.usd_micros > 0 && spent.usd_micros * 10 >= budget.usd_micros * 9);
            if near {
                ui.label(RichText::new(icons::WARNING).font(ds.text.icon(14.0, false)).color(ds.theme.warn));
            }
            if ui.add(Button::new(&say(ds, text::LIMITS, &[])).ghost().icon(icons::SLIDERS_HORIZONTAL)).clicked() {
                self.limits = if self.limits.is_some() { None } else { Some(Limits::of(budget)) };
            }
        });
        let Some(mut limits) = self.limits else { return };
        ui.label(RichText::new(say(ds, text::LIMITS_HOW, &[])).font(ds.text.small.clone()).color(ds.theme.text2));
        let mut set = false;
        ui.horizontal_wrapped(|ui| {
            NumberField::new(&say(ds, text::LIMIT_MONEY, &[]), &mut limits.dollars, 0.0..=1000.0)
                .id_salt("theatre-limit-money")
                .show(ui);
            NumberField::new(&say(ds, text::LIMIT_TOKENS, &[]), &mut limits.tokens, 0.0..=1.0e9)
                .id_salt("theatre-limit-tokens")
                .show(ui);
            NumberField::new(&say(ds, text::LIMIT_MINUTES, &[]), &mut limits.minutes, 0.0..=1440.0)
                .id_salt("theatre-limit-minutes")
                .show(ui);
            set = ui.add(Button::new(&say(ds, text::LIMITS_SAVE, &[])).primary()).clicked();
        });
        self.limits = Some(limits);
        if set {
            self.problem = sessions.set_budget(session.id, limits.budget(), io.actor).err().map(|e| e.message);
            if self.problem.is_none() {
                self.limits = None;
            }
        }
    }

    /// The viewport with its caption, highlight, follow mode and drawn notes.
    #[allow(clippy::too_many_arguments, reason = "the viewport: what it shows and what its notes reach")]
    fn viewport_ui(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        theatre: &Theatre,
        io: Io<'_>,
        views: Views<'_>,
        book: &Book,
        session: Option<&SessionInfo>,
    ) {
        if self.viewport.is_none() {
            self.viewport = views.services.map(|s| Viewport::new(self.focus.clone(), s.clone()));
        }
        let newest = book.steps.last();
        let looking = theatre.looking();
        if let Some(session) = session {
            let agent = who(ui.ctx(), &session.actor);
            let word = if looking.follow { text::FOLLOW } else { text::FOLLOW_OFF };
            let mut follow = looking.follow;
            let label = say(ds, word, &[("agent", &agent)]);
            if ui.add(Toggle::switch(&label, &mut follow).id_salt("theatre-follow")).changed() {
                theatre.look(|l| l.follow = follow);
            }
        }
        // The notes' fields take what the viewport leaves; the viewport takes the rest.
        let rows = f32::from(u8::try_from(self.drafts.len()).unwrap_or(u8::MAX));
        let fields = if self.drafts.is_empty() { 0.0 } else { 40.0 + 36.0 * rows };
        let Some(viewport) = self.viewport.as_mut() else { return };
        viewport.set_preview(views.preview.cloned());
        let height = (ui.available_height() - fields).max(160.0);
        let mut area = ui.available_rect_before_wrap();
        area.set_height(height);
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| viewport.ui(ui, io));
        ui.advance_cursor_after_rect(area);
        let Some(rect) = viewport.rect() else { return };
        let followed_step = newest.filter(|s| session.is_some_and(|session| session.actor == s.actor));
        if looking.follow
            && let Some(step) = followed_step
            && self.followed != Some(step.tx)
        {
            self.followed = Some(step.tx);
            if let Some(bounds) = follow_bounds(viewport, step) {
                viewport.frame(bounds);
            }
        }
        if let Some(step) = newest {
            highlight(ui, ds, viewport, rect, step);
            caption(ui, ds, rect, step);
        }
        if looking.pen {
            self.draw(ui, ds, rect);
        }
        self.drafts_ui(ui, ds, rect, io, newest);
    }

    /// With Draw to direct on: a drag over the viewport circles a region.
    fn draw(&mut self, ui: &mut Ui, ds: &Ds, rect: Rect) {
        let response = ui.interact(rect, ui.id().with("theatre-draw"), Sense::drag());
        let t = &ds.theme;
        if response.drag_started() {
            self.drawing = response.interact_pointer_pos();
        }
        let now = ui.input(|i| i.pointer.interact_pos());
        if let (Some(from), Some(to)) = (self.drawing, now) {
            let region = Rect::from_two_pos(from, to).intersect(rect);
            ui.painter().rect_stroke(region, 255, Stroke::new(2.0, t.danger), StrokeKind::Middle);
            if response.drag_stopped() {
                self.drawing = None;
                let share = |v: f32, lo: f32, size: f32| ((v - lo) / size.max(1.0)).clamp(0.0, 1.0);
                let shared = [
                    share(region.left(), rect.left(), rect.width()),
                    share(region.top(), rect.top(), rect.height()),
                    region.width() / rect.width().max(1.0),
                    region.height() / rect.height().max(1.0),
                ];
                if shared[2] >= MIN_REGION && shared[3] >= MIN_REGION {
                    let part = self.viewport.as_ref().and_then(|v| part_under(v, rect, region.center()));
                    let camera = self.viewport.as_ref().map(Viewport::camera).unwrap_or_default();
                    self.drafts.push(Draft { region: shared, part, camera, text: String::new() });
                }
            }
        }
        if self.drafts.is_empty() && self.drawing.is_none() {
            let how = say(ds, text::DRAW_HOW, &[]);
            let at = rect.center_top() + vec2(0.0, 36.0);
            ui.painter().text(at, Align2::CENTER_TOP, how, ds.text.body.clone(), t.text);
        }
    }

    /// The drawn regions over the viewport, each numbered, and their note fields with Send.
    fn drafts_ui(&mut self, ui: &mut Ui, ds: &Ds, rect: Rect, io: Io<'_>, newest: Option<&Step>) {
        if self.drafts.is_empty() {
            return;
        }
        let theme = &ds.theme;
        for (n, draft) in self.drafts.iter().enumerate() {
            let [left, top, width, height] = draft.region;
            let region = Rect::from_min_size(
                rect.min + vec2(left * rect.width(), top * rect.height()),
                vec2(width * rect.width(), height * rect.height()),
            );
            let painter = ui.painter();
            painter.rect_filled(region, 255, theme.danger.gamma_multiply(0.12));
            painter.rect_stroke(region, 255, Stroke::new(2.0, theme.danger), StrokeKind::Middle);
            let pill = Rect::from_center_size(region.right_top(), vec2(20.0, 20.0));
            painter.rect_filled(pill, 255, theme.danger);
            let number = (n + 1).to_string();
            painter.text(pill.center(), Align2::CENTER_CENTER, number, ds.text.small.clone(), theme.on_accent);
        }
        let label = say(ds, text::NOTE_TEXT, &[]);
        let mut remove = None;
        for (n, draft) in self.drafts.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new((n + 1).to_string()).font(ds.text.strong.clone()).color(theme.danger));
                ui.add(TextField::new(&label, &mut draft.text).width(360.0).id_salt(("theatre-note", n)));
                let discard = say(ds, text::DISCARD_NOTES, &[]);
                if ui.add(Button::new(&discard).ghost().icon(icons::X).accessible_label(&discard)).clicked() {
                    remove = Some(n);
                }
            });
        }
        if let Some(n) = remove {
            self.drafts.remove(n);
        }
        let ready: Vec<&Draft> = self.drafts.iter().filter(|d| !d.text.trim().is_empty()).collect();
        let send_label = match ready.len() {
            1 => say(ds, text::SEND_NOTE, &[]),
            n => say(ds, text::SEND_NOTES, &[("count", &n.to_string())]),
        };
        let button = Button::new(&send_label).primary().icon(icons::PEN).disabled_if(ready.is_empty(), &label);
        if ui.add(button).clicked() {
            let picture = newest.map(|s| s.tx);
            let mut sent = 0;
            for draft in &ready {
                let [left, top, width, height] = draft.region;
                let camera = draft.camera;
                let target = camera.target;
                let args = json!({
                    "text": draft.text.trim(),
                    "region": { "x": left, "y": top, "w": width, "h": height },
                    "part": draft.part.as_ref().map(PartId::as_str),
                    "camera": { "yaw": camera.yaw, "pitch": camera.pitch, "distance": camera.distance,
                                "target": [target.x, target.y, target.z] },
                    "picture": picture,
                });
                match io.run("theatre.direct", args, None) {
                    Ok(_) => sent += 1,
                    Err(e) => self.problem = Some(e.message),
                }
            }
            if sent > 0 {
                self.drafts.retain(|d| d.text.trim().is_empty());
                self.note = Some(say(ds, text::SENT, &[("count", &sent.to_string())]).into_owned());
            }
        }
    }
}

/// `seconds` as minutes and seconds ("3:12").
fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The boxes of the parts `step` changed, together.
fn parts_bounds(viewport: &Viewport, step: &Step) -> Option<Bounds> {
    step.parts
        .iter()
        .filter_map(|p| PartId::new(p.clone()).ok())
        .filter_map(|p| viewport.part_bounds(&p))
        .reduce(Bounds::union)
}

/// What following `step` frames: the parts it changed, or the whole character when they are most of it (a body
/// change framed on the body alone would cut off the head).
fn follow_bounds(viewport: &Viewport, step: &Step) -> Option<Bounds> {
    let changed = parts_bounds(viewport, step)?;
    let whole = viewport.parts().map(|(_, b)| b).reduce(Bounds::union).unwrap_or(changed);
    Some(if changed.radius() > whole.radius() * MOST_OF_IT { whole } else { changed })
}

/// The share of the character's size above which a change is framed on the whole character.
const MOST_OF_IT: f32 = 0.5;

/// A quiet outline round the parts the newest step changed: corner marks, so the character under it stays in view.
fn highlight(ui: &Ui, ds: &Ds, viewport: &Viewport, rect: Rect, step: &Step) {
    let Some(bounds) = parts_bounds(viewport, step) else { return };
    let Some([l, t, r, b]) = viewport.camera().project_bounds(bounds, [rect.width(), rect.height()]) else { return };
    let marked = Rect::from_min_max(rect.min + vec2(l, t), rect.min + vec2(r, b)).expand(6.0).intersect(rect);
    let arm = (marked.width().min(marked.height()) * 0.2).clamp(6.0, 24.0);
    let stroke = Stroke::new(2.0, ds.theme.agent);
    let painter = ui.painter();
    for (corner, dx, dy) in [
        (marked.left_top(), arm, arm),
        (marked.right_top(), -arm, arm),
        (marked.left_bottom(), arm, -arm),
        (marked.right_bottom(), -arm, -arm),
    ] {
        painter.line_segment([corner, corner + vec2(dx, 0.0)], stroke);
        painter.line_segment([corner, corner + vec2(0.0, dy)], stroke);
    }
}

/// The caption pill naming the newest step, at the top of the viewport.
fn caption(ui: &Ui, ds: &Ds, rect: Rect, step: &Step) {
    let agent = who(ui.ctx(), &step.actor);
    let line = say(ds, text::CAPTION, &[("agent", &agent), ("label", &step.label)]);
    let galley = ui.painter().layout_no_wrap(line.into_owned(), ds.text.body.clone(), ds.theme.text);
    let size = galley.size() + vec2(20.0, 10.0);
    let pill = Rect::from_center_size(rect.center_top() + vec2(0.0, 12.0 + size.y / 2.0), size);
    let painter = ui.painter();
    painter.rect_filled(pill, 255, ds.theme.bg2);
    painter.rect_stroke(pill, 255, Stroke::new(1.0, ds.theme.agent), StrokeKind::Inside);
    painter.galley(pill.min + vec2(10.0, 5.0), galley, ds.theme.text);
}

/// The smallest part whose screen box holds `at`.
fn part_under(viewport: &Viewport, rect: Rect, at: Pos2) -> Option<PartId> {
    let camera = viewport.camera();
    let size = [rect.width(), rect.height()];
    viewport
        .parts()
        .filter_map(|(id, bounds)| {
            let [l, t, r, b] = camera.project_bounds(bounds, size)?;
            let screen = Rect::from_min_max(rect.min + vec2(l, t), rect.min + vec2(r, b));
            screen.contains(at).then(|| (id.clone(), screen.area()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

impl Panel for StagePanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let Some(theatre) = Theatre::from_ctx(ctx) else { return no_book(ui, &ds(ui.ctx())) };
        let views = Views::from_ctx(ctx);
        let io = Io::from_ctx(ctx);
        if !theatre.looking().popped {
            return self.show(ui, theatre, io, views);
        }
        let ds = ds(ui.ctx());
        ui.label(RichText::new(say(&ds, text::POPPED_OUT, &[])).color(ds.theme.text2));
        if ui.add(Button::new(&say(&ds, text::BRING_BACK, &[])).icon(icons::ARROW_U_UP_LEFT)).clicked() {
            theatre.look(|l| l.popped = false);
            return;
        }
        let title = say(&ds, text::STAGE_WINDOW, &[]).into_owned();
        let builder = egui::ViewportBuilder::default().with_title(title).with_inner_size([960.0, 720.0]);
        let id = egui::ViewportId::from_hash_of("theatre-stage");
        ui.ctx().show_viewport_immediate(id, builder, |ctx, class| {
            if class == egui::ViewportClass::EmbeddedWindow {
                egui::Window::new(say(&ds, text::STAGE_WINDOW, &[])).show(ctx, |ui| self.show(ui, theatre, io, views));
            } else {
                egui::CentralPanel::default().show(ctx, |ui| self.show(ui, theatre, io, views));
            }
            if ctx.input(|i| i.viewport().close_requested()) {
                theatre.look(|l| l.popped = false);
            }
        });
    }

    /// Without a character the stage still shows the controls.
    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_read_and_write_a_budget_with_zero_for_no_cap() {
        let budget = Budget { usd_micros: 2_500_000, tokens: Some(40_000), seconds: Some(600) };
        let limits = Limits::of(budget);
        assert_eq!(limits, Limits { dollars: 2.5, tokens: 40_000.0, minutes: 10.0 });
        assert_eq!(limits.budget(), budget);
        let open = Limits { dollars: 0.0, tokens: 0.0, minutes: 0.0 }.budget();
        assert_eq!(open, Budget { usd_micros: 0, tokens: None, seconds: None });
        assert_eq!(clock(192), "3:12");
    }
}
