//! `ChannelMeter` and `MeterGrid`: live values of tracking channels (`design-system.md` section 7.6).
//!
//! Each meter shows the processed value as an accent fill, the raw input as a thin tick, the calibrated min and max as
//! brackets, a dead-zone band, a peak-hold tick that decays over 1.5 s, and who owns the channel. Low confidence
//! hatches the fill; clipping turns the end cap into a warning triangle (shape and word, not only colour).
//!
//! A grid is drawn straight with the painter, with **one** hit-test rectangle and the hovered cell found by
//! arithmetic, so 52 meters cost one widget. Values are smoothed and shown at 30 Hz at most whatever the tracking
//! rate; an idle grid (nothing moving) asks for no more frames.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use egui::{
    Align2, Context, Id, Key, Rect, Response, Sense, Stroke, Ui, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::text::NumberBuf;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, mix, radius, size, space};
use crate::register_contrast;

/// The display updates at most this often, whatever the tracking rate.
const DISPLAY_HZ: f64 = 30.0;
/// Seconds for a peak-hold tick to fall from the top to zero.
const PEAK_DECAY_SECS: f64 = 1.5;
const CELL_HEIGHT: f32 = 22.0;

/// Who is driving a channel (PF-23).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Owner {
    /// The face tracker.
    Tracker,
    /// Audio (lip sync from voice).
    Audio,
    /// An AI agent or model: violet, because it is an agent.
    Ai,
    /// The life layer (blinks, breathing, idle).
    LifeLayer,
    /// A hotkey or expression override.
    Hotkey,
    /// A score: an authored timeline, or an AI's intents compiled into cues.
    Score,
    /// Nobody: the channel holds the character's resting expression.
    Rest,
}

impl Owner {
    /// The message that names the owner.
    const fn name(self) -> Msg {
        match self {
            Self::Tracker => Msg::OwnerTracker,
            Self::Audio => Msg::OwnerAudio,
            Self::Ai => Msg::OwnerAi,
            Self::LifeLayer => Msg::OwnerLifeLayer,
            Self::Hotkey => Msg::OwnerHotkey,
            Self::Score => Msg::OwnerScore,
            Self::Rest => Msg::OwnerRest,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Tracker => icons::WEBCAM,
            Self::Audio => icons::WAVEFORM,
            Self::Ai => icons::SPARKLE,
            Self::LifeLayer => icons::HEARTBEAT,
            Self::Hotkey => icons::KEYBOARD,
            Self::Score => icons::CLOCK,
            Self::Rest => icons::CIRCLE_DASHED,
        }
    }
}

/// One channel's current reading. All values are normalised to 0..=1.
#[derive(Clone, Copy, Debug)]
pub struct Channel<'a> {
    /// The channel name ("Jaw open").
    pub name: &'a str,
    /// The value after calibration, smoothing and blending: what the avatar uses.
    pub processed: f32,
    /// The raw input, shown as a thin tick.
    pub raw: f32,
    /// The calibrated minimum and maximum, shown as brackets.
    pub calibrated: Option<(f32, f32)>,
    /// The dead zone, shown as a lighter strip.
    pub dead_zone: Option<(f32, f32)>,
    /// Who drives it.
    pub owner: Owner,
    /// Tracking confidence, 0 to 1; below 0.5 the fill is hatched.
    pub confidence: f32,
    /// The processed value hit the end of its range.
    pub clipping: bool,
}

impl<'a> Channel<'a> {
    /// A channel with a processed value and no extras: raw equals processed, tracker-owned, full confidence.
    pub const fn new(name: &'a str, value: f32) -> Self {
        Self {
            name,
            processed: value,
            raw: value,
            calibrated: None,
            dead_zone: None,
            owner: Owner::Tracker,
            confidence: 1.0,
            clipping: false,
        }
    }
}

/// What the grid keeps between frames: what is shown (smoothed), the peak holds and when it last updated.
#[derive(Default)]
struct Shown {
    values: Vec<f32>,
    peaks: Vec<f32>,
    last_update: f64,
}

/// A grid of meters, one tab stop with arrow-key roving between them.
#[must_use = "a meter grid does nothing until it is shown"]
pub struct MeterGrid<'a> {
    label: &'a str,
    channels: &'a [Channel<'a>],
    columns: usize,
    show_values: bool,
}

/// What the grid reports.
#[derive(Debug)]
pub struct MeterGridOutput {
    /// The single response of the whole grid.
    pub response: Response,
    /// The cell under the pointer.
    pub hovered: Option<usize>,
    /// The cell with keyboard focus.
    pub focused: Option<usize>,
}

impl<'a> MeterGrid<'a> {
    /// A grid named `label` (the accessible name, such as "Face channels") over `channels`.
    pub fn new(label: &'a str, channels: &'a [Channel<'a>]) -> Self {
        a11y::require_label(label);
        Self { label, channels, columns: 1, show_values: false }
    }

    /// How many meters per row.
    pub const fn columns(mut self, columns: usize) -> Self {
        self.columns = if columns == 0 { 1 } else { columns };
        self
    }

    /// Show the number at the end of each bar.
    pub const fn values(mut self, show: bool) -> Self {
        self.show_values = show;
        self
    }

    /// Shows the grid.
    pub fn show(self, ui: &mut Ui) -> MeterGridOutput {
        let ds = ds(ui.ctx());
        let ctx = ui.ctx().clone();
        let columns = self.columns.min(self.channels.len().max(1));
        let rows = self.channels.len().div_ceil(columns);
        let gap = space::S8;
        let width = ui.available_width();
        let cell_w = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let height = rows as f32 * CELL_HEIGHT;
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, height), Sense::focusable_noninteractive() | Sense::hover());
        let id = response.id;

        let shown = self.update_shown(&ctx, id);
        let cell_rect = |index: usize| {
            let (row, col) = (index / columns, index % columns);
            Rect::from_min_size(
                pos2(rect.left() + col as f32 * (cell_w + gap), rect.top() + row as f32 * CELL_HEIGHT),
                vec2(cell_w, CELL_HEIGHT),
            )
        };
        let hovered = response.hover_pos().and_then(|p| {
            let (col, row) =
                (((p.x - rect.left()) / (cell_w + gap)).floor(), ((p.y - rect.top()) / CELL_HEIGHT).floor());
            let (col, row) = (col as isize, row as isize);
            let in_cell = (p.x - rect.left()) - col as f32 * (cell_w + gap) <= cell_w;
            (in_cell && col >= 0 && row >= 0 && (col as usize) < columns)
                .then(|| row as usize * columns + col as usize)
                .filter(|&i| i < self.channels.len())
        });
        let focused = self.keyboard(&ctx, &response, columns);

        if ui.is_rect_visible(rect) {
            let painter = ui.painter().with_clip_rect(rect.expand(4.0));
            let guard = shown.lock().unwrap_or_else(PoisonError::into_inner);
            let name_w = name_width(&ctx, &ds, self.channels, cell_w);
            for (i, channel) in self.channels.iter().enumerate() {
                let cell = cell_rect(i);
                let value = guard.values.get(i).copied().unwrap_or(channel.processed);
                let peak = guard.peaks.get(i).copied().unwrap_or(0.0);
                if hovered == Some(i) || (focused == Some(i) && response.has_focus()) {
                    painter.rect_filled(cell.shrink2(vec2(-2.0, 1.0)), radius::CONTROL, ds.theme.bg3);
                }
                draw_cell(&painter, &ds, cell, name_w, channel, value, peak, self.show_values);
            }
            if let (Some(i), true) = (focused, response.has_focus())
                && crate::ds::focus::visible(&ctx)
            {
                paint::draw_ring(
                    &painter,
                    &ds.theme,
                    cell_rect(i).shrink2(vec2(-2.0, 1.0)),
                    radius::CONTROL,
                    RingPlacement::Inside,
                );
            }
        }

        let mut response = response;
        if let Some(i) = hovered {
            let channel = &self.channels[i];
            let detail = describe(&ds, channel);
            response = response.tip(Tip::new(channel.name).detail(&detail));
        }
        let announced = focused.filter(|_| response.has_focus()).or(hovered).and_then(|i| self.channels.get(i));
        let label = self.label;
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, label),
            |node| {
                node.set_role(accesskit::Role::Group);
                if let Some(channel) = announced {
                    node.set_value(format!("{}: {}", channel.name, describe(&ds, channel)));
                }
            },
        );
        MeterGridOutput { response, hovered, focused }
    }

    /// Smooths the displayed values and decays the peak holds, at most 30 times a second; asks for frames only while
    /// something is still moving.
    fn update_shown(&self, ctx: &Context, id: Id) -> Arc<Mutex<Shown>> {
        let key = id.with("shown");
        let shared = ctx.data(|d| d.get_temp::<Arc<Mutex<Shown>>>(key)).unwrap_or_else(|| {
            let fresh = Arc::new(Mutex::new(Shown::default()));
            ctx.data_mut(|d| d.insert_temp(key, Arc::clone(&fresh)));
            fresh
        });
        let now = ctx.input(|i| i.time);
        let mut moving = false;
        {
            let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
            let first = s.values.len() != self.channels.len();
            if first {
                s.values = self.channels.iter().map(|c| c.processed).collect();
                s.peaks = vec![0.0; self.channels.len()];
                s.last_update = now;
            }
            let dt = now - s.last_update;
            if dt >= 1.0 / DISPLAY_HZ {
                s.last_update = now;
                #[allow(clippy::cast_possible_truncation, reason = "a frame interval in seconds fits f32")]
                let (blend, fall) = (0.55_f32, (dt / PEAK_DECAY_SECS) as f32);
                for (i, channel) in self.channels.iter().enumerate() {
                    let target = channel.processed.clamp(0.0, 1.0);
                    s.values[i] += (target - s.values[i]) * blend;
                    s.peaks[i] = (s.peaks[i] - fall).max(target);
                }
            }
            for (i, channel) in self.channels.iter().enumerate() {
                moving |= (channel.processed - s.values[i]).abs() > 0.002 || s.peaks[i] > channel.processed + 0.01;
            }
        }
        if moving {
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / DISPLAY_HZ));
        }
        shared
    }

    /// Arrow-key roving between meters; returns the focused cell.
    fn keyboard(&self, ctx: &Context, response: &Response, columns: usize) -> Option<usize> {
        let key = response.id.with("cell");
        let mut cell = ctx.data(|d| d.get_temp::<usize>(key)).unwrap_or(0).min(self.channels.len().saturating_sub(1));
        if response.has_focus() {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..egui::EventFilter::default()
                    },
                );
            });
            let last = self.channels.len().saturating_sub(1);
            ctx.input(|i| {
                if i.key_pressed(Key::ArrowRight) {
                    cell = (cell + 1).min(last);
                } else if i.key_pressed(Key::ArrowLeft) {
                    cell = cell.saturating_sub(1);
                } else if i.key_pressed(Key::ArrowDown) {
                    cell = (cell + columns).min(last);
                } else if i.key_pressed(Key::ArrowUp) {
                    cell = cell.saturating_sub(columns);
                } else if i.key_pressed(Key::Home) {
                    cell = 0;
                } else if i.key_pressed(Key::End) {
                    cell = last;
                }
            });
            ctx.data_mut(|d| d.insert_temp(key, cell));
        }
        (!self.channels.is_empty()).then_some(cell)
    }
}

/// The screen-reader announcement and the tooltip detail: "0.52, raw 0.45, owner tracker".
fn describe(ds: &Ds, channel: &Channel<'_>) -> String {
    let mut parts = vec![NumberBuf::new(f64::from(channel.processed), 2).as_str().to_owned()];
    if (channel.raw - channel.processed).abs() > 0.005 {
        let raw = NumberBuf::new(f64::from(channel.raw), 2);
        parts.push(ds.text_with(Msg::MeterRaw, &[("value", raw.as_str())]));
    }
    parts.push(ds.text_with(Msg::MeterOwner, &[("owner", &ds.text(channel.owner.name()))]));
    if channel.clipping {
        parts.push(ds.text(Msg::MeterClipping).into_owned());
    }
    if channel.confidence < 0.5 {
        parts.push(ds.text(Msg::MeterLowConfidence).into_owned());
    }
    parts.join(&ds.text(Msg::ListSeparator))
}

/// The fill colour at `value` (0 to 1): one hue ramp, brighter as the value rises, that never drops below
/// `meter_floor` so even the dimmest fill is 3:1 against the track (`design-system.md` 7.6).
fn fill_colour(t: &Theme, value: f32) -> egui::Color32 {
    let span = (100 - t.meter_floor) as f32;
    mix(t.accent, t.bg4, t.meter_floor + (span * value.clamp(0.0, 1.0)) as u32)
}

/// The width of the name column: the longest name in the list whole, so "Brow outer up L" never reads "Brow outer up…",
/// at least 72 pt and at most half the cell, leaving the bar room. Names are laid out through the galley cache, so this
/// allocates nothing in a steady frame.
fn name_width(ctx: &Context, ds: &Ds, channels: &[Channel<'_>], cell_w: f32) -> f32 {
    let widest = channels.iter().map(|c| paint::text_width(ctx, c.name, &ds.text.small)).fold(0.0, f32::max);
    (widest + space::S6 + 1.0).clamp(72.0, (cell_w * 0.5).max(72.0))
}

#[allow(clippy::too_many_arguments)] // one cell's geometry, its channel and how it is shown
fn draw_cell(
    painter: &egui::Painter,
    ds: &Ds,
    cell: Rect,
    name_w: f32,
    c: &Channel<'_>,
    shown: f32,
    peak: f32,
    show_value: bool,
) {
    let t = &ds.theme;
    let cy = cell.center().y;
    paint::text_in(
        painter,
        Rect::from_min_size(cell.min, vec2(name_w - space::S6, cell.height())),
        Align2::LEFT_CENTER,
        c.name,
        &ds.text.small,
        t.text2,
    );

    let chip = 16.0;
    let value_w = if show_value { 34.0 } else { 0.0 };
    let bar_left = cell.left() + name_w;
    let bar_right = cell.right() - chip - space::S6 - value_w - if show_value { space::S4 } else { 0.0 };
    let cap = if c.clipping { 12.0 } else { 0.0 };
    let bar = Rect::from_min_max(
        pos2(bar_left, cy - size::METER_HEIGHT / 2.0),
        pos2((bar_right - cap).max(bar_left + 20.0), cy + size::METER_HEIGHT / 2.0),
    );
    let x = |v: f32| bar.left() + v.clamp(0.0, 1.0) * bar.width();

    painter.rect_filled(bar, 2, t.bg4);
    if let Some((a, b)) = c.dead_zone {
        painter.rect_filled(
            Rect::from_min_max(pos2(x(a), bar.top()), pos2(x(b), bar.bottom())),
            0,
            mix(t.text3, t.bg4, 35),
        );
    }
    // One hue ramp (accent, never green to red): brighter as the value rises.
    let value = shown.clamp(0.0, 1.0);
    if value > 0.002 {
        let fill = Rect::from_min_max(bar.min, pos2(x(value), bar.bottom()));
        let colour = fill_colour(t, value);
        painter.rect_filled(fill, 2, colour);
        if c.confidence < 0.5 {
            hatch(painter, fill, t.bg4);
        }
    }
    if let Some((lo, hi)) = c.calibrated {
        for edge in [lo, hi] {
            painter.rect_filled(
                Rect::from_center_size(pos2(x(edge), cy), vec2(1.5, size::METER_HEIGHT + 6.0)),
                0,
                t.text3,
            );
        }
    }
    if peak > value + 0.02 {
        painter.rect_filled(Rect::from_center_size(pos2(x(peak), cy), vec2(2.0, size::METER_HEIGHT)), 0, t.text2);
    }
    // The raw tick has a halo in the window colour, so it reads over the accent fill as well as over the track.
    let raw = pos2(x(c.raw), cy);
    painter.rect_filled(Rect::from_center_size(raw, vec2(4.0, size::METER_HEIGHT + 6.0)), 1, t.bg0);
    painter.rect_filled(Rect::from_center_size(raw, vec2(2.0, size::METER_HEIGHT + 4.0)), 1, t.text);
    if c.clipping {
        paint::icon(painter, ds, pos2(bar.right() + 8.0, cy), icons::WARNING, 12.0, t.warn, true);
    }
    if show_value {
        let number = NumberBuf::new(f64::from(c.processed), 2);
        paint::text(
            painter,
            pos2(cell.right() - chip - space::S6, cy),
            Align2::RIGHT_CENTER,
            number.as_str(),
            &egui::FontId::monospace(11.0),
            t.text2,
        );
    }
    let owner_ink = if c.owner == Owner::Ai { t.agent } else { t.text3 };
    paint::icon(painter, ds, pos2(cell.right() - chip / 2.0, cy), c.owner.glyph(), 13.0, owner_ink, false);
}

/// Diagonal hatching over a rectangle, in the colour of the track behind, so a low-confidence fill reads as striped.
pub(super) fn hatch(painter: &egui::Painter, rect: Rect, colour: egui::Color32) {
    let painter = painter.with_clip_rect(rect);
    let stroke = Stroke::new(1.5, colour);
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        painter.line_segment([pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())], stroke);
        x += 5.0;
    }
}

/// A single labelled meter: a grid of one.
#[must_use = "a meter does nothing until it is shown"]
pub struct ChannelMeter<'a> {
    channel: Channel<'a>,
}

impl<'a> ChannelMeter<'a> {
    /// A meter for one channel.
    pub const fn new(channel: Channel<'a>) -> Self {
        Self { channel }
    }

    /// Shows the meter at full width.
    pub fn show(&self, ui: &mut Ui) -> MeterGridOutput {
        let channels = [self.channel];
        MeterGrid::new(self.channel.name, &channels).values(true).show(ui)
    }
}

register_contrast!("channel_meter", pairs);

/// The fill ramp and the ticks against the track and the fill they are drawn over.
fn pairs(table: &mut Table<'_>) {
    let t = *table.theme();
    let (dim, bright) = (fill_colour(&t, 0.0), fill_colour(&t, 1.0));
    table.add("dimmest fill on the track", dim, t.bg4, Rule::NonText);
    table.add("brightest fill on the track", bright, t.bg4, Rule::NonText);
    table.add("calibration bracket on the track", t.text3, t.bg4, Rule::NonText);
    table.add("peak tick on the track", t.text2, t.bg4, Rule::NonText);
    // The raw tick is `text` on a halo of `bg0`, so it reads over the track and over any fill.
    table.add("raw tick on its halo", t.text, t.bg0, Rule::NonText);
    table.add("raw tick halo on the dimmest fill", t.bg0, dim, Rule::NonText);
    table.add("raw tick halo on the brightest fill", t.bg0, bright, Rule::NonText);
    table.add("raw tick on the track", t.text, t.bg4, Rule::NonText);
}
