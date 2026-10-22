//! Installing a theme into egui, and reading it back from widgets (`design-system.md` sections 3, 4 and 8).
//!
//! [`apply`] builds the whole `egui::Style` once, from the tokens, and stores a [`Ds`] (the theme, the metrics and
//! the font ids) in the egui `Context`. Widgets read their tokens from that `Ds` through [`ds`]: there is no process
//! global, so two contexts in one test binary can use two themes at once.

use std::sync::Arc;

use egui::style::{HandleShape, Selection, TextCursorStyle, WidgetVisuals, Widgets};
use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Id, Margin, Shadow, Stroke, Style, TextStyle, Vec2};

use std::borrow::Cow;

use super::fonts;
use super::strings::{self, English, Msg, Strings};
use super::tokens::{DISABLED_ALPHA, Density, Metrics, Scale, Status, Theme, ThemeKind, dur, radius};

/// Everything the user can choose about how the UI looks. Saved with the app's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// The palette.
    pub theme: ThemeKind,
    /// UI scale, 75% to 200%.
    pub scale: Scale,
    /// Row sizes.
    pub density: Density,
    /// Zero every duration and replace pulses with static markers (WCAG 2.3.3, 2.2.2).
    pub reduce_motion: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: ThemeKind::Dark, scale: Scale::NORMAL, density: Density::Comfortable, reduce_motion: false }
    }
}

/// The font ids of the type scale (section 3), cloned out of here by widgets (cloning a `FontId` allocates nothing).
#[derive(Clone, Debug)]
pub struct TypeScale {
    /// 28 pt, `Bricolage Grotesque` `SemiBold`: splash, empty-state headline, first-run titles.
    pub display: FontId,
    /// 18 pt, `Instrument Sans` `SemiBold`: dialog and workspace titles.
    pub title: FontId,
    /// 14 pt, `Instrument Sans` `SemiBold`: section and panel headers.
    pub section: FontId,
    /// 13 pt, Instrument Sans Regular: controls, labels, tree rows.
    pub body: FontId,
    /// 13 pt, Instrument Sans Medium: selected rows, button labels.
    pub strong: FontId,
    /// 12 pt, Instrument Sans Regular: secondary text, tooltips, chips.
    pub small: FontId,
    /// 11 pt, Instrument Sans Medium: badges and units only.
    pub micro: FontId,
    /// 12.5 pt, `JetBrains Mono`: numbers, op names, ids, logs, key caps.
    pub mono: FontId,
    /// The Phosphor Regular and Fill families, built once: `FontFamily::Name` holds an `Arc<str>`, so making one per
    /// icon drawn would allocate every frame.
    icon_regular: FontFamily,
    icon_fill: FontFamily,
    fonts_active: bool,
}

impl TypeScale {
    /// The scale. While the font families are not yet active (the first frame; egui installs fonts at the start of
    /// the next pass) every face falls back to the built-in families, because egui panics on an unknown one.
    fn new(fonts_active: bool) -> Self {
        let named = |name: &str, size: f32| {
            if fonts_active { FontId::new(size, FontFamily::Name(name.into())) } else { FontId::proportional(size) }
        };
        Self {
            display: named(fonts::DISPLAY, 28.0),
            title: named(fonts::SANS_SEMIBOLD, 18.0),
            section: named(fonts::SANS_SEMIBOLD, 14.0),
            body: FontId::proportional(13.0),
            strong: named(fonts::SANS_MEDIUM, 13.0),
            small: FontId::proportional(12.0),
            micro: named(fonts::SANS_MEDIUM, 11.0),
            mono: FontId::monospace(12.5),
            icon_regular: if fonts_active { FontFamily::Name(fonts::ICONS.into()) } else { FontFamily::Proportional },
            icon_fill: if fonts_active { FontFamily::Name(fonts::ICONS_FILL.into()) } else { FontFamily::Proportional },
            fonts_active,
        }
    }

    pub(crate) fn fonts_active(&self) -> bool {
        self.fonts_active
    }

    /// A Phosphor icon font at `size` pt: Regular, or Fill for "on" states. Allocation-free.
    pub fn icon(&self, size: f32, filled: bool) -> FontId {
        FontId::new(size, if filled { self.icon_fill.clone() } else { self.icon_regular.clone() })
    }
}

/// Everything a widget needs to draw itself, set by [`apply`].
#[derive(Clone, Debug)]
pub struct Ds {
    /// The palette.
    pub theme: Theme,
    /// Row sizes for the current density.
    pub metrics: Metrics,
    /// The settings this was built from.
    pub settings: Settings,
    /// The type scale.
    pub text: TypeScale,
    /// The words components say on their own, in the language the app chose (see [`strings`](super::strings)).
    pub strings: Arc<dyn Strings>,
}

impl Ds {
    /// A duration in seconds, zero under reduce motion.
    pub fn motion(&self, seconds: f32) -> f32 {
        if self.settings.reduce_motion { 0.0 } else { seconds }
    }

    /// The text of a message that has no placeholders.
    pub fn text(&self, msg: Msg) -> Cow<'_, str> {
        self.strings.pattern(msg)
    }

    /// The text of a message with its `{name}` placeholders filled from `args`.
    pub fn text_with(&self, msg: Msg, args: &[(&str, &str)]) -> String {
        strings::fill(&self.strings.pattern(msg), args)
    }

    /// The word that goes with a status icon ("Warning"): what a screen reader says where colour would be the cue.
    pub fn status_word(&self, status: Status) -> Cow<'_, str> {
        self.text(match status {
            Status::Info => Msg::StatusInfo,
            Status::Ok => Msg::StatusOk,
            Status::Warn => Msg::StatusWarning,
            Status::Danger => Msg::StatusError,
        })
    }
}

fn state_id() -> Id {
    Id::new("omuzaki_ui::ds::state")
}

/// The pass in which the fonts were handed to egui (they become active in a later pass).
fn fonts_id() -> Id {
    Id::new("omuzaki_ui::ds::fonts")
}

/// Whether the bundled fonts are active yet: egui applies them at the start of the pass after the one that set them.
fn fonts_active(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(fonts_id())).is_some_and(|installed_in| ctx.cumulative_pass_nr() > installed_in)
}

/// Applies the theme with default motion: the call the spec names, `apply(ctx, &Theme, Scale, Density)`.
pub fn apply(ctx: &Context, theme: &Theme, scale: Scale, density: Density) {
    apply_settings(ctx, Settings { theme: theme.kind, scale, density, reduce_motion: false });
}

/// Applies all the appearance settings. Cheap when nothing changed, so it is fine to call every frame.
///
/// It may be called before the first frame or during any frame. The fonts it installs become active one pass later;
/// until then text uses egui's built-in faces, and [`ds`] upgrades itself as soon as the real ones are available.
pub fn apply_settings(ctx: &Context, settings: Settings) {
    if ctx.data(|d| d.get_temp::<u64>(fonts_id())).is_none() {
        ctx.set_fonts(fonts::definitions());
        // Read the pass number first: calling a `Context` method inside `data_mut` would deadlock on its lock.
        let installed_in = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(fonts_id(), installed_in));
    }
    let active = fonts_active(ctx);
    let current = ctx.data(|d| d.get_temp::<Arc<Ds>>(state_id()));
    if current.as_ref().is_some_and(|ds| ds.settings == settings && ds.text.fonts_active() == active) {
        return;
    }
    if !active {
        ctx.request_repaint();
    }
    let strings = current.map_or_else(|| Arc::new(English) as Arc<dyn Strings>, |ds| Arc::clone(&ds.strings));
    let ds = Ds {
        theme: *Theme::of(settings.theme),
        metrics: settings.density.metrics(),
        settings,
        text: TypeScale::new(active),
        strings,
    };
    ctx.set_theme(ds.theme.kind.egui_theme());
    ctx.set_style_of(ds.theme.kind.egui_theme(), build_style(&ds));
    ctx.set_zoom_factor(settings.scale.factor());
    ctx.data_mut(|d| d.insert_temp(state_id(), Arc::new(ds)));
}

/// The design system state of this context. The first call applies the default settings (dark, 100%, comfortable) if
/// the app has not called [`apply`], so a widget works in any test or window; it also swaps in the real fonts once
/// egui has activated them.
pub fn ds(ctx: &Context) -> Arc<Ds> {
    let current = ctx.data(|d| d.get_temp::<Arc<Ds>>(state_id()));
    match current {
        Some(ds) if ds.text.fonts_active() || !fonts_active(ctx) => ds,
        other => {
            apply_settings(ctx, other.map_or_else(Settings::default, |ds| ds.settings));
            // `apply_settings` always stores a state, so this lookup cannot miss; the default is for the type system.
            ctx.data(|d| d.get_temp::<Arc<Ds>>(state_id())).unwrap_or_else(|| Arc::new(Ds::fallback()))
        }
    }
}

/// Switches the language the components speak. Takes effect on the next frame and needs no restart.
pub fn set_strings(ctx: &Context, strings: Arc<dyn Strings>) {
    let current = ds(ctx);
    ctx.data_mut(|d| d.insert_temp(state_id(), Arc::new(Ds { strings, ..(*current).clone() })));
    ctx.request_repaint();
}

impl Ds {
    /// Never used in practice (see [`ds`]); keeps that function free of `unwrap`.
    fn fallback() -> Self {
        let settings = Settings::default();
        Self {
            theme: *Theme::of(settings.theme),
            metrics: settings.density.metrics(),
            settings,
            text: TypeScale::new(false),
            strings: Arc::new(English),
        }
    }
}

fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(width, colour)
}

/// The corner radius of controls.
pub fn control_radius() -> CornerRadius {
    CornerRadius::same(radius::CONTROL)
}

fn widget_visuals(fill: Color32, weak_fill: Color32, border: Stroke, text: Color32) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: weak_fill,
        bg_stroke: border,
        corner_radius: control_radius(),
        // Width 1 on every state: egui draws text weight from this, and a wider stroke on hover would jitter.
        fg_stroke: stroke(1.0, text),
        expansion: 0.0,
    }
}

fn visuals(ds: &Ds) -> egui::Visuals {
    let t = &ds.theme;
    let border = stroke(t.control_stroke, t.border_control);
    let shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(t.shadow_alpha) };
    let mut v = if t.is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };

    v.widgets = Widgets {
        // Labels, separators, frames.
        noninteractive: widget_visuals(t.bg1, t.bg1, stroke(1.0, t.line), t.text),
        // Buttons rest on `bg3` with the control border; checkboxes and sliders use `bg_fill`.
        inactive: widget_visuals(t.bg2, t.bg3, border, t.text),
        hovered: widget_visuals(t.bg3, t.bg4, border, t.text),
        active: widget_visuals(t.bg4, t.bg4, border, t.text),
        open: widget_visuals(t.bg3, t.bg3, border, t.text),
    };
    v.override_text_color = None;
    v.weak_text_color = Some(t.text3);
    v.selection = Selection { bg_fill: t.selection, stroke: stroke(2.0, t.accent_text) };
    v.hyperlink_color = t.accent_text;
    v.faint_bg_color = t.bg2;
    v.extreme_bg_color = t.bg0;
    v.text_edit_bg_color = Some(t.bg2);
    v.code_bg_color = t.bg3;
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.danger;
    v.window_fill = t.bg2;
    v.window_stroke = stroke(t.control_stroke.max(1.0), if t.control_stroke > 1.0 { t.border_control } else { t.line });
    v.window_corner_radius = CornerRadius::same(radius::POPUP);
    v.menu_corner_radius = CornerRadius::same(radius::CARD);
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.panel_fill = t.bg1;
    v.text_cursor = TextCursorStyle { stroke: stroke(2.0, t.accent), ..TextCursorStyle::default() };
    v.slider_trailing_fill = false;
    v.handle_shape = HandleShape::Circle;
    v.striped = false;
    v.interact_cursor = None;
    v.disabled_alpha = DISABLED_ALPHA;
    v
}

fn build_style(ds: &Ds) -> Style {
    let t = &ds.text;
    let m = &ds.metrics;
    let mut style = Style { visuals: visuals(ds), ..Style::default() };

    style.text_styles = [
        (TextStyle::Heading, t.title.clone()),
        (TextStyle::Name("display".into()), t.display.clone()),
        (TextStyle::Name("section".into()), t.section.clone()),
        (TextStyle::Body, t.body.clone()),
        (TextStyle::Button, t.strong.clone()),
        (TextStyle::Small, t.small.clone()),
        (TextStyle::Name("micro".into()), t.micro.clone()),
        (TextStyle::Monospace, t.mono.clone()),
    ]
    .into();
    style.drag_value_text_style = TextStyle::Monospace;
    style.animation_time = ds.motion(dur::SECTION);
    style.explanation_tooltips = false;
    style.interaction.tooltip_delay = 0.5;
    style.interaction.selectable_labels = false;

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(8.0, 4.0);
    s.button_padding = Vec2::new(10.0, 4.0);
    s.window_margin = Margin::same(12);
    s.menu_margin = Margin::same(6);
    s.indent = 16.0;
    s.interact_size = Vec2::new(m.control_row, m.control_row);
    s.icon_width = 16.0;
    s.icon_width_inner = 10.0;
    s.icon_spacing = 6.0;
    s.text_edit_width = 160.0;
    s.combo_width = 140.0;
    s.tooltip_width = 320.0;
    s.slider_rail_height = super::tokens::size::TRACK_HEIGHT;
    s.slider_width = 160.0;
    s.scroll = egui::style::ScrollStyle { bar_width: 8.0, ..egui::style::ScrollStyle::solid() };
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ds::tokens::{DARK, HIGH_CONTRAST, LIGHT};

    fn run_frame(ctx: &Context) {
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Width");
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn apply_installs_the_style_and_a_frame_lays_out_every_text_style() {
        for theme in [&DARK, &LIGHT, &HIGH_CONTRAST] {
            let ctx = Context::default();
            apply(&ctx, theme, Scale::NORMAL, Density::Comfortable);
            // A first pass activates the fonts; the second proves every named family resolves.
            run_frame(&ctx);
            ctx.run_ui(egui::RawInput::default(), |ui| {
                for style in ctx.global_style().text_styles() {
                    ui.label(egui::RichText::new("Aa 0.62").text_style(style));
                }
            })
            .drop_without_applying_deltas();
            assert_eq!(ds(&ctx).theme.kind, theme.kind);
            assert_eq!(ctx.global_style().visuals.panel_fill, theme.bg1);
        }
    }

    #[test]
    fn the_very_first_frame_needs_no_setup_and_the_second_has_the_real_fonts() {
        let ctx = Context::default();
        let text_ready = |ctx: &Context| {
            let mut ready = false;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                let d = ds(ui.ctx());
                ui.label(egui::RichText::new("Width").font(d.text.strong.clone()));
                ui.label(egui::RichText::new(crate::ds::icons::EYE).font(d.text.icon(16.0, true)));
                ready = d.text.fonts_active();
            })
            .drop_without_applying_deltas();
            ready
        };
        assert!(!text_ready(&ctx), "egui activates fonts one pass after they are set");
        assert!(text_ready(&ctx), "so the second frame uses the bundled families");
    }

    #[test]
    fn applying_the_same_settings_twice_changes_nothing() {
        let ctx = Context::default();
        apply(&ctx, &DARK, Scale::NORMAL, Density::Comfortable);
        run_frame(&ctx);
        run_frame(&ctx);
        let first = ds(&ctx);
        apply(&ctx, &DARK, Scale::NORMAL, Density::Comfortable);
        assert!(Arc::ptr_eq(&first, &ds(&ctx)), "an idempotent apply must not rebuild the style");
        apply(&ctx, &LIGHT, Scale::NORMAL, Density::Comfortable);
        assert!(!Arc::ptr_eq(&first, &ds(&ctx)));
    }

    #[test]
    fn reduce_motion_zeroes_every_duration() {
        let ctx = Context::default();
        apply_settings(&ctx, Settings { reduce_motion: true, ..Settings::default() });
        let d = ds(&ctx);
        assert!(d.motion(dur::HOVER).abs() < f32::EPSILON);
        assert!(ctx.global_style().animation_time.abs() < f32::EPSILON);
    }

    #[test]
    fn scale_steps_snap_to_the_allowed_list() {
        assert_eq!(Scale::nearest(103).percent(), 100);
        assert_eq!(Scale::nearest(400).percent(), 200);
        assert_eq!(Scale::nearest(1).percent(), 75);
        assert!((Scale::nearest(150).factor() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn density_rows_match_the_spec_table() {
        let c = Density::Comfortable.metrics();
        assert!((c.control_row - 28.0).abs() < f32::EPSILON && (c.tree_row - 26.0).abs() < f32::EPSILON);
        assert!((Density::Compact.metrics().control_row - 24.0).abs() < f32::EPSILON);
        assert!((Density::Touch.metrics().icon_button - 40.0).abs() < f32::EPSILON);
    }
}
