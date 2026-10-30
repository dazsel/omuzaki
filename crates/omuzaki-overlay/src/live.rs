//! Each widget's live state (the alert showing and those queued, the chat lines, the goal's value, the timer's time),
//! fed by the show's events and the live ops, and what it draws.
//!
//! State is kept apart from the document: the document says what a widget is; this says what it shows right now. A
//! widget whose definition changes keeps its state where that still makes sense (the chat keeps its lines, a goal its
//! value), so editing a widget mid-stream never empties it.

use std::collections::VecDeque;

use omuzaki_live_api::event::LiveEvent;
use serde_json::{Value, json};

use crate::model::{AlertKind, Content, GoalMetric, TimerMode, WidgetDef, parse_hex};
use crate::paint::{Canvas, Face, Fonts, Pen, Rgba};

/// The most alerts that wait their turn; past it the oldest waiting one is dropped.
pub const MAX_QUEUED_ALERTS: usize = 50;
/// How long an alert takes to come in and to go, seconds.
pub const ALERT_EASE_S: f64 = 0.35;

/// One alert.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// What it is for.
    pub kind: AlertKind,
    /// Who it is about: filled into `{name}`.
    pub name: String,
    /// An amount, already formatted (`500 bits`, `$5.00`): `{amount}`.
    pub amount: String,
    /// A count (gifted subs, raiders, months): `{count}` and `{months}`.
    pub count: u32,
    /// The reward redeemed: `{reward}`.
    pub reward: String,
    /// A custom alert's title: `{title}`.
    pub title: String,
    /// The viewer's own words, shown under the title.
    pub message: String,
}

impl Alert {
    /// An alert of `kind` about `name`, with nothing else filled in.
    pub fn new(kind: AlertKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            amount: String::new(),
            count: 0,
            reward: String::new(),
            title: String::new(),
            message: String::new(),
        }
    }

    /// The title, from `template` with the fields filled in.
    pub fn render(&self, template: &str) -> String {
        template
            .replace("{name}", &self.name)
            .replace("{amount}", &self.amount)
            .replace("{count}", &self.count.to_string())
            .replace("{months}", &self.count.to_string())
            .replace("{reward}", &self.reward)
            .replace("{title}", &self.title)
    }
}

/// One line of the chat box.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatLine {
    /// The viewer's display name.
    pub name: String,
    /// Their colour, when the platform gives one.
    pub colour: Option<[u8; 3]>,
    /// What they said.
    pub text: String,
    /// When it arrived, on the overlay's clock.
    pub at: f64,
    /// The platform's message id, so a deleted message can be taken down.
    pub id: String,
}

/// A money amount in millionths as text: `$5.00`, `¥500`, `12.50 EUR`.
pub fn format_money(micros: i64, currency: &str) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a display value")]
    let units = micros as f64 / 1_000_000.0;
    match currency {
        "USD" => format!("${units:.2}"),
        "JPY" => format!("\u{a5}{units:.0}"),
        "EUR" => format!("\u{20ac}{units:.2}"),
        "GBP" => format!("\u{a3}{units:.2}"),
        other => format!("{units:.2} {other}"),
    }
}

fn name_of(user: &Value) -> String {
    user.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("Someone").to_owned()
}

/// The alert an event makes, if it is a kind alerts are for.
pub fn alert_for(event: &LiveEvent) -> Option<Alert> {
    let p = &event.payload;
    let kind = AlertKind::parse(event.kind.as_str())?;
    let mut alert = Alert::new(kind, "");
    match kind {
        AlertKind::Follow => alert.name = name_of(&p["user"]),
        AlertKind::Sub => {
            alert.name = name_of(&p["user"]);
            alert.count = p["months"].as_u64().and_then(|m| u32::try_from(m).ok()).unwrap_or(1);
            p["message"].as_str().unwrap_or_default().clone_into(&mut alert.message);
        }
        AlertKind::SubGift => {
            alert.name = p.get("gifter").map_or_else(|| "Someone".to_owned(), name_of);
            alert.count = p["count"].as_u64().and_then(|m| u32::try_from(m).ok()).unwrap_or(1);
        }
        AlertKind::Cheer => {
            alert.name = p.get("user").map_or_else(|| "Anonymous".to_owned(), name_of);
            alert.amount = format!("{} bits", p["bits"].as_u64().unwrap_or(0));
            p["message"].as_str().unwrap_or_default().clone_into(&mut alert.message);
        }
        AlertKind::Tip => {
            alert.name = name_of(&p["user"]);
            alert.amount = format_money(
                p["amount"]["micros"].as_i64().unwrap_or(0),
                p["amount"]["currency"].as_str().unwrap_or(""),
            );
            p["message"].as_str().unwrap_or_default().clone_into(&mut alert.message);
        }
        AlertKind::Raid => {
            alert.name = name_of(&p["from"]);
            alert.count = p["viewers"].as_u64().and_then(|m| u32::try_from(m).ok()).unwrap_or(0);
        }
        AlertKind::Redeem => {
            alert.name = name_of(&p["user"]);
            p["reward"]["title"].as_str().unwrap_or_default().clone_into(&mut alert.reward);
            p["input"].as_str().unwrap_or_default().clone_into(&mut alert.message);
        }
        AlertKind::Custom => return None,
    }
    Some(alert)
}

/// What a widget shows right now.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Alerts: the one up and when it went up, and those waiting.
    Alerts {
        /// The alert showing and when it started.
        current: Option<(Alert, f64)>,
        /// Those waiting, oldest first.
        queue: VecDeque<Alert>,
    },
    /// The chat box's lines, oldest first.
    Chat(VecDeque<ChatLine>),
    /// A goal's value.
    Goal(f64),
    /// A timer: its time (left, or gone for a stopwatch) and whether it runs.
    Timer {
        /// Seconds left (countdown, subathon) or gone (stopwatch).
        seconds: f64,
        /// Whether it counts.
        running: bool,
    },
    /// Text has no state.
    Text,
}

impl State {
    /// The fresh state for `def`.
    pub fn new(def: &WidgetDef) -> Self {
        match &def.content {
            Content::Alerts(_) => State::Alerts { current: None, queue: VecDeque::new() },
            Content::Chat(_) => State::Chat(VecDeque::new()),
            Content::Goal(g) => State::Goal(g.start),
            Content::Timer(t) => {
                State::Timer { seconds: if t.mode == TimerMode::Stopwatch { 0.0 } else { t.seconds }, running: false }
            }
            Content::Text(_) => State::Text,
        }
    }

    /// The state as `overlay.list` reports it.
    pub fn report(&self) -> Value {
        match self {
            State::Alerts { current, queue } => json!({
                "showing": current.as_ref().map(|(a, _)| a.render(a.kind.default_template())),
                "queued": queue.len(),
            }),
            State::Chat(lines) => json!({ "lines": lines.len() }),
            State::Goal(value) => json!({ "value": value }),
            State::Timer { seconds, running } => json!({ "seconds": seconds, "running": running }),
            State::Text => json!({}),
        }
    }
}

/// A widget at run time: its definition, its state and its cached picture.
#[derive(Debug)]
pub struct LiveWidget {
    /// What the document says it is.
    pub def: WidgetDef,
    /// What it shows now.
    pub state: State,
    /// Bumped whenever what it draws changes.
    revision: u64,
    /// The picture drawn at a revision and frame size.
    picture: Option<(u64, (u32, u32), Canvas)>,
    /// The last whole second a timer showed, so it redraws once a second.
    shown_second: i64,
}

impl LiveWidget {
    /// A widget for `def` with fresh state.
    pub fn new(def: WidgetDef) -> Self {
        let state = State::new(&def);
        Self { def, state, revision: 1, picture: None, shown_second: i64::MIN }
    }

    /// Takes a new definition, keeping the state when the kind is the same.
    pub fn redefine(&mut self, def: WidgetDef) {
        if def.content.kind() != self.def.content.kind() {
            self.state = State::new(&def);
        } else if let (Content::Timer(old), Content::Timer(new), State::Timer { seconds, running: false }) =
            (&self.def.content, &def.content, &mut self.state)
            && (old.mode != new.mode || (old.seconds - new.seconds).abs() > f64::EPSILON)
        {
            // A stopped timer whose start changed shows the new start.
            *seconds = if new.mode == TimerMode::Stopwatch { 0.0 } else { new.seconds };
        }
        self.def = def;
        self.touch();
    }

    /// Marks the picture stale.
    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Takes an event from the show at `now`. Returns whether the widget changed.
    pub fn observe(&mut self, event: &LiveEvent, now: f64) -> bool {
        let kind = event.kind.as_str();
        let p = &event.payload;
        let changed = match (&self.def.content, &mut self.state) {
            (Content::Alerts(def), State::Alerts { queue, .. }) => {
                let Some(alert) = alert_for(event) else { return false };
                if !def.events.iter().any(|e| e == alert.kind.event()) {
                    return false;
                }
                if queue.len() >= MAX_QUEUED_ALERTS {
                    queue.pop_front();
                }
                queue.push_back(alert);
                true
            }
            (Content::Chat(def), State::Chat(lines)) => match kind {
                "chat.message" => {
                    let text = p["text"].as_str().unwrap_or_default();
                    if text.is_empty() || (def.hide_commands && text.starts_with('!')) {
                        return false;
                    }
                    lines.push_back(ChatLine {
                        name: name_of(&p["user"]),
                        colour: p["user"]["colour"].as_str().and_then(parse_hex),
                        text: text.to_owned(),
                        at: now,
                        id: p["message_id"].as_str().unwrap_or_default().to_owned(),
                    });
                    while lines.len() > def.lines as usize {
                        lines.pop_front();
                    }
                    true
                }
                _ => false,
            },
            (Content::Goal(def), State::Goal(value)) => {
                let add = match (def.metric, kind) {
                    (GoalMetric::Follows, "follow") | (GoalMetric::Subs, "sub") => 1.0,
                    (GoalMetric::Subs, "sub_gift") => p["count"].as_f64().unwrap_or(1.0),
                    (GoalMetric::Bits, "cheer") => p["bits"].as_f64().unwrap_or(0.0),
                    #[allow(clippy::cast_precision_loss, reason = "a display value")]
                    (GoalMetric::Tips, "tip") => p["amount"]["micros"].as_i64().unwrap_or(0) as f64 / 1_000_000.0,
                    _ => 0.0,
                };
                // A gifted sub also arrives as a sub event of its own on Twitch; count the gift event only.
                let gifted = kind == "sub" && p["gift"].as_bool().unwrap_or(false);
                if add <= 0.0 || gifted {
                    return false;
                }
                *value += add;
                true
            }
            (Content::Timer(def), State::Timer { seconds, .. }) if def.mode == TimerMode::Subathon => {
                let a = &def.add;
                #[allow(clippy::cast_precision_loss, reason = "a display value")]
                let add = match kind {
                    "follow" => a.follow,
                    "sub" if !p["gift"].as_bool().unwrap_or(false) => a.sub,
                    "sub_gift" => a.gift * p["count"].as_f64().unwrap_or(1.0),
                    "cheer" => a.bits_100 * p["bits"].as_f64().unwrap_or(0.0) / 100.0,
                    "tip" => a.tip_unit * p["amount"]["micros"].as_i64().unwrap_or(0) as f64 / 1_000_000.0,
                    "raid" => a.raid,
                    _ => 0.0,
                };
                if add <= 0.0 {
                    return false;
                }
                *seconds += add;
                true
            }
            _ => false,
        };
        if changed {
            self.touch();
        }
        changed
    }

    /// Queues `alert` (alerts only). Returns whether it was taken.
    pub fn push_alert(&mut self, alert: Alert) -> bool {
        let State::Alerts { queue, .. } = &mut self.state else { return false };
        if queue.len() >= MAX_QUEUED_ALERTS {
            queue.pop_front();
        }
        queue.push_back(alert);
        self.touch();
        true
    }

    /// Moves time on to `now` (`dt` since the last step): alerts take turns, chat lines expire, timers count.
    pub fn step(&mut self, now: f64, dt: f64) {
        match (&self.def.content, &mut self.state) {
            (Content::Alerts(def), State::Alerts { current, queue }) => {
                if current.as_ref().is_some_and(|(_, at)| now - at >= f64::from(def.seconds)) {
                    *current = None;
                    self.revision = self.revision.wrapping_add(1);
                }
                if current.is_none()
                    && let Some(next) = queue.pop_front()
                {
                    *current = Some((next, now));
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            (Content::Chat(def), State::Chat(lines)) if def.seconds > 0.0 => {
                let before = lines.len();
                lines.retain(|l| now - l.at < f64::from(def.seconds));
                if lines.len() != before {
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            (Content::Timer(def), State::Timer { seconds, running }) => {
                if *running {
                    if def.mode == TimerMode::Stopwatch {
                        *seconds += dt;
                    } else {
                        *seconds = (*seconds - dt).max(0.0);
                        if *seconds <= 0.0 {
                            *running = false;
                        }
                    }
                }
                #[allow(clippy::cast_possible_truncation, reason = "at most ten days of seconds")]
                let whole = seconds.ceil() as i64;
                if whole != self.shown_second {
                    self.shown_second = whole;
                    self.revision = self.revision.wrapping_add(1);
                }
            }
            _ => {}
        }
    }

    /// Whether anything is on screen: an empty alert slot, an empty chat or a hidden widget draws nothing.
    pub fn showing(&self) -> bool {
        match &self.state {
            State::Alerts { current, .. } => current.is_some(),
            State::Chat(lines) => !lines.is_empty(),
            _ => true,
        }
    }

    /// The picture for a frame `size`, drawn again only if the widget changed or the frame resized.
    pub fn picture(&mut self, fonts: &Fonts, size: (u32, u32)) -> &Canvas {
        if !matches!(&self.picture, Some((rev, at, _)) if *rev == self.revision && *at == size) {
            self.picture = None;
        }
        let (revision, def, state) = (self.revision, &self.def, &self.state);
        &self.picture.get_or_insert_with(|| (revision, size, draw(def, state, fonts, size))).2
    }

    /// Opacity and vertical offset (pixels at `height`) of an alert coming in or going: it rises and fades in, then
    /// fades out.
    pub fn motion(&self, now: f64, height: u32) -> (f32, f32) {
        let (Content::Alerts(def), State::Alerts { current: Some((_, at)), .. }) = (&self.def.content, &self.state)
        else {
            return (1.0, 0.0);
        };
        let age = now - at;
        let left = f64::from(def.seconds) - age;
        let t_in = (age / ALERT_EASE_S).clamp(0.0, 1.0);
        let t_out = (left / ALERT_EASE_S).clamp(0.0, 1.0);
        let ease = |t: f64| 1.0 - (1.0 - t).powi(3);
        #[allow(clippy::cast_possible_truncation, reason = "opacity and pixel offsets")]
        let (opacity, rise) = ((ease(t_in) * t_out) as f32, ((1.0 - ease(t_in)) * f64::from(height) * 0.03) as f32);
        (opacity, rise)
    }
}

/// `00:42`, `12:05`, `1:02:03`.
pub fn clock_text(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most ten days, never negative")]
    let s = seconds.max(0.0).ceil() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// What every widget's picture shares: its colours, sizes and the fonts.
struct Look<'a> {
    fonts: &'a Fonts,
    accent: Rgba,
    text: Rgba,
    panel: Rgba,
    outline: bool,
    /// One pixel of a 1080-line frame, at the widget's scale.
    unit: f32,
    width: f32,
    pad: f32,
    radius: f32,
    /// The width inside the padding.
    inner: f32,
}

impl Look<'_> {
    fn pen(&self, face: Face, px: f32, colour: Rgba) -> Pen {
        Pen { face, px, colour, outline: self.outline }
    }

    /// A canvas `height` tall with the panel drawn.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel sizes")]
    fn canvas(&self, height: f32) -> Canvas {
        let mut c = Canvas::new(self.width.ceil() as u32, height.ceil() as u32);
        c.round_rect(0.0, 0.0, self.width, height, self.radius, self.panel);
        c
    }

    /// Draws `lines` from `y` (their top), centred or from the padding, each `step` apart; returns the bottom.
    fn lines(&self, c: &mut Canvas, lines: &[String], pen: Pen, y: f32, step: f32, centred: bool) -> f32 {
        let mut y = y;
        let ascent = self.fonts.ascent(pen.face, pen.px);
        for line in lines {
            let x = if centred { (self.width - self.fonts.width(pen.face, line, pen.px)) * 0.5 } else { self.pad };
            c.text(self.fonts, pen, line, x, y + ascent);
            y += step;
        }
        y
    }
}

/// Draws `def` showing `state` for a frame of `size`.
#[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
fn draw(def: &WidgetDef, state: &State, fonts: &Fonts, size: (u32, u32)) -> Canvas {
    let style = &def.style;
    // Sizes scale with the frame's height, so a 720p and a 4K program look the same.
    let unit = size.1 as f32 / 1080.0 * def.layout.scale;
    let width = (size.0 as f32 * def.layout.width).max(16.0);
    let pad = 18.0 * unit;
    let look = Look {
        fonts,
        accent: Rgba::bytes(parse_hex(&style.accent).unwrap_or([255, 111, 174]), 1.0),
        text: Rgba::bytes(parse_hex(&style.text).unwrap_or([255, 255, 255]), 1.0),
        panel: Rgba::bytes(parse_hex(&style.panel).unwrap_or([27, 20, 36]), style.panel_opacity),
        outline: style.panel_opacity < 0.35,
        unit,
        width,
        pad,
        radius: 14.0 * unit,
        inner: (width - 2.0 * pad).max(8.0),
    };
    match (&def.content, state) {
        (Content::Alerts(a), State::Alerts { current: Some((alert, _)), .. }) => {
            let template = a.templates.get(alert.kind.event()).map_or(alert.kind.default_template(), String::as_str);
            draw_alert(&look, &alert.render(template), &alert.message)
        }
        (Content::Chat(_), State::Chat(lines)) => draw_chat(&look, lines),
        (Content::Goal(g), State::Goal(value)) => draw_goal(&look, &g.title, *value, g.target),
        (Content::Timer(t), State::Timer { seconds, .. }) => draw_timer(&look, &t.label, *seconds),
        (Content::Text(t), _) => draw_text(&look, &t.text, &t.subtitle),
        _ => Canvas::new(1, 1),
    }
}

#[allow(clippy::cast_precision_loss, reason = "line counts")]
fn draw_alert(look: &Look<'_>, title: &str, message: &str) -> Canvas {
    let (fonts, unit) = (look.fonts, look.unit);
    let title_px = 44.0 * unit;
    let body_px = 26.0 * unit;
    let titles = fonts.wrap(Face::Display, title, title_px, look.inner);
    let body: Vec<String> = if message.is_empty() {
        Vec::new()
    } else {
        fonts.wrap(Face::Body, message, body_px, look.inner).into_iter().take(3).collect()
    };
    let gap = if body.is_empty() { 0.0 } else { 6.0 * unit };
    let height = look.pad * 2.0 + titles.len() as f32 * title_px * 1.15 + body.len() as f32 * body_px * 1.3 + gap;
    let mut c = look.canvas(height);
    // The accent edge along the bottom.
    c.round_rect(0.0, height - 6.0 * unit, look.width, 6.0 * unit, 3.0 * unit, look.accent);
    let y = look.lines(&mut c, &titles, look.pen(Face::Display, title_px, look.text), look.pad, title_px * 1.15, true);
    look.lines(&mut c, &body, look.pen(Face::Body, body_px, look.text.fade(0.88)), y + gap, body_px * 1.3, true);
    c
}

#[allow(clippy::cast_precision_loss, reason = "line counts")]
fn draw_chat(look: &Look<'_>, lines: &VecDeque<ChatLine>) -> Canvas {
    let fonts = look.fonts;
    let px = 24.0 * look.unit;
    let line_h = px * 1.32;
    // Each message: the name, then its words wrapped after it.
    let mut laid: Vec<(Option<(String, Rgba)>, String)> = Vec::new();
    for line in lines {
        let name = format!("{}:", fonts.fit(Face::Body, &line.name, px, look.inner * 0.45));
        let colour = line.colour.map_or(look.accent, |c| Rgba::bytes(lift(c), 1.0));
        let indent = fonts.width(Face::Body, &name, px) + px * 0.3;
        let wrapped = fonts.wrap_after(Face::Body, &line.text, px, (look.inner - indent).max(px * 2.0), look.inner);
        let mut name = Some((name, colour));
        for words in wrapped {
            laid.push((name.take(), words));
        }
    }
    let mut c = look.canvas(look.pad * 2.0 + laid.len() as f32 * line_h);
    let mut y = look.pad + fonts.ascent(Face::Body, px);
    for (name, words) in &laid {
        let mut x = look.pad;
        if let Some((name, colour)) = name {
            x = c.text(fonts, look.pen(Face::Body, px, *colour), name, x, y) + px * 0.3;
        }
        c.text(fonts, look.pen(Face::Body, px, look.text), words, x, y);
        y += line_h;
    }
    c
}

#[allow(clippy::cast_possible_truncation, reason = "a fraction of the bar")]
fn draw_goal(look: &Look<'_>, title: &str, value: f64, target: f64) -> Canvas {
    let (fonts, unit, pad, inner) = (look.fonts, look.unit, look.pad, look.inner);
    let title_px = 26.0 * unit;
    let bar_h = 30.0 * unit;
    let mut c = look.canvas(pad * 2.0 + title_px * 1.3 + 8.0 * unit + bar_h);
    let y = pad + fonts.ascent(Face::Body, title_px);
    let title = fonts.fit(Face::Body, title, title_px, inner * 0.62);
    c.text(fonts, look.pen(Face::Body, title_px, look.text), &title, pad, y);
    let count = format!("{} / {}", number(value), number(target));
    let cw = fonts.width(Face::Body, &count, title_px);
    c.text(fonts, look.pen(Face::Body, title_px, look.text.fade(0.85)), &count, look.width - pad - cw, y);
    let bar_y = pad + title_px * 1.3 + 8.0 * unit;
    c.round_rect(pad, bar_y, inner, bar_h, bar_h * 0.5, Rgba { rgb: [1.0, 1.0, 1.0], a: 0.14 });
    let share = (value / target).clamp(0.0, 1.0) as f32;
    if share > 0.0 {
        c.round_rect(pad, bar_y, (inner * share).max(bar_h), bar_h, bar_h * 0.5, look.accent);
    }
    c
}

fn draw_timer(look: &Look<'_>, label: &str, seconds: f64) -> Canvas {
    let (fonts, unit, pad) = (look.fonts, look.unit, look.pad);
    let digits_px = 56.0 * unit;
    let label_px = 22.0 * unit;
    let label_h = if label.is_empty() { 0.0 } else { label_px * 1.4 };
    let mut c = look.canvas(pad * 2.0 + digits_px * 1.05 + label_h);
    let mut y = pad;
    if !label.is_empty() {
        let label = [fonts.fit(Face::Body, label, label_px, look.inner)];
        y = look.lines(&mut c, &label, look.pen(Face::Body, label_px, look.accent), y, label_h, true);
    }
    look.lines(&mut c, &[clock_text(seconds)], look.pen(Face::Display, digits_px, look.text), y, digits_px, true);
    c
}

#[allow(clippy::cast_precision_loss, reason = "line counts")]
fn draw_text(look: &Look<'_>, text: &str, subtitle: &str) -> Canvas {
    let (fonts, unit) = (look.fonts, look.unit);
    let px = 40.0 * unit;
    let sub_px = 24.0 * unit;
    let lines = fonts.wrap(Face::Display, text, px, look.inner);
    let subs = if subtitle.is_empty() { Vec::new() } else { fonts.wrap(Face::Body, subtitle, sub_px, look.inner) };
    let height = look.pad * 2.0 + lines.len() as f32 * px * 1.15 + subs.len() as f32 * sub_px * 1.35;
    let mut c = look.canvas(height);
    c.round_rect(0.0, 0.0, 6.0 * unit, height, 3.0 * unit, look.accent);
    let y = look.lines(&mut c, &lines, look.pen(Face::Display, px, look.text), look.pad, px * 1.15, false);
    look.lines(&mut c, &subs, look.pen(Face::Body, sub_px, look.text.fade(0.85)), y, sub_px * 1.35, false);
    c
}

/// A whole number as an integer, else with up to two decimals.
fn number(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 { format!("{v:.0}") } else { format!("{v:.2}") }
}

/// A viewer's colour lifted until it reads on a dark panel (dark blue names are common).
fn lift(rgb: [u8; 3]) -> [u8; 3] {
    let luma = 0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2]);
    if luma >= 110.0 {
        return rgb;
    }
    let k = 110.0 / luma.max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to a byte")]
    rgb.map(|c| (f32::from(c) * k + 40.0).clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;
    use omuzaki_live_api::event::Source;
    use omuzaki_live_api::ids::EventKind;

    use super::*;
    use crate::model::{AlertsDef, ChatDef, GoalDef, Kind, TimerDef};

    fn event(kind: &str, payload: Value) -> LiveEvent {
        LiveEvent::new(EventKind::new(kind).unwrap(), Source::Input, Seconds(0.0), payload)
    }

    fn user(name: &str) -> Value {
        json!({ "id": name, "login": name, "name": name })
    }

    #[test]
    fn alerts_take_turns_and_expire() {
        let mut w =
            LiveWidget::new(WidgetDef::new(Content::Alerts(AlertsDef { seconds: 2.0, ..AlertsDef::default() })));
        assert!(w.observe(&event("follow", json!({ "user": user("Ann") })), 0.0));
        assert!(w.observe(&event("raid", json!({ "from": user("Bo"), "viewers": 40 })), 0.0));
        w.step(0.0, 0.0);
        let State::Alerts { current, queue } = &w.state else { panic!() };
        assert_eq!(current.as_ref().map(|(a, _)| a.render(a.kind.default_template())), Some("Ann followed!".into()));
        assert_eq!(queue.len(), 1);
        w.step(2.1, 2.1);
        let State::Alerts { current, .. } = &w.state else { panic!() };
        assert_eq!(
            current.as_ref().map(|(a, _)| a.render(a.kind.default_template())),
            Some("Bo is raiding with 40!".into())
        );
    }

    #[test]
    fn alerts_ignore_kinds_they_were_not_set_for() {
        let def = AlertsDef { events: vec!["sub".into()], ..AlertsDef::default() };
        let mut w = LiveWidget::new(WidgetDef::new(Content::Alerts(def)));
        assert!(!w.observe(&event("follow", json!({ "user": user("Ann") })), 0.0));
        assert!(w.observe(&event("sub", json!({ "user": user("Ann"), "tier": "1000", "months": 3 })), 0.0));
    }

    #[test]
    fn a_tip_reads_as_money() {
        let alert = alert_for(&event(
            "tip",
            json!({ "user": user("Cy"), "amount": { "micros": 5_000_000, "currency": "USD" }, "message": "gm" }),
        ))
        .unwrap();
        assert_eq!(alert.render(AlertKind::Tip.default_template()), "Cy sent $5.00!");
        assert_eq!(alert.message, "gm");
    }

    #[test]
    fn chat_keeps_its_last_lines_and_hides_commands() {
        let mut w = LiveWidget::new(WidgetDef::new(Content::Chat(ChatDef { lines: 2, ..ChatDef::default() })));
        for (i, text) in ["one", "!bonk", "two", "three"].into_iter().enumerate() {
            w.observe(
                &event("chat.message", json!({ "user": user("V"), "text": text, "message_id": i.to_string() })),
                0.0,
            );
        }
        let State::Chat(lines) = &w.state else { panic!() };
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["two", "three"]);
        w.step(61.0, 61.0);
        assert!(!w.showing(), "lines go after their minute");
    }

    #[test]
    fn a_goal_counts_its_metric_once_per_gift() {
        let mut w = LiveWidget::new(WidgetDef::new(Content::Goal(GoalDef::default())));
        w.observe(&event("sub", json!({ "user": user("A"), "tier": "1000", "months": 1 })), 0.0);
        w.observe(&event("sub_gift", json!({ "count": 5, "tier": "1000" })), 0.0);
        w.observe(&event("sub", json!({ "user": user("B"), "tier": "1000", "months": 1, "gift": true })), 0.0);
        w.observe(&event("follow", json!({ "user": user("C") })), 0.0);
        assert_eq!(w.state, State::Goal(6.0));
    }

    #[test]
    fn a_subathon_gains_time_from_events_and_counts_down() {
        let def = TimerDef { mode: TimerMode::Subathon, seconds: 600.0, ..TimerDef::default() };
        let mut w = LiveWidget::new(WidgetDef::new(Content::Timer(def)));
        let State::Timer { running, .. } = &mut w.state else { panic!() };
        *running = true;
        w.observe(&event("sub", json!({ "user": user("A"), "tier": "1000", "months": 1 })), 0.0);
        w.observe(&event("cheer", json!({ "bits": 500 })), 0.0);
        w.step(10.0, 10.0);
        // 600 + 60 + 5 x 30 - 10
        assert_eq!(w.state, State::Timer { seconds: 800.0, running: true });
    }

    #[test]
    fn a_countdown_stops_at_zero() {
        let def = TimerDef { seconds: 3.0, ..TimerDef::default() };
        let mut w = LiveWidget::new(WidgetDef::new(Content::Timer(def)));
        if let State::Timer { running, .. } = &mut w.state {
            *running = true;
        }
        w.step(5.0, 5.0);
        assert_eq!(w.state, State::Timer { seconds: 0.0, running: false });
    }

    #[test]
    fn redefining_keeps_the_state_of_the_same_kind() {
        let mut w = LiveWidget::new(WidgetDef::new(Content::Goal(GoalDef::default())));
        w.observe(&event("sub", json!({ "user": user("A"), "tier": "1000", "months": 1 })), 0.0);
        let mut def = w.def.clone();
        def.layout.width = 0.5;
        w.redefine(def);
        assert_eq!(w.state, State::Goal(1.0));
        w.redefine(WidgetDef::new(Content::of(Kind::Text)));
        assert_eq!(w.state, State::Text);
    }

    #[test]
    fn pictures_are_cached_until_the_widget_changes() {
        let fonts = Fonts::new().unwrap();
        let mut w = LiveWidget::new(WidgetDef::new(Content::Goal(GoalDef::default())));
        let first = w.picture(&fonts, (1920, 1080)).clone();
        assert!(first.width > 400 && first.height > 50, "{}x{}", first.width, first.height);
        assert_eq!(w.picture(&fonts, (1920, 1080)), &first);
        w.observe(&event("sub", json!({ "user": user("A"), "tier": "1000", "months": 1 })), 0.0);
        assert_ne!(w.picture(&fonts, (1920, 1080)), &first, "the bar moved");
        assert!(w.picture(&fonts, (1280, 720)).width < first.width, "a smaller frame draws it smaller");
    }

    #[test]
    fn clocks_read_like_clocks() {
        assert_eq!(clock_text(42.0), "00:42");
        assert_eq!(clock_text(725.2), "12:06");
        assert_eq!(clock_text(3723.0), "1:02:03");
        assert_eq!(clock_text(-4.0), "00:00");
    }
}
