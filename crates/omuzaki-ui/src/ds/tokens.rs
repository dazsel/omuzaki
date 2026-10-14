//! Design tokens: colours, sizes, shapes and durations (`design-system.md` sections 2 to 6).
//!
//! Nothing outside this file writes a literal colour or size. Colours are semantic (`bg1`, `text2`, `accent`), one
//! [`Theme`] per palette, and every hex value is final: the contrast test in [`crate::ds::contrast`] recomputes
//! each ratio the spec quotes.

// Colour tokens are written as the 6-digit hex of the spec (`design-system.md` section 2); grouping the digits would
// make them harder to compare against the document.
#![allow(clippy::unreadable_literal)]

use egui::Color32;

/// A colour from a `0xRRGGBB` literal.
#[allow(clippy::cast_possible_truncation, reason = "each channel is masked to eight bits first")]
pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16 & 0xFF) as u8, (rgb >> 8 & 0xFF) as u8, (rgb & 0xFF) as u8)
}

/// `fg` laid over `bg` at `percent` opacity (0 to 100), as an opaque colour.
///
/// Selections and glows are "the accent at 22% over the surface" (section 2.3). Computing the opaque result once keeps
/// them exact (the spec quotes `#443A2C` for a selected gold row, which is alpha 56/255 rounded per channel) and
/// avoids blending in the shader.
#[allow(clippy::cast_possible_truncation, reason = "a weighted mean of two u8 channels fits in u8")]
pub const fn mix(fg: Color32, bg: Color32, percent: u32) -> Color32 {
    const fn channel(f: u8, b: u8, alpha: u32) -> u8 {
        ((b as u32 * (255 - alpha) + f as u32 * alpha + 127) / 255) as u8
    }
    let alpha = (percent * 255 + 50) / 100;
    Color32::from_rgb(channel(fg.r(), bg.r(), alpha), channel(fg.g(), bg.g(), alpha), channel(fg.b(), bg.b(), alpha))
}

/// How much of its opacity a disabled control keeps (`Visuals::disabled_alpha`).
pub const DISABLED_ALPHA: f32 = 0.55;

/// Which palette a [`Theme`] is. Dark is the design baseline (streaming rooms are dim, viewport content is bright).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ThemeKind {
    /// The baseline: warm, quiet, dark.
    Dark,
    /// Warm paper tones for bright rooms.
    Light,
    /// Black and white with 2 pt borders, every status above 7:1 (AAA).
    HighContrast,
}

impl ThemeKind {
    /// Every palette, in the order the gallery and settings list them.
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::HighContrast];

    /// The stable name for file names and saved settings. The name shown to a person is a message
    /// ([`Msg::ThemeDark`](super::strings::Msg::ThemeDark) and its siblings), so it can be translated.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
        }
    }

    /// The egui theme whose style slot we fill: High contrast is a dark palette.
    pub const fn egui_theme(self) -> egui::Theme {
        match self {
            Self::Dark | Self::HighContrast => egui::Theme::Dark,
            Self::Light => egui::Theme::Light,
        }
    }
}

/// What a status means. Colour is never the only signal: every status also has an icon and a word (P9); the word is
/// [`Ds::status_word`](super::theme::Ds::status_word), so it can be translated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Status {
    /// Neutral information: `text2` and an "i" icon (there is no blue; it collided with the agent violet).
    Info,
    /// Passed, connected, done.
    Ok,
    /// Failed gate, degraded, caution.
    Warn,
    /// Errors, destructive, LIVE and recording.
    Danger,
}

/// The focus ring: drawn by us, because egui paints no ring of its own (spike item 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRing {
    /// Ring thickness in points.
    pub width: f32,
    /// Gap between the widget edge and the ring, so a ring never merges with a same-coloured fill.
    pub gap: f32,
    /// The ring colour.
    pub colour: Color32,
    /// A thin outer edge for high contrast, so the ring reads on white as well as black.
    pub edge: Option<Color32>,
}

/// One complete palette. Every field is a final value from `design-system.md` section 2; the "derived" ones are
/// computed from the others by [`mix`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Which palette this is.
    pub kind: ThemeKind,
    /// Window background, viewport surround, scrollbar tracks.
    pub bg0: Color32,
    /// Panels.
    pub bg1: Color32,
    /// Raised: inputs, cards, tab bar, top and status bars.
    pub bg2: Color32,
    /// Hover fill, secondary buttons, code background.
    pub bg3: Color32,
    /// Selected row, pressed fill, slider rails.
    pub bg4: Color32,
    /// Decorative dividers only (not required to meet 3:1).
    pub line: Color32,
    /// The outline of every input, button and checkbox (3:1 on every surface it sits on).
    pub border_control: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text.
    pub text2: Color32,
    /// The dimmest text allowed for information.
    pub text3: Color32,
    /// Disabled text: exempt from WCAG, still legible, always paired with a reason.
    pub text_disabled: Color32,
    /// Focus ring, selection bar, primary button fill, the "modified" dot: "you".
    pub accent: Color32,
    /// The accent used as text or a link.
    pub accent_text: Color32,
    /// Primary button hover.
    pub accent_hover: Color32,
    /// Primary button pressed.
    pub accent_press: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Agents only: chip, highlight, presence, plan "running" ring.
    pub agent: Color32,
    /// Text on an agent fill.
    pub on_agent: Color32,
    /// Passed, connected, done.
    pub ok: Color32,
    /// Failed gate, degraded, caution.
    pub warn: Color32,
    /// Errors, destructive, LIVE and recording.
    pub danger: Color32,
    /// Derived: a selected row, the accent at 22% over `bg1`.
    pub selection: Color32,
    /// Derived: the agent glow behind a touched control, the agent colour at 22% over `bg1`.
    pub agent_glow: Color32,
    /// Width of every control border (1 pt; 2 pt in high contrast).
    pub control_stroke: f32,
    /// The focus ring.
    pub focus: FocusRing,
    /// Width of the selection bar on a tree row (2 pt; 3 pt in high contrast).
    pub selection_bar: f32,
    /// Opacity (0 to 255) of the black popup and window shadow: 35% dark, 18% light, none in high contrast.
    pub shadow_alpha: u8,
    /// How much of the accent a meter fill keeps when its value is near zero, as a percent over the track (`bg4`). The
    /// fill brightens from here to the full accent as the value rises, and this floor is what keeps even the dimmest
    /// fill 3:1 (7:1 in high contrast) against the track: 55 in dark, 100 in light (the accent itself is only 3.4:1 on
    /// `bg4` there, so the ramp is flat) and 88 in high contrast.
    pub meter_floor: u32,
}

impl Theme {
    /// Builds a theme and computes its derived colours.
    const fn build(kind: ThemeKind, p: &Palette, hc: bool) -> Self {
        Self {
            kind,
            bg0: p.bg[0],
            bg1: p.bg[1],
            bg2: p.bg[2],
            bg3: p.bg[3],
            bg4: p.bg[4],
            line: p.line,
            border_control: p.border_control,
            text: p.text[0],
            text2: p.text[1],
            text3: p.text[2],
            text_disabled: p.text[3],
            accent: p.accent[0],
            accent_text: p.accent[1],
            accent_hover: p.accent[2],
            accent_press: p.accent[3],
            on_accent: p.on_accent,
            agent: p.agent,
            on_agent: p.on_agent,
            ok: p.status[0],
            warn: p.status[1],
            danger: p.status[2],
            selection: mix(p.accent[0], p.bg[1], 22),
            agent_glow: mix(p.agent, p.bg[1], 22),
            control_stroke: if hc { 2.0 } else { 1.0 },
            focus: FocusRing {
                width: if hc { 3.0 } else { 2.0 },
                gap: 1.0,
                colour: if hc { p.text[0] } else { p.accent[0] },
                edge: if hc { Some(hex(0x00_0000)) } else { None },
            },
            selection_bar: if hc { 3.0 } else { 2.0 },
            shadow_alpha: p.shadow_alpha,
            meter_floor: p.meter_floor,
        }
    }

    /// The colour of a [`Status`].
    pub const fn status(&self, status: Status) -> Color32 {
        match status {
            Status::Info => self.text2,
            Status::Ok => self.ok,
            Status::Warn => self.warn,
            Status::Danger => self.danger,
        }
    }

    /// A quiet fill for a status chip or banner: the status colour at 10% over `surface`.
    ///
    /// Text in the status colour on this fill still passes 4.5:1 (checked in the contrast test).
    pub const fn status_tint(&self, status: Status, surface: Color32) -> Color32 {
        mix(self.status(status), surface, 10)
    }

    /// The pressed fill of a status-outlined control: the status colour at 16% over `surface`.
    ///
    /// 22% was the first choice, but `danger` text on it is 4.1:1 in light and 4.2:1 in dark; 16% keeps 4.5:1 or more
    /// in all three themes (checked in the contrast test).
    pub const fn status_press(&self, status: Status, surface: Color32) -> Color32 {
        mix(self.status(status), surface, 16)
    }

    /// Whether this palette is a dark one (high contrast counts).
    pub const fn is_dark(&self) -> bool {
        !matches!(self.kind, ThemeKind::Light)
    }

    /// The palette for a kind.
    pub const fn of(kind: ThemeKind) -> &'static Self {
        match kind {
            ThemeKind::Dark => &DARK,
            ThemeKind::Light => &LIGHT,
            ThemeKind::HighContrast => &HIGH_CONTRAST,
        }
    }
}

/// The raw values of a palette, before derived colours.
struct Palette {
    bg: [Color32; 5],
    line: Color32,
    border_control: Color32,
    /// `text`, `text.2`, `text.3`, `text.disabled`.
    text: [Color32; 4],
    /// `accent`, `accent.text`, `accent.hover`, `accent.press`.
    accent: [Color32; 4],
    on_accent: Color32,
    agent: Color32,
    on_agent: Color32,
    /// `ok`, `warn`, `danger`.
    status: [Color32; 3],
    shadow_alpha: u8,
    meter_floor: u32,
}

/// Dark (design baseline), section 2.1 to 2.3.
pub const DARK: Theme = Theme::build(
    ThemeKind::Dark,
    &Palette {
        bg: [hex(0x0E0D11), hex(0x16141A), hex(0x1E1B23), hex(0x28242F), hex(0x342F3D)],
        line: hex(0x3A3542),
        border_control: hex(0x7D7589),
        text: [hex(0xEFE8D8), hex(0xB9B2C0), hex(0xA09AAB), hex(0x6B6575)],
        accent: [hex(0xE6C36A), hex(0xE6C36A), hex(0xF0D285), hex(0xCFAA4E)],
        on_accent: hex(0x1D1508),
        agent: hex(0xB9A2FF),
        on_agent: hex(0x17102B),
        status: [hex(0x5FD7B8), hex(0xF58A1F), hex(0xF25A72)],
        shadow_alpha: 89, // 35%
        meter_floor: 55,
    },
    false,
);

/// Light, section 2.1 to 2.3. `warn` is `#8A3A00`, not the draft's `#A64B00`: a design decision (2026-10-11) moved
/// it to stay distinct from the gold accent for deutan vision (CIE76 gap 5 became 16; see `design-system.md` 2.3).
pub const LIGHT: Theme = Theme::build(
    ThemeKind::Light,
    &Palette {
        bg: [hex(0xE9E3D6), hex(0xF7F3EA), hex(0xFFFFFF), hex(0xEFE9DB), hex(0xE3DBC9)],
        line: hex(0xCFC6B2),
        border_control: hex(0x8A816E),
        text: [hex(0x1F1B24), hex(0x4C4655), hex(0x635C6C), hex(0x8C8596)],
        accent: [hex(0x9A6B00), hex(0x7F5700), hex(0x7F5700), hex(0x664600)],
        on_accent: hex(0xFFFFFF),
        agent: hex(0x5B3FC4),
        on_agent: hex(0xFFFFFF),
        status: [hex(0x0B7259), hex(0x8A3A00), hex(0xB3243A)],
        shadow_alpha: 46, // 18%
        meter_floor: 100,
    },
    false,
);

/// High contrast, section 2.4. The spec fixes surfaces, text, border, focus ring and the five hues; the interaction
/// fills (`bg3`, `bg4`), the dimmer text steps and the divider are ours, chosen to stay above 7:1 against black.
/// `bg4` is `#2A2A2A`, not a lighter grey: `warn` on it is 7.4:1, so a gate warning on a selected row stays AAA.
/// `warn` is `#F6AB40` (design-system 2.4). The first draft's `#FFA040` sat 4.7 (CIEDE2000) from `danger` under tritan
/// vision, and the colour-vision check (`ds::contrast::separations`) asks for 7.
pub const HIGH_CONTRAST: Theme = Theme::build(
    ThemeKind::HighContrast,
    &Palette {
        bg: [hex(0x000000), hex(0x000000), hex(0x000000), hex(0x1A1A1A), hex(0x2A2A2A)],
        line: hex(0x8F8F8F),
        border_control: hex(0xFFFFFF),
        text: [hex(0xFFFFFF), hex(0xE6E6E6), hex(0xCFCFCF), hex(0x8A8A8A)],
        accent: [hex(0xFFD54A), hex(0xFFD54A), hex(0xFFE680), hex(0xE6B800)],
        on_accent: hex(0x000000),
        agent: hex(0xC9B8FF),
        on_agent: hex(0x000000),
        status: [hex(0x7CF5C8), hex(0xF6AB40), hex(0xFF8C9B)],
        shadow_alpha: 0,
        meter_floor: 88,
    },
    true,
);

// ----------------------------------------------------------------------------
// Sizes, shapes and time. All in points (section 4); egui multiplies by the UI scale.

/// The 4 pt grid with a 2 pt half step: 2, 4, 6, 8, 12, 16, 24, 32.
pub mod space {
    /// Half step.
    pub const S2: f32 = 2.0;
    /// One grid unit.
    pub const S4: f32 = 4.0;
    /// 1.5 units.
    pub const S6: f32 = 6.0;
    /// Two units: default item gap.
    pub const S8: f32 = 8.0;
    /// Three units.
    pub const S12: f32 = 12.0;
    /// Four units.
    pub const S16: f32 = 16.0;
    /// Six units.
    pub const S24: f32 = 24.0;
    /// Eight units.
    pub const S32: f32 = 32.0;
}

/// Corner radii (section 4): 4 controls, 6 cards and panels, 8 popups, windows and the palette.
pub mod radius {
    /// Buttons, inputs, tree rows.
    pub const CONTROL: u8 = 4;
    /// Cards and panels.
    pub const CARD: u8 = 6;
    /// Popups, windows, the command palette.
    pub const POPUP: u8 = 8;
    /// Chips and switches: egui clamps this to half the shorter side, so it always makes a pill.
    pub const FULL: u8 = 255;
}

/// Stroke widths (section 4).
pub mod stroke {
    /// Lines and control borders.
    pub const HAIRLINE: f32 = 1.0;
    /// Emphasis: the selected outline.
    pub const EMPHASIS: f32 = 1.5;
}

/// Fixed component sizes (section 4).
pub mod size {
    use egui::{Vec2, vec2};

    /// Panel header.
    pub const PANEL_HEADER: f32 = 28.0;
    /// Top bar.
    pub const TOP_BAR: f32 = 40.0;
    /// Status bar.
    pub const STATUS_BAR: f32 = 24.0;
    /// Dock tab.
    pub const DOCK_TAB: f32 = 28.0;
    /// Section header (an inspector section).
    pub const SECTION_HEADER: f32 = 26.0;
    /// The numeric field of a parameter row, mono.
    pub const NUMERIC_FIELD: Vec2 = vec2(56.0, 22.0);
    /// Share of a parameter row the label takes.
    pub const LABEL_COLUMN: f32 = 0.38;
    /// Narrowest label column.
    pub const LABEL_COLUMN_MIN: f32 = 96.0;
    /// Widest label column.
    pub const LABEL_COLUMN_MAX: f32 = 160.0;
    /// Shortest slider track in a parameter row.
    pub const TRACK_MIN: f32 = 80.0;
    /// The track thickness of a parameter slider.
    pub const TRACK_HEIGHT: f32 = 4.0;
    /// The slider handle diameter (inside a full-height hit row).
    pub const HANDLE: f32 = 14.0;
    /// Switch size.
    pub const SWITCH: Vec2 = vec2(34.0, 18.0);
    /// Checkbox edge.
    pub const CHECKBOX: f32 = 16.0;
    /// Minimum width of a segment.
    pub const SEGMENT_MIN: f32 = 56.0;
    /// Toast width.
    pub const TOAST_WIDTH: f32 = 320.0;
    /// Smallest target anywhere (WCAG 2.5.8).
    pub const MIN_TARGET: f32 = 24.0;
    /// Channel meter bar height.
    pub const METER_HEIGHT: f32 = 10.0;
    /// Colour field swatch.
    pub const SWATCH: Vec2 = vec2(28.0, 20.0);
    /// Icon sizes: inline in text and chips.
    pub const ICON_INLINE: f32 = 14.0;
    /// Icon size: default.
    pub const ICON: f32 = 16.0;
    /// Icon size: toolbars.
    pub const ICON_TOOLBAR: f32 = 20.0;
    /// Icon size: empty states.
    pub const ICON_LARGE: f32 = 24.0;
}

/// Motion durations in seconds (section 6). Reduce motion sets every one to zero.
pub mod dur {
    /// Hover and press colour.
    pub const HOVER: f32 = 0.09;
    /// Section expand and collapse.
    pub const SECTION: f32 = 0.14;
    /// Palette and popup open.
    pub const POPUP: f32 = 0.12;
    /// Toast in.
    pub const TOAST_IN: f32 = 0.14;
    /// Toast out.
    pub const TOAST_OUT: f32 = 0.10;
    /// The agent glow fading after a touch.
    pub const AGENT_GLOW: f32 = 0.9;
}

/// How densely controls are packed. Comfortable is the default; Touch turns on for coarse pointers.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Density {
    /// Default.
    #[default]
    Comfortable,
    /// Fewer pixels per row, for small screens and experts.
    Compact,
    /// 40 pt targets.
    Touch,
}

/// The row sizes that change with [`Density`] (section 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Control row height: buttons, inputs, parameter rows.
    pub control_row: f32,
    /// Tree and list row height.
    pub tree_row: f32,
    /// Icon button hit area (square).
    pub icon_button: f32,
    /// Panel padding.
    pub panel_padding: f32,
}

impl Density {
    /// Every density, in order.
    pub const ALL: [Self; 3] = [Self::Comfortable, Self::Compact, Self::Touch];

    /// The sizes for this density.
    pub const fn metrics(self) -> Metrics {
        match self {
            Self::Comfortable => Metrics { control_row: 28.0, tree_row: 26.0, icon_button: 28.0, panel_padding: 8.0 },
            Self::Compact => Metrics { control_row: 24.0, tree_row: 24.0, icon_button: 24.0, panel_padding: 6.0 },
            Self::Touch => Metrics { control_row: 40.0, tree_row: 40.0, icon_button: 40.0, panel_padding: 12.0 },
        }
    }

    /// The stable name for saved settings; the name shown to a person is a message
    /// ([`Msg::DensityTouch`](super::strings::Msg::DensityTouch) and its siblings).
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Comfortable => "comfortable",
            Self::Compact => "compact",
            Self::Touch => "touch",
        }
    }
}

/// UI scale: the user's multiplier on the OS scale, from the fixed steps of section 3 (WCAG 1.4.4 needs 200%).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Scale(u16);

impl Scale {
    /// The allowed percentages.
    pub const STEPS: [u16; 8] = [75, 90, 100, 110, 125, 150, 175, 200];

    /// 100%.
    pub const NORMAL: Self = Self(100);

    /// The step nearest to `percent`.
    pub fn nearest(percent: u16) -> Self {
        let best = Self::STEPS.iter().copied().min_by_key(|s| s.abs_diff(percent)).unwrap_or(100);
        Self(best)
    }

    /// The percentage.
    pub const fn percent(self) -> u16 {
        self.0
    }

    /// The multiplier on the OS scale, as egui's zoom factor.
    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self::NORMAL
    }
}
