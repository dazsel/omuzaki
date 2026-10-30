//! The overlay as the document keeps it: one [`WidgetDef`] per widget under `overlay/widgets/<id>`, and the program's
//! default transition under `overlay/settings`. Everything here is plain data, saved with the project and undoable,
//! so a person, a script and an agent edit the same widgets through the same ops.

use std::collections::BTreeMap;

use omuzaki_core::slug_id;
use omuzaki_doc_api::DocPath;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::transition::TransitionSpec;

slug_id!(
    /// A widget of the overlay (`overlay/widgets/<id>`).
    WidgetId
);

/// Where the overlay lives in the document.
pub mod paths {
    use super::{DocPath, WidgetId};

    /// `overlay`: the whole overlay.
    #[must_use]
    pub fn root() -> DocPath {
        DocPath::root().join("overlay")
    }

    /// `overlay/widgets`: every widget.
    #[must_use]
    pub fn widgets() -> DocPath {
        root().join("widgets")
    }

    /// `overlay/widgets/<id>`: one widget.
    #[must_use]
    pub fn widget(id: &WidgetId) -> DocPath {
        widgets().join(id.as_str())
    }

    /// `overlay/settings`: the safe margin and the program's default transition.
    #[must_use]
    pub fn settings() -> DocPath {
        root().join("settings")
    }
}

/// The most widgets one overlay may hold: each costs a picture and a blend per frame.
pub const MAX_WIDGETS: usize = 32;
/// The longest text a widget's own fields may carry, in bytes.
pub const MAX_TEXT: usize = 500;

/// One on-stream widget.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "RawWidgetDef")]
pub struct WidgetDef {
    /// What it is, with its own settings.
    #[serde(flatten)]
    pub content: Content,
    /// Where it sits on the program.
    #[serde(default)]
    pub layout: Layout,
    /// Its colours.
    #[serde(default)]
    pub style: Style,
    /// Whether it is drawn at all.
    #[serde(default = "yes")]
    pub visible: bool,
    /// The program scenes it shows in; empty for every scene.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<String>,
}

const fn yes() -> bool {
    true
}

/// A widget as written, before a missing layout becomes its kind's usual one.
#[derive(Deserialize, JsonSchema)]
struct RawWidgetDef {
    #[serde(flatten)]
    content: Content,
    #[serde(default)]
    layout: Option<Layout>,
    #[serde(default)]
    style: Style,
    #[serde(default = "yes")]
    visible: bool,
    #[serde(default)]
    scenes: Vec<String>,
}

impl From<RawWidgetDef> for WidgetDef {
    fn from(raw: RawWidgetDef) -> Self {
        let layout = raw.layout.unwrap_or_else(|| raw.content.kind().default_layout());
        Self { content: raw.content, layout, style: raw.style, visible: raw.visible, scenes: raw.scenes }
    }
}

impl WidgetDef {
    /// A visible widget with `content`, laid out where its kind usually goes, in the default style.
    pub fn new(content: Content) -> Self {
        let layout = content.kind().default_layout();
        Self { content, layout, style: Style::default(), visible: true, scenes: Vec::new() }
    }

    /// Whether it shows in `scene` (the program scene; `None` when the program follows the project's default).
    pub fn shows_in(&self, scene: Option<&str>) -> bool {
        self.visible && (self.scenes.is_empty() || scene.is_some_and(|s| self.scenes.iter().any(|w| w == s)))
    }

    /// Checks the fields an op can't check by type.
    ///
    /// # Errors
    /// The field at fault and why.
    pub fn check(&self) -> Result<(), (String, String)> {
        self.layout.check()?;
        self.style.check()?;
        self.content.check()
    }
}

/// The kinds of widget.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Pops up for follows, subs, gifts, cheers, tips, raids and redeems, one at a time.
    Alerts,
    /// The latest chat messages, with names in each viewer's colour.
    Chat,
    /// A goal bar that fills from the show's events (follows, subs, bits, tips) or by hand.
    Goal,
    /// A countdown, a stopwatch, or a subathon timer that subs, bits, tips and follows add time to.
    Timer,
    /// A line of text: a title, a lower third, "be right back".
    Text,
}

impl Kind {
    /// Every kind, in the order `overlay.kinds` lists them.
    pub const ALL: [Kind; 5] = [Kind::Alerts, Kind::Chat, Kind::Goal, Kind::Timer, Kind::Text];

    /// Its name in ops and the document.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Alerts => "alerts",
            Kind::Chat => "chat",
            Kind::Goal => "goal",
            Kind::Timer => "timer",
            Kind::Text => "text",
        }
    }

    /// One line on what it does.
    pub const fn summary(self) -> &'static str {
        match self {
            Kind::Alerts => "Pops up for follows, subs, gifts, cheers, tips, raids and redeems, one at a time.",
            Kind::Chat => "The latest chat messages, with names in each viewer's colour.",
            Kind::Goal => "A goal bar that fills from follows, subs, bits or tips, or by hand.",
            Kind::Timer => "A countdown, a stopwatch, or a subathon timer that events add time to.",
            Kind::Text => "A line of text: a title, a lower third, a 'be right back' card.",
        }
    }

    /// Where it usually goes: alerts top centre, chat bottom left, goal top left, timer top right, text bottom.
    pub fn default_layout(self) -> Layout {
        let (anchor, width) = match self {
            Kind::Alerts => (Anchor::Top, 0.36),
            Kind::Chat => (Anchor::BottomLeft, 0.28),
            Kind::Goal => (Anchor::TopLeft, 0.26),
            Kind::Timer => (Anchor::TopRight, 0.16),
            Kind::Text => (Anchor::Bottom, 0.5),
        };
        Layout { anchor, x: 0.0, y: 0.0, width, scale: 1.0 }
    }
}

/// What a widget is, with the settings of its kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    /// Alerts for the show's events.
    Alerts(AlertsDef),
    /// The chat box.
    Chat(ChatDef),
    /// A goal bar.
    Goal(GoalDef),
    /// A timer.
    Timer(TimerDef),
    /// A line of text.
    Text(TextDef),
}

impl Content {
    /// Its kind.
    pub const fn kind(&self) -> Kind {
        match self {
            Content::Alerts(_) => Kind::Alerts,
            Content::Chat(_) => Kind::Chat,
            Content::Goal(_) => Kind::Goal,
            Content::Timer(_) => Kind::Timer,
            Content::Text(_) => Kind::Text,
        }
    }

    /// The default content of `kind`.
    pub fn of(kind: Kind) -> Self {
        match kind {
            Kind::Alerts => Content::Alerts(AlertsDef::default()),
            Kind::Chat => Content::Chat(ChatDef::default()),
            Kind::Goal => Content::Goal(GoalDef::default()),
            Kind::Timer => Content::Timer(TimerDef::default()),
            Kind::Text => Content::Text(TextDef::default()),
        }
    }

    fn check(&self) -> Result<(), (String, String)> {
        let bad = |field: &str, why: String| Err((field.to_owned(), why));
        match self {
            Content::Alerts(a) => {
                if !(1.0..=30.0).contains(&a.seconds) {
                    return bad("seconds", format!("{} is outside 1 to 30 seconds", a.seconds));
                }
                if a.events.iter().any(|e| AlertKind::parse(e).is_none()) {
                    return bad("events", format!("the kinds alerts take are {}", AlertKind::names().join(", ")));
                }
                for (event, template) in &a.templates {
                    if AlertKind::parse(event).is_none() {
                        return bad("templates", format!("'{event}' is not an alert kind"));
                    }
                    if template.len() > MAX_TEXT {
                        return bad("templates", format!("longer than {MAX_TEXT} bytes"));
                    }
                }
                Ok(())
            }
            Content::Chat(c) => {
                if !(1..=20).contains(&c.lines) {
                    return bad("lines", format!("{} is outside 1 to 20", c.lines));
                }
                if !(0.0..=3600.0).contains(&c.seconds) {
                    return bad("seconds", format!("{} is outside 0 to 3600", c.seconds));
                }
                Ok(())
            }
            Content::Goal(g) => {
                if !(g.target.is_finite() && g.target > 0.0) {
                    return bad("target", "must be above zero".to_owned());
                }
                if !(g.start.is_finite() && g.start >= 0.0) {
                    return bad("start", "must be zero or more".to_owned());
                }
                if g.title.len() > MAX_TEXT {
                    return bad("title", format!("longer than {MAX_TEXT} bytes"));
                }
                Ok(())
            }
            Content::Timer(t) => {
                if !(t.seconds.is_finite() && (0.0..=864_000.0).contains(&t.seconds)) {
                    return bad("seconds", "must be 0 to 864000 (ten days)".to_owned());
                }
                let adds = [t.add.follow, t.add.sub, t.add.gift, t.add.bits_100, t.add.tip_unit, t.add.raid];
                if adds.iter().any(|a| !(a.is_finite() && (0.0..=86_400.0).contains(a))) {
                    return bad("add", "each must be 0 to 86400 seconds".to_owned());
                }
                if t.label.len() > MAX_TEXT {
                    return bad("label", format!("longer than {MAX_TEXT} bytes"));
                }
                Ok(())
            }
            Content::Text(t) => {
                if t.text.len() > MAX_TEXT || t.subtitle.len() > MAX_TEXT {
                    return bad("text", format!("longer than {MAX_TEXT} bytes"));
                }
                Ok(())
            }
        }
    }
}

/// The alert kinds, the events they come from, and their default wording.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AlertKind {
    /// `follow`.
    Follow,
    /// `sub`.
    Sub,
    /// `sub_gift`.
    SubGift,
    /// `cheer`.
    Cheer,
    /// `tip` (Super Chats and Super Stickers too).
    Tip,
    /// `raid`.
    Raid,
    /// `redeem`.
    Redeem,
    /// `overlay.alert`: one made by hand or by an automation.
    Custom,
}

impl AlertKind {
    const ALL: [AlertKind; 8] = [
        AlertKind::Follow,
        AlertKind::Sub,
        AlertKind::SubGift,
        AlertKind::Cheer,
        AlertKind::Tip,
        AlertKind::Raid,
        AlertKind::Redeem,
        AlertKind::Custom,
    ];

    /// The event kind it comes from (`custom` for `overlay.alert`).
    pub const fn event(self) -> &'static str {
        match self {
            AlertKind::Follow => "follow",
            AlertKind::Sub => "sub",
            AlertKind::SubGift => "sub_gift",
            AlertKind::Cheer => "cheer",
            AlertKind::Tip => "tip",
            AlertKind::Raid => "raid",
            AlertKind::Redeem => "redeem",
            AlertKind::Custom => "custom",
        }
    }

    /// The kind for event kind `name`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.event() == name)
    }

    /// Every kind's event name.
    pub fn names() -> Vec<&'static str> {
        Self::ALL.iter().map(|k| k.event()).collect()
    }

    /// The title it shows when the widget sets none: `{name}` and the other fields of [`crate::live::Alert`] are filled in.
    pub const fn default_template(self) -> &'static str {
        match self {
            AlertKind::Follow => "{name} followed!",
            AlertKind::Sub => "{name} subscribed!",
            AlertKind::SubGift => "{name} gifted {count} subs!",
            AlertKind::Cheer => "{name} cheered {amount}!",
            AlertKind::Tip => "{name} sent {amount}!",
            AlertKind::Raid => "{name} is raiding with {count}!",
            AlertKind::Redeem => "{name} redeemed {reward}",
            AlertKind::Custom => "{title}",
        }
    }
}

/// Alerts' settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AlertsDef {
    /// The event kinds it alerts for: `follow`, `sub`, `sub_gift`, `cheer`, `tip`, `raid`, `redeem`, `custom`.
    pub events: Vec<String>,
    /// How long each alert stays up, seconds (1 to 30).
    pub seconds: f32,
    /// Titles by event kind, replacing the default wording: `{name}`, `{amount}`, `{count}`, `{reward}`, `{months}`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub templates: BTreeMap<String, String>,
}

impl Default for AlertsDef {
    fn default() -> Self {
        Self {
            events: AlertKind::names().into_iter().map(str::to_owned).collect(),
            seconds: 5.0,
            templates: BTreeMap::new(),
        }
    }
}

/// The chat box's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChatDef {
    /// How many messages it shows at most (1 to 20).
    pub lines: u32,
    /// How long a message stays, seconds; 0 keeps it until newer ones push it out.
    pub seconds: f32,
    /// Hides messages that start with `!` (chat commands for bots and automations).
    pub hide_commands: bool,
}

impl Default for ChatDef {
    fn default() -> Self {
        Self { lines: 8, seconds: 60.0, hide_commands: true }
    }
}

/// What fills a goal bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalMetric {
    /// One per follow.
    Follows,
    /// One per sub, and one per gifted sub.
    #[default]
    Subs,
    /// Bits cheered.
    Bits,
    /// Tips and Super Chats, in units of the currency (all currencies counted alike).
    Tips,
    /// Only `overlay.goal` moves it.
    Manual,
}

/// A goal bar's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GoalDef {
    /// What it says above the bar.
    pub title: String,
    /// What fills it.
    pub metric: GoalMetric,
    /// The amount that fills it.
    pub target: f64,
    /// Where it starts when the studio opens.
    pub start: f64,
}

impl Default for GoalDef {
    fn default() -> Self {
        Self { title: "Sub goal".to_owned(), metric: GoalMetric::Subs, target: 50.0, start: 0.0 }
    }
}

/// How a timer counts.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerMode {
    /// Down from `seconds` to zero.
    #[default]
    Countdown,
    /// Up from zero.
    Stopwatch,
    /// Down from `seconds`, with time added by the show's events (`add`).
    Subathon,
}

/// Seconds a subathon timer gains per event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TimeAdds {
    /// Per follow.
    pub follow: f64,
    /// Per sub.
    pub sub: f64,
    /// Per gifted sub.
    pub gift: f64,
    /// Per 100 bits.
    pub bits_100: f64,
    /// Per unit of currency tipped.
    pub tip_unit: f64,
    /// Per raid.
    pub raid: f64,
}

impl Default for TimeAdds {
    fn default() -> Self {
        Self { follow: 0.0, sub: 60.0, gift: 60.0, bits_100: 30.0, tip_unit: 12.0, raid: 0.0 }
    }
}

/// A timer's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TimerDef {
    /// How it counts.
    pub mode: TimerMode,
    /// Where a countdown or subathon starts, seconds.
    pub seconds: f64,
    /// What it says beside the time.
    pub label: String,
    /// What a subathon gains per event.
    pub add: TimeAdds,
}

impl Default for TimerDef {
    fn default() -> Self {
        Self { mode: TimerMode::Countdown, seconds: 300.0, label: "Starting soon".to_owned(), add: TimeAdds::default() }
    }
}

/// A text widget's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TextDef {
    /// The main line.
    pub text: String,
    /// A smaller line under it; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
}

impl Default for TextDef {
    fn default() -> Self {
        Self { text: "Be right back".to_owned(), subtitle: String::new() }
    }
}

/// The nine places a widget can hang from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Top left corner.
    TopLeft,
    /// Top edge, centred.
    Top,
    /// Top right corner.
    TopRight,
    /// Left edge, centred.
    Left,
    /// The middle.
    Center,
    /// Right edge, centred.
    Right,
    /// Bottom left corner.
    #[default]
    BottomLeft,
    /// Bottom edge, centred.
    Bottom,
    /// Bottom right corner.
    BottomRight,
}

impl Anchor {
    /// Where on each axis it sits, `0` (left, top) to `1` (right, bottom).
    pub const fn fractions(self) -> (f32, f32) {
        match self {
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::Top => (0.5, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Center => (0.5, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        }
    }
}

/// Where a widget sits: hung from an anchor inside the safe margin, nudged by a share of the frame, as wide as a share
/// of the frame's width. Its height follows its content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Layout {
    /// What it hangs from.
    pub anchor: Anchor,
    /// Moved right by this share of the frame's width (`-1..=1`).
    pub x: f32,
    /// Moved down by this share of the frame's height (`-1..=1`).
    pub y: f32,
    /// Its width as a share of the frame's width (`0.05..=1`).
    pub width: f32,
    /// Its text size against the default (`0.5..=3`).
    pub scale: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Kind::Text.default_layout()
    }
}

impl Layout {
    fn check(&self) -> Result<(), (String, String)> {
        let within = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !within(self.x, -1.0, 1.0) || !within(self.y, -1.0, 1.0) {
            return Err(("layout".to_owned(), "x and y must be -1 to 1".to_owned()));
        }
        if !within(self.width, 0.05, 1.0) {
            return Err(("layout.width".to_owned(), format!("{} is outside 0.05 to 1", self.width)));
        }
        if !within(self.scale, 0.5, 3.0) {
            return Err(("layout.scale".to_owned(), format!("{} is outside 0.5 to 3", self.scale)));
        }
        Ok(())
    }
}

/// A widget's colours, as `#rrggbb`, and how solid its panel is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Style {
    /// Highlights: the bar, the alert's edge, names without a colour of their own.
    pub accent: String,
    /// The text.
    pub text: String,
    /// The panel behind the text.
    pub panel: String,
    /// The panel's opacity, `0..=1`; 0 draws text alone, with an outline to keep it readable.
    pub panel_opacity: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            accent: "#ff6fae".to_owned(),
            text: "#ffffff".to_owned(),
            panel: "#1b1424".to_owned(),
            panel_opacity: 0.82,
        }
    }
}

impl Style {
    fn check(&self) -> Result<(), (String, String)> {
        for (field, value) in [("style.accent", &self.accent), ("style.text", &self.text), ("style.panel", &self.panel)]
        {
            if parse_hex(value).is_none() {
                return Err((field.to_owned(), format!("'{value}' is not a #rrggbb colour")));
            }
        }
        if !(self.panel_opacity.is_finite() && (0.0..=1.0).contains(&self.panel_opacity)) {
            return Err(("style.panel_opacity".to_owned(), "must be 0 to 1".to_owned()));
        }
        Ok(())
    }
}

/// `#rrggbb` (or `rrggbb`) as straight sRGB bytes.
pub fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The program's settings: the safe margin every widget stays inside, the default transition, and how the frame holds
/// its budget.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OverlaySettings {
    /// The margin kept clear at each edge, as a share of the frame's height.
    pub safe: f32,
    /// The transition `program.switch_scene` uses when it names none.
    pub transition: TransitionSpec,
    /// Adaptive quality (`program.set_adaptive`).
    pub adaptive: AdaptiveSpec,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self { safe: 0.04, transition: TransitionSpec::default(), adaptive: AdaptiveSpec::default() }
    }
}

/// Adaptive quality: whether the frame lowers its quality (effects, shadows, render scale, samples) to hold the spec §10
/// budget when something else (a game) takes the GPU, and raises it again when there is room.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AdaptiveSpec {
    /// On or off; unset follows the app (on in the studio).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Whether quality may also change while a take records. Off by default: a recording keeps one look from start to
    /// end, at the quality it started with.
    pub while_recording: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_widget_round_trips_through_the_document_shape() {
        let def = WidgetDef::new(Content::Goal(GoalDef { title: "Bits".into(), ..GoalDef::default() }));
        let json = serde_json::to_value(&def).unwrap();
        assert_eq!(json["kind"], "goal");
        assert_eq!(json["title"], "Bits");
        let back: WidgetDef = serde_json::from_value(json).unwrap();
        assert_eq!(back, def);
    }

    #[test]
    fn defaults_fill_in_what_a_short_definition_leaves_out() {
        let def: WidgetDef =
            serde_json::from_value(serde_json::json!({ "kind": "timer", "mode": "subathon" })).unwrap();
        let Content::Timer(timer) = &def.content else { panic!("a timer") };
        assert_eq!(timer.mode, TimerMode::Subathon);
        assert!((timer.add.sub - 60.0).abs() < 1e-9);
        assert!(def.visible);
        assert_eq!(def.layout, Kind::Timer.default_layout(), "a missing layout is the kind's usual one");
        assert_eq!(def.check(), Ok(()));
    }

    #[test]
    fn checks_name_the_field_at_fault() {
        let mut def = WidgetDef::new(Content::of(Kind::Chat));
        def.style.accent = "pink".into();
        assert_eq!(def.check().unwrap_err().0, "style.accent");
        let mut def = WidgetDef::new(Content::Alerts(AlertsDef { events: vec!["hug".into()], ..AlertsDef::default() }));
        assert_eq!(def.check().unwrap_err().0, "events");
        def.content = Content::of(Kind::Alerts);
        def.layout.width = 2.0;
        assert_eq!(def.check().unwrap_err().0, "layout.width");
    }

    #[test]
    fn scenes_limit_where_a_widget_shows() {
        let mut def = WidgetDef::new(Content::of(Kind::Text));
        assert!(def.shows_in(None) && def.shows_in(Some("desk")));
        def.scenes = vec!["brb".into()];
        assert!(def.shows_in(Some("brb")) && !def.shows_in(Some("desk")) && !def.shows_in(None));
        def.visible = false;
        assert!(!def.shows_in(Some("brb")));
    }

    #[test]
    fn hex_colours_parse_strictly() {
        assert_eq!(parse_hex("#ff6fae"), Some([255, 111, 174]));
        assert_eq!(parse_hex("00ff00"), Some([0, 255, 0]));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gg0000"), None);
    }
}
