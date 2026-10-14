//! Palette swatches: a colour chip with its slot name, hex code and WCAG contrast ratio against the sheet.

use omuzaki_core::colour::Colour;

use super::canvas::mul255;
use super::{Align, Rect, SheetLayout, TextStyle, Weight};

/// What a palette colour is used for, which decides whether a WCAG level means anything for it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SwatchRole {
    /// Painted as an area (skin, hair, an outfit): the contrast ratio is shown, but no level, because nobody reads
    /// text in it.
    #[default]
    Fill,
    /// Used for text or thin lines (a name colour, an accent): the WCAG level is shown beside the ratio.
    Text,
}

/// One palette slot to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swatch {
    /// The slot's name (`Skin`, `Hair shadow`).
    pub slot: String,
    /// Its colour.
    pub colour: Colour,
    /// What the colour is for.
    pub role: SwatchRole,
}

impl Swatch {
    /// A colour painted as an area: ratio only.
    pub fn fill(slot: impl Into<String>, colour: Colour) -> Self {
        Self { slot: slot.into(), colour, role: SwatchRole::Fill }
    }

    /// A colour used for text: ratio and WCAG level.
    pub fn text(slot: impl Into<String>, colour: Colour) -> Self {
        Self { slot: slot.into(), colour, role: SwatchRole::Text }
    }
}

/// The WCAG 2 level a contrast ratio reaches for text.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WcagGrade {
    /// 7:1 or more.
    Aaa,
    /// 4.5:1 or more.
    Aa,
    /// 3:1 or more: passes AA for large text only (18 pt, or 14 pt bold).
    AaLarge,
    /// Below 3:1.
    Fail,
}

impl WcagGrade {
    /// The grade of `ratio`.
    pub fn of(ratio: f32) -> Self {
        match ratio {
            r if r >= 7.0 => Self::Aaa,
            r if r >= 4.5 => Self::Aa,
            r if r >= 3.0 => Self::AaLarge,
            _ => Self::Fail,
        }
    }

    /// The short name drawn on the sheet.
    pub fn label(self) -> &'static str {
        match self {
            Self::Aaa => "AAA",
            Self::Aa => "AA",
            Self::AaLarge => "AA large",
            Self::Fail => "below AA",
        }
    }
}

/// What [`SheetLayout::swatches`] drew for one swatch.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SwatchReport {
    /// The colour chip.
    pub chip: Rect,
    /// The WCAG 2 contrast ratio of the colour against the sheet background, `1.0..=21.0`.
    pub ratio: f32,
    /// The level that ratio reaches.
    pub grade: WcagGrade,
}

/// The WCAG 2 contrast ratio of `colour` against `background`: `(L1 + 0.05) / (L2 + 0.05)` with `L1` the lighter
/// relative luminance. A translucent colour is first composited over the background, as it appears on the sheet.
///
/// The luminance is the one the palette contrast checker uses ([`Colour::to_linear`], Rec. 709 weights); the sheet
/// computes it here because that checker does not exist yet.
pub fn contrast_ratio(colour: Colour, background: Colour) -> f32 {
    let (a, b) = (luminance(over(colour, background)), luminance(background));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn luminance(c: Colour) -> f32 {
    let l = c.to_linear();
    0.2126 * l.x + 0.7152 * l.y + 0.0722 * l.z
}

/// `top` composited over an opaque `bottom`.
fn over(top: Colour, bottom: Colour) -> Colour {
    let (a, inverse) = (u32::from(top.a), 255 - u32::from(top.a));
    let mix = |t: u8, b: u8| (mul255(u32::from(t), a) + mul255(u32::from(b), inverse)).min(255) as u8;
    Colour::rgb(mix(top.r, bottom.r), mix(top.g, bottom.g), mix(top.b, bottom.b))
}

/// `#RRGGBB` in capitals: how a hex code reads best on a sheet.
fn hex_label(c: Colour) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// A ratio as `4.62:1 vs page`, **truncated** to two decimals (rounding up could show 4.50 for a colour that fails
/// 4.5) and saying what it is measured against.
fn ratio_label(ratio: f32) -> String {
    format!("{:.2}:1 vs page", (ratio * 100.0).floor() / 100.0)
}

/// The type of one swatch card and the heights it is laid out with.
struct CardType {
    name: TextStyle,
    hex: TextStyle,
    ratio: TextStyle,
    /// The WCAG level of a text colour.
    badge: TextStyle,
    /// Distance between the capitals of successive lines.
    pitch: i32,
    /// Space between the chip and the first line.
    pad: i32,
    name_cap: i32,
    hex_cap: i32,
    ratio_cap: i32,
    badge_cap: i32,
}

impl SheetLayout {
    fn card_type(&self) -> CardType {
        let styles = self.styles();
        let name = TextStyle { size_pt: 8.5, weight: Weight::SemiBold, ..styles.caption };
        let hex = TextStyle { size_pt: 8.0, ..styles.value };
        let ratio = TextStyle { size_pt: 7.5, ..styles.small };
        let badge = TextStyle { size_pt: 7.0, weight: Weight::SemiBold, colour: self.theme.muted, ..styles.small };
        let cap = |s: &TextStyle| self.text_metrics(s).cap_height.round() as i32;
        CardType {
            pitch: (self.em_px(&hex) * 1.55).round() as i32,
            pad: (self.em_px(&hex) * 0.8).round() as i32,
            name_cap: cap(&name),
            hex_cap: cap(&hex),
            ratio_cap: cap(&ratio),
            badge_cap: cap(&badge),
            badge,
            name,
            hex,
            ratio,
        }
    }

    /// The height `rows` rows of swatch cards need to show a chip as tall as a line of text is wide-set: lay the
    /// palette's area out with at least this much so the text under the chips is never cut off.
    pub fn swatch_rows_height(&self, rows: usize) -> i32 {
        let t = self.card_type();
        let card = t.pad + 2 * t.pitch + t.name_cap + (self.em_px(&t.name) * 2.4).round() as i32;
        let rows = rows.max(1) as i32;
        rows * card + (rows - 1) * self.metrics.gap
    }

    /// Draws `swatches` in `cols` columns inside `area`, each as a chip with the slot name, the hex code and the
    /// WCAG ratio and level against the sheet background. The chips take the height the text leaves; size the area
    /// with [`swatch_rows_height`](Self::swatch_rows_height) so they are not squeezed away.
    pub fn swatches(&mut self, area: Rect, swatches: &[Swatch], cols: usize) -> Vec<SwatchReport> {
        let cols = cols.max(1);
        let rows = swatches.len().div_ceil(cols).max(1);
        let cells = area.grid(cols, rows, self.metrics.gap, self.metrics.gap);
        let t = self.card_type();
        let text_h = t.pad + 2 * t.pitch + t.name_cap;

        swatches
            .iter()
            .zip(cells)
            .map(|(swatch, card)| {
                let (chip, text) = card.take_bottom(text_h);
                let (hairline, radius) = (self.theme.hairline, self.metrics.radius);
                self.canvas.fill_round_rect(chip, radius, swatch.colour);
                // A hairline edge keeps a pale chip visible on pale paper (and a dark one on a dark board).
                self.canvas.stroke_round_rect(chip, radius, self.metrics.hairline as f32, hairline);

                let ratio = contrast_ratio(swatch.colour, self.theme.background);
                let grade = WcagGrade::of(ratio);
                let (left, right) = (text.x as f32, text.right() as f32);
                let top = text.y + t.pad;
                let name_w = self.text(&swatch.slot, &t.name, (left, top), Align::Left, Some(text.w as f32));
                let hex_top = top + t.pitch + t.name_cap - t.hex_cap;
                let hex_w = self.text(&hex_label(swatch.colour), &t.hex, (left, hex_top), Align::Left, None);
                let ratio_top = top + 2 * t.pitch + t.name_cap - t.ratio_cap;
                self.text(&ratio_label(ratio), &t.ratio, (left, ratio_top), Align::Left, None);
                // The level is for colours used as text. It sits right-aligned on the hex line, or on the name line
                // when the hex code leaves it no room, so it never crowds the ratio's "vs page"; a card too narrow for
                // either goes without.
                if swatch.role == SwatchRole::Text {
                    let level_w = self.text_width(grade.label(), &t.badge) + (t.pitch / 2) as f32;
                    let fits = |used: f32| used + level_w <= text.w as f32;
                    let baseline = if fits(hex_w) {
                        Some(hex_top + t.hex_cap)
                    } else if fits(name_w) {
                        Some(top + t.name_cap)
                    } else {
                        None
                    };
                    if let Some(baseline) = baseline {
                        self.text(grade.label(), &t.badge, (right, baseline - t.badge_cap), Align::Right, None);
                    }
                }
                SwatchReport { chip, ratio, grade }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_on_white_is_21_to_1() {
        assert!((contrast_ratio(Colour::BLACK, Colour::WHITE) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio(Colour::WHITE, Colour::WHITE) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_known_pair_matches_the_wcag_reference() {
        // #767676 on white is the classic "just passes AA" grey: 4.54:1.
        let ratio = contrast_ratio(Colour::hex(0x0076_7676), Colour::WHITE);
        assert!((ratio - 4.54).abs() < 0.01, "{ratio}");
        assert_eq!(WcagGrade::of(ratio), WcagGrade::Aa);
        assert_eq!(WcagGrade::of(4.49), WcagGrade::AaLarge);
        assert_eq!(WcagGrade::of(2.9), WcagGrade::Fail);
    }

    #[test]
    fn translucent_colours_are_measured_as_they_appear() {
        let ratio = contrast_ratio(Colour::rgba(0, 0, 0, 128), Colour::WHITE);
        assert!(ratio > 3.5 && ratio < 4.5, "mid grey over white: {ratio}");
    }

    #[test]
    fn the_ratio_is_truncated_never_rounded_up() {
        assert_eq!(ratio_label(4.499), "4.49:1 vs page", "truncated, and says what it is measured against");
        assert_eq!(ratio_label(21.0), "21.00:1 vs page");
        assert_eq!(hex_label(Colour::hex(0x00f9_d3c5)), "#F9D3C5");
    }
}
