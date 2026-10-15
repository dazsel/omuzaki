//! Audits of a rendered frame: every piece of painted text, the colour it was drawn in and the colour behind it.
//!
//! The hand-written pairs in [`crate::ds::contrast::pairs`] only cover what somebody remembered to list. This reads
//! the frame egui actually produced instead: for each text shape it finds the filled shapes painted under it,
//! composites them in paint order and measures the ratio, so a new component that draws an unlisted pair is caught
//! the first time the gallery shows it. The same walk lists the strings that were painted, which is how the
//! pseudo-locale test proves no English is hard-coded in a component.
//!
//! Available to this crate's tests and, with the `capture` feature, to the panels' tests.

use egui::accesskit::Role;
use egui::epaint::ClippedShape;
use egui::{Color32, FontFamily, FullOutput, Pos2, Rect, Shape};

use super::contrast::{Rule, contrast_ratio, threshold};
use super::fonts;
use super::tokens::Theme;

/// One run of text as painted.
#[derive(Clone, Debug)]
pub struct Painted {
    /// The text, whole (an elided label reports what it was asked to say).
    pub text: String,
    /// Where the galley sits on screen.
    pub rect: Rect,
    /// The colour of the glyphs, with its opacity.
    pub ink: Color32,
    /// An icon glyph (Phosphor): held to the non-text threshold, not the text one.
    pub icon: bool,
    /// The opaque colour behind the text's centre after compositing every fill painted under it; `None` when
    /// nothing was painted there (the frame's clear colour).
    pub backdrop: Option<Color32>,
    /// The text was cut with an ellipsis because it did not fit.
    pub elided: bool,
}

/// Flattens nested shapes into paint order.
fn flatten<'a>(shape: &'a Shape, out: &mut Vec<&'a Shape>) {
    match shape {
        Shape::Vec(inner) => inner.iter().for_each(|s| flatten(s, out)),
        other => out.push(other),
    }
}

/// `top` laid over `under`, blended in gamma space the way egui's renderer does. `top` is premultiplied.
fn over(top: Color32, under: Color32) -> Color32 {
    let keep = 1.0 - f32::from(top.a()) / 255.0;
    let mix = |t: u8, u: u8| (f32::from(t) + f32::from(u) * keep).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(mix(top.r(), under.r()), mix(top.g(), under.g()), mix(top.b(), under.b()))
}

/// The fill and area of a shape that can sit behind text, if it is one.
fn backing(shape: &Shape) -> Option<(Rect, Color32)> {
    match shape {
        Shape::Rect(r) if r.fill.a() > 0 && r.blur_width <= 0.0 => Some((r.rect, r.fill)),
        Shape::Circle(c) if c.fill.a() > 0 => {
            Some((Rect::from_center_size(c.center, egui::vec2(c.radius, c.radius) * 2.0), c.fill))
        }
        _ => None,
    }
}

/// What is behind `at` just before shape number `upto` is painted.
fn backdrop(shapes: &[&Shape], upto: usize, at: Pos2) -> Option<Color32> {
    let covering: Vec<Color32> = shapes[..upto]
        .iter()
        .filter_map(|s| backing(s))
        .filter(|(rect, _)| rect.contains(at))
        .map(|(_, fill)| fill)
        .collect();
    // Everything under the last opaque fill is hidden by it.
    let first = covering.iter().rposition(|c| c.a() == 255)?;
    Some(covering[first + 1..].iter().fold(covering[first], |under, top| over(*top, under)))
}

/// Every text shape in the frame, in paint order, with its colours.
pub fn painted(output: &FullOutput) -> Vec<Painted> {
    let mut shapes: Vec<&Shape> = Vec::new();
    for ClippedShape { shape, .. } in &output.shapes {
        flatten(shape, &mut shapes);
    }
    let mut out = Vec::new();
    for (index, shape) in shapes.iter().enumerate() {
        let Shape::Text(text) = shape else { continue };
        if text.galley.job.text.trim().is_empty() {
            continue;
        }
        let rect = text.galley.rect.translate(text.pos.to_vec2());
        let Some(section) = text.galley.job.sections.first() else { continue };
        let base = text.override_text_color.unwrap_or(if section.format.color == Color32::PLACEHOLDER {
            text.fallback_color
        } else {
            section.format.color
        });
        let family = &section.format.font_id.family;
        let icon = matches!(family, FontFamily::Name(name) if &**name == fonts::ICONS || &**name == fonts::ICONS_FILL);
        out.push(Painted {
            text: text.galley.job.text.clone(),
            rect,
            ink: base.gamma_multiply(text.opacity_factor),
            icon,
            backdrop: backdrop(&shapes, index, rect.center()),
            elided: text.galley.elided,
        });
    }
    out
}

/// A painted text that misses its contrast threshold.
#[derive(Clone, Debug)]
pub struct Finding {
    /// The text.
    pub text: String,
    /// Where it is.
    pub rect: Rect,
    /// The measured ratio.
    pub ratio: f64,
    /// What it needed.
    pub needed: f64,
    /// The glyph colour as drawn (over the backdrop).
    pub ink: Color32,
    /// The backdrop.
    pub backdrop: Color32,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} at ({:.0}, {:.0}): {:.2}:1 needs {:.1}:1 (ink #{:02X}{:02X}{:02X} on #{:02X}{:02X}{:02X})",
            self.text.chars().take(32).collect::<String>(),
            self.rect.left(),
            self.rect.top(),
            self.ratio,
            self.needed,
            self.ink.r(),
            self.ink.g(),
            self.ink.b(),
            self.backdrop.r(),
            self.backdrop.g(),
            self.backdrop.b()
        )
    }
}

/// Every painted text whose contrast against what is behind it is below `theme`'s threshold: 4.5:1 for text (7:1 in
/// high contrast), 3:1 for icons, and 3:1 for faded text, which is how a disabled control is drawn.
pub fn contrast_findings(output: &FullOutput, theme: &Theme) -> Vec<Finding> {
    painted(output)
        .into_iter()
        .filter_map(|p| {
            let backdrop = p.backdrop?;
            let ink = over(p.ink, backdrop);
            let faded = p.ink.a() < 250 || p.ink == theme.text_disabled;
            let rule = if faded {
                Rule::Disabled
            } else if p.icon {
                Rule::NonText
            } else {
                Rule::Text
            };
            let needed = threshold(theme, rule);
            let ratio = contrast_ratio(ink, backdrop);
            (ratio < needed).then_some(Finding { text: p.text, rect: p.rect, ratio, needed, ink, backdrop })
        })
        .collect()
}

/// The rectangles of everything drawn as a keyboard focus indicator this frame: a stroked rectangle at least as thick
/// as the focus ring (2 pt) in the ring colour or the accent, which is the ring itself and the 2 pt accent border a
/// focused text field draws instead. A control inside a focused widget (a slider's handle) draws its ring around the
/// handle, not around the whole widget, so the answer is a list of areas, not a yes or no.
pub fn focus_indicators(output: &FullOutput, theme: &Theme) -> Vec<Rect> {
    let mut shapes: Vec<&Shape> = Vec::new();
    for ClippedShape { shape, .. } in &output.shapes {
        flatten(shape, &mut shapes);
    }
    shapes
        .into_iter()
        .filter_map(|shape| match shape {
            Shape::Rect(r)
                if r.fill.a() == 0
                    && r.stroke.width >= 2.0
                    && (r.stroke.color == theme.focus.colour || r.stroke.color == theme.accent) =>
            {
                Some(r.rect)
            }
            _ => None,
        })
        .collect()
}

/// Scope ids that the app shows as they are: they name what an op may touch and are never translated (SE-03: ids and
/// tool names stay as written). The gallery draws a few of them in its scope badges.
const IDENTIFIER_WORDS: &[&str] = &["read", "render", "broadcast", "publish", "spend", "native", "input", "process"];

/// The strings, among `texts`, that read as plain English: ASCII only, with a word of three letters or more that has
/// a lowercase letter in it.
///
/// This is the pseudo-locale check. In that locale every message and every gallery string is accented, so a string
/// that comes through as plain ASCII words was written into the code instead of the message table. Not counted as
/// prose: identifiers (`hair.root`, `edit:character`, `#8A6FD1`: no spaces and a dot, colon, hash, slash or
/// underscore), the scope ids above, the key legends the table marks as verbatim (`Ctrl`, `Enter`), and all-capital
/// abbreviations and units (`GPU`, `GB`).
pub fn plain_english<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let verbatim: Vec<&str> =
        super::strings::Msg::ALL.iter().filter(|m| m.is_verbatim()).flat_map(|m| m.english().split(' ')).collect();
    let mut found: Vec<String> = Vec::new();
    for text in texts {
        let text = text.trim();
        let identifier = !text.contains(char::is_whitespace) && text.contains(['.', ':', '#', '/', '_']);
        if text.is_empty() || !text.is_ascii() || identifier {
            continue;
        }
        let prose = text.split(|c: char| !c.is_ascii_alphabetic()).any(|word| {
            word.len() >= 3
                && word.bytes().any(|b| b.is_ascii_lowercase())
                && !IDENTIFIER_WORDS.contains(&word)
                && !verbatim.contains(&word)
        });
        if prose && !found.iter().any(|f| f == text) {
            found.push(text.to_owned());
        }
    }
    found
}

/// What is wrong with a piece of a laid-out frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutProblem {
    /// Text runs past the clip rectangle it is painted in, so the part beyond the edge is never seen.
    Clipped,
    /// Text or a control lies outside the window: a person cannot see it or reach it without the window scrolling.
    OutsideWindow,
    /// Two pieces of text, or two controls, are drawn over each other (neither wholly inside the other).
    Overlap,
}

/// One layout problem: what it is, the text or control concerned, and where it is.
#[derive(Clone, Debug)]
pub struct LayoutFinding {
    /// What is wrong.
    pub problem: LayoutProblem,
    /// The text, or the control's name, as far as it is known.
    pub what: String,
    /// Where it is on screen.
    pub rect: Rect,
    /// For an overlap, the other piece it is drawn over.
    pub with: Option<String>,
}

impl std::fmt::Display for LayoutFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let problem = match self.problem {
            LayoutProblem::Clipped => "clipped",
            LayoutProblem::OutsideWindow => "outside the window",
            LayoutProblem::Overlap => "overlaps",
        };
        write!(
            f,
            "{problem}: {:?} at ({:.0}, {:.0}) {:.0}x{:.0}",
            self.what.chars().take(40).collect::<String>(),
            self.rect.left(),
            self.rect.top(),
            self.rect.width(),
            self.rect.height(),
        )?;
        match &self.with {
            Some(other) => write!(f, " over {:?}", other.chars().take(40).collect::<String>()),
            None => Ok(()),
        }
    }
}

/// Half a point: the slack allowed where a glyph's ink or a rounded edge reaches a hair past its layout box.
const EDGE_SLACK: f32 = 0.5;

/// Two rectangles overlap by more than a sliver (more than [`EDGE_SLACK`] in both directions).
fn overlaps(a: Rect, b: Rect) -> bool {
    let shared = a.intersect(b);
    shared.width() > EDGE_SLACK && shared.height() > EDGE_SLACK
}

/// Text that is clipped, lies outside `window`, or is drawn over other text in the frame.
///
/// Text is the one thing every control shows, so this is the first layout check; controls are checked through
/// [`control_overlaps`] and [`control_bounds`] with the tree's rectangles. Elided text is measured as drawn: an
/// ellipsis is a cut too, and it fits its clip rectangle by construction, so only text that does not fit is reported.
/// Two pieces of text are a collision whatever their positions, identical ones included: nothing in the frame may
/// draw text over other text.
pub fn layout_findings(output: &FullOutput, window: Rect) -> Vec<LayoutFinding> {
    let mut texts: Vec<(String, Rect, Rect)> = Vec::new();
    for ClippedShape { shape, clip_rect } in &output.shapes {
        let mut flat = Vec::new();
        flatten(shape, &mut flat);
        for shape in flat {
            let Shape::Text(text) = shape else { continue };
            if text.galley.job.text.trim().is_empty() {
                continue;
            }
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            texts.push((text.galley.job.text.clone(), rect, *clip_rect));
        }
    }

    let mut out = Vec::new();
    for (index, (what, rect, clip)) in texts.iter().enumerate() {
        if !clip.expand(EDGE_SLACK).contains_rect(*rect) {
            out.push(finding(LayoutProblem::Clipped, what, *rect, None));
        }
        if !window.expand(EDGE_SLACK).contains_rect(*rect) {
            out.push(finding(LayoutProblem::OutsideWindow, what, *rect, None));
        }
        for (other, other_rect, _) in &texts[index + 1..] {
            if overlaps(*rect, *other_rect) {
                out.push(finding(LayoutProblem::Overlap, what, *rect, Some(other)));
            }
        }
    }
    out
}

/// An operable node of the accessibility tree: a control a person can operate.
#[derive(Clone, Debug)]
pub struct Control {
    /// Its accessible name.
    pub name: String,
    /// Its AccessKit role.
    pub role: Role,
    /// Where it is on screen.
    pub rect: Rect,
    /// The nearest operable node above it in the tree, as an index into the same list. Parents come before their
    /// children in the list, so the chain of parents always ends.
    pub parent: Option<usize>,
}

/// Whether `ancestor` is above `node` in the tree (through any number of operable nodes in between).
fn is_ancestor(controls: &[Control], ancestor: usize, node: usize) -> bool {
    let mut current = controls[node].parent;
    while let Some(parent) = current {
        if parent == ancestor {
            return true;
        }
        current = controls[parent].parent;
    }
    false
}

/// A tab and the button wholly inside it: the tab's own close affordance. AccessKit keeps the button a sibling of the
/// tab (a button is not a child of a tab in ARIA), so the tree does not say it is nested, but it is drawn inside the
/// tab on purpose and the tab bar keeps the tab's label clear of it.
fn is_tab_close(a: &Control, b: &Control) -> bool {
    let (tab, button) = if a.role == Role::Tab { (a, b) } else { (b, a) };
    tab.role == Role::Tab && button.role == Role::Button && tab.rect.expand(EDGE_SLACK).contains_rect(button.rect)
}

/// Controls drawn over one another. A control drawn over its own ancestor or descendant is nesting (a row's button
/// sits inside the row, a curve's point handles on its curve), and a tab's close button inside its tab is the same
/// by design ([`is_tab_close`]), so neither is a collision. Any other pair that overlaps is: two controls with identical rectangles, or one inside the other as siblings, are both reported.
pub fn control_overlaps(controls: &[Control]) -> Vec<LayoutFinding> {
    let mut out = Vec::new();
    for (index, control) in controls.iter().enumerate() {
        for (other, rival) in controls.iter().enumerate().skip(index + 1) {
            let nested = is_ancestor(controls, index, other) || is_ancestor(controls, other, index);
            if !nested && !is_tab_close(control, rival) && overlaps(control.rect, rival.rect) {
                out.push(finding(LayoutProblem::Overlap, &control.name, control.rect, Some(&rival.name)));
            }
        }
    }
    out
}

/// Controls that lie outside `window`. Every control is checked, composite parents included: a parent that is off
/// screen is a problem even when its children are visible.
pub fn control_bounds(controls: &[Control], window: Rect) -> Vec<LayoutFinding> {
    controls
        .iter()
        .filter(|control| !window.expand(EDGE_SLACK).contains_rect(control.rect))
        .map(|control| finding(LayoutProblem::OutsideWindow, &control.name, control.rect, None))
        .collect()
}

fn finding(problem: LayoutProblem, what: &str, rect: Rect, with: Option<&String>) -> LayoutFinding {
    LayoutFinding { problem, what: what.to_owned(), rect, with: with.cloned() }
}
