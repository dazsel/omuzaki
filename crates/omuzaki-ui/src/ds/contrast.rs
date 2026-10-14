//! WCAG 2.x contrast ratios and the table of token pairs the design system promises (`design-system.md` section 10).
//!
//! The table lives in the library, not only in the test, so the gallery can print it next to the palette and a
//! reviewer sees the same numbers CI asserts.

use egui::Color32;
use omuzaki_core::colour::{Colour, Deficiency, simulate};

use super::tokens::{Status, Theme, ThemeKind, mix};

/// Relative luminance of an sRGB colour (WCAG 2.x). Alpha is ignored: callers pass the opaque result of a blend.
pub fn relative_luminance(colour: Color32) -> f64 {
    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    0.2126 * linear(colour.r()) + 0.7152 * linear(colour.g()) + 0.0722 * linear(colour.b())
}

/// WCAG contrast ratio between two colours, from 1 to 21, in either order.
pub fn contrast_ratio(a: Color32, b: Color32) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

/// What a pair is held to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rule {
    /// Text: 4.5:1 (WCAG 1.4.3), 7:1 in high contrast (AAA).
    Text,
    /// Borders, focus rings, selection bars: 3:1 (WCAG 1.4.11), 7:1 in high contrast.
    NonText,
    /// Disabled text: exempt from WCAG but kept legible at 3:1.
    Disabled,
}

/// One foreground on one background, with the least ratio the spec accepts.
#[derive(Clone, Debug)]
pub struct Pair {
    /// For failure messages: `"text on bg1"`.
    pub name: String,
    /// The foreground.
    pub fg: Color32,
    /// The background it sits on.
    pub bg: Color32,
    /// The rule it is held to.
    pub rule: Rule,
    /// The least acceptable ratio in this theme.
    pub min: f64,
}

impl Pair {
    /// The measured ratio.
    pub fn ratio(&self) -> f64 {
        contrast_ratio(self.fg, self.bg)
    }

    /// Whether the pair meets its threshold.
    pub fn passes(&self) -> bool {
        self.ratio() >= self.min
    }
}

/// The least ratio `rule` accepts in `theme`.
pub(crate) fn threshold(theme: &Theme, rule: Rule) -> f64 {
    let hc = theme.kind == ThemeKind::HighContrast;
    match (rule, hc) {
        (Rule::Text | Rule::NonText, true) => 7.0,
        (Rule::Text, false) | (Rule::Disabled, true) => 4.5,
        (Rule::NonText | Rule::Disabled, false) => 3.0,
    }
}

/// The pairs of one theme being collected: the token-level ones in [`pairs`], and what each component adds through
/// [`register_contrast!`].
pub struct Table<'t> {
    theme: &'t Theme,
    pairs: Vec<Pair>,
}

impl Table<'_> {
    /// The theme being collected.
    pub const fn theme(&self) -> &Theme {
        self.theme
    }

    /// Adds `fg` on `bg`, held to what `rule` asks in this theme.
    pub fn add(&mut self, name: impl Into<String>, fg: Color32, bg: Color32, rule: Rule) {
        self.pairs.push(Pair { name: name.into(), fg, bg, rule, min: threshold(self.theme, rule) });
    }

    /// Adds `fg` on each of `backgrounds`, naming each pair `"{name} on {surface}"`. A control with a transparent
    /// fill sits on whatever surface the panel has, so it is held to every surface it is allowed on.
    pub fn add_on(&mut self, name: &str, fg: Color32, backgrounds: &[(&str, Color32)], rule: Rule) {
        for (surface, bg) in backgrounds {
            self.add(format!("{name} on {surface}"), fg, *bg, rule);
        }
    }

    /// The surfaces a transparent control may be drawn on (panels and cards).
    pub fn panel_surfaces(&self) -> [(&'static str, Color32); 2] {
        [("bg1", self.theme.bg1), ("bg2", self.theme.bg2)]
    }
}

/// One component's share of the contrast table: the pairs its drawing code relies on.
///
/// A component builds its colours in a plain function that both the painter and this contribution call, so the table
/// is generated from what is drawn rather than from a list somebody has to keep up to date. The gallery test
/// ([`crate::ds::audit`]) measures the real frame as well, which catches a component that forgot to register.
pub struct Contribution {
    /// For failure messages.
    pub component: &'static str,
    /// Adds the component's pairs for the table's theme.
    pub add: fn(&mut Table<'_>),
}

inventory::collect!(Contribution);

/// Registers a component's contribution to the contrast table:
///
/// ```ignore
/// register_contrast!("button", |table| { /* table.add(...) */ });
/// ```
#[macro_export]
macro_rules! register_contrast {
    ($component:literal, $add:expr $(,)?) => {
        ::inventory::submit! {
            $crate::ds::contrast::Contribution { component: $component, add: $add }
        }
        $crate::__record_registration!("contrast", $component);
    };
}

/// Every foreground and background combination `design-system.md` section 2 relies on, for one theme, followed by
/// the pairs each component registered.
///
/// Status colours as text are held to `bg0` to `bg3` only: on `bg4` the spec says to use `text` plus the icon.
pub fn pairs(theme: &Theme) -> Vec<Pair> {
    let mut table = Table { theme, pairs: Vec::new() };
    token_pairs(&mut table);
    let mut contributions: Vec<&Contribution> = inventory::iter::<Contribution>.into_iter().collect();
    contributions.sort_by_key(|c| c.component);
    for contribution in contributions {
        let from = table.pairs.len();
        (contribution.add)(&mut table);
        for pair in &mut table.pairs[from..] {
            pair.name = format!("{}: {}", contribution.component, pair.name);
        }
    }
    table.pairs
}

/// The pairs that follow from the tokens alone (section 2): text on surfaces, borders, rings, `on.*` on fills.
fn token_pairs(table: &mut Table<'_>) {
    let theme = *table.theme();
    let quiet_surfaces = [("bg0", theme.bg0), ("bg1", theme.bg1), ("bg2", theme.bg2), ("bg3", theme.bg3)];
    for (surface, bg) in quiet_surfaces {
        table.add(format!("text on {surface}"), theme.text, bg, Rule::Text);
        table.add(format!("text2 on {surface}"), theme.text2, bg, Rule::Text);
        table.add(format!("text3 on {surface}"), theme.text3, bg, Rule::Text);
        table.add(format!("accent.text on {surface}"), theme.accent_text, bg, Rule::Text);
        table.add(format!("agent on {surface}"), theme.agent, bg, Rule::Text);
        table.add(format!("ok on {surface}"), theme.ok, bg, Rule::Text);
        table.add(format!("warn on {surface}"), theme.warn, bg, Rule::Text);
        table.add(format!("danger on {surface}"), theme.danger, bg, Rule::Text);
        table.add(format!("accent (ring, bar) on {surface}"), theme.accent, bg, Rule::NonText);
        table.add(format!("focus ring on {surface}"), theme.focus.colour, bg, Rule::NonText);
    }
    for (surface, bg) in [("bg1", theme.bg1), ("bg2", theme.bg2), ("bg3", theme.bg3)] {
        table.add(format!("border.control on {surface}"), theme.border_control, bg, Rule::NonText);
    }
    // bg4 is a selection and rail fill: text on it must read, status colours are replaced by text plus an icon.
    table.add("text on bg4", theme.text, theme.bg4, Rule::Text);
    table.add("text2 on bg4", theme.text2, theme.bg4, Rule::Text);
    table.add("text3 on bg4", theme.text3, theme.bg4, Rule::Text);
    table.add("text.disabled on bg1", theme.text_disabled, theme.bg1, Rule::Disabled);
    table.add("text.disabled on bg2", theme.text_disabled, theme.bg2, Rule::Disabled);

    table.add("on.accent on accent", theme.on_accent, theme.accent, Rule::Text);
    table.add("on.accent on accent.hover", theme.on_accent, theme.accent_hover, Rule::Text);
    table.add("on.accent on accent.press", theme.on_accent, theme.accent_press, Rule::Text);
    table.add("on.agent on agent", theme.on_agent, theme.agent, Rule::Text);
    table.add("text on selection", theme.text, theme.selection, Rule::Text);
    table.add("text on agent glow", theme.text, theme.agent_glow, Rule::Text);
    table.add("selection bar on selection", theme.accent, theme.selection, Rule::NonText);

    // Chips and banners tint a surface with their status colour; their text must still read.
    for status in [Status::Ok, Status::Warn, Status::Danger] {
        for (surface, bg) in [("bg1", theme.bg1), ("bg2", theme.bg2)] {
            table.add(
                format!("{status:?} text on its tint over {surface}"),
                theme.status(status),
                theme.status_tint(status, bg),
                Rule::Text,
            );
        }
    }
}

// ----------------------------------------------------------------------------
// Colour-vision deficiency (SE-02): colours that carry a meaning must stay apart for everyone.

/// The least CIEDE2000 distance two colours that mean different things must keep, after simulation, in any of the
/// three deficiencies.
///
/// 7 continues the spec's own bar: `design-system.md` section 2.3 calls two colours the same below a CIE76 gap of about
/// 10, and for these saturated hues that is a CIEDE2000 gap of about 6 to 7 (CIEDE2000 shrinks the chroma terms CIE76
/// overstates). A just-noticeable difference is 2.3; the marks here are small and seen apart rather than side by side,
/// so the bar sits three times higher.
pub const MIN_CVD_DELTA_E: f64 = 7.0;

/// CIE L*a*b* (D65) of a colour as seen with `deficiency`, or normally when `None`.
#[allow(
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "the published sRGB to XYZ matrix and the names of the CIE formulas"
)]
fn lab_as_seen(colour: Color32, deficiency: Option<Deficiency>) -> [f64; 3] {
    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    // The shared simulation (`omuzaki-core`), so the UI and the palette checker see the same colours.
    let seen = deficiency.map_or(colour, |d| {
        let s = simulate(Colour::rgb(colour.r(), colour.g(), colour.b()), d);
        Color32::from_rgb(s.r, s.g, s.b)
    });
    let [r, g, b] = [linear(seen.r()), linear(seen.g()), linear(seen.b())];
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 between two L*a*b* colours (Sharma, Wu and Dalal 2005; `kL = kC = kH = 1`).
#[allow(clippy::many_single_char_names, reason = "the names of the published formula")]
fn delta_e_2000([l1, a1, b1]: [f64; 3], [l2, a2, b2]: [f64; 3]) -> f64 {
    let pow7 = |x: f64| x.powi(7);
    let mean_c = f64::midpoint(a1.hypot(b1), a2.hypot(b2));
    let g = 0.5 * (1.0 - (pow7(mean_c) / (pow7(mean_c) + pow7(25.0))).sqrt());
    let (ap1, ap2) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1, c2) = (ap1.hypot(b1), ap2.hypot(b2));
    let hue = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1, h2) = (hue(b1, ap1), hue(b2, ap2));
    let both_chromatic = c1 * c2 != 0.0;
    let mut dh = h2 - h1;
    if !both_chromatic {
        dh = 0.0;
    } else if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    let big_dh = 2.0 * (c1 * c2).sqrt() * (dh.to_radians() / 2.0).sin();
    let (mean_l, mean_cp) = (f64::midpoint(l1, l2), f64::midpoint(c1, c2));
    let mean_h = if !both_chromatic {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        f64::midpoint(h1, h2)
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) / 2.0
    } else {
        (h1 + h2 - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (mean_h - 30.0).to_radians().cos()
        + 0.24 * (2.0 * mean_h).to_radians().cos()
        + 0.32 * (3.0 * mean_h + 6.0).to_radians().cos()
        - 0.20 * (4.0 * mean_h - 63.0).to_radians().cos();
    let theta = 30.0 * (-((mean_h - 275.0) / 25.0).powi(2)).exp();
    let r_c = 2.0 * (pow7(mean_cp) / (pow7(mean_cp) + pow7(25.0))).sqrt();
    let s_l = 1.0 + 0.015 * (mean_l - 50.0).powi(2) / (20.0 + (mean_l - 50.0).powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * mean_cp;
    let s_h = 1.0 + 0.015 * mean_cp * t;
    let r_t = -(2.0 * theta).to_radians().sin() * r_c;
    let (dl, dc, dhh) = ((l2 - l1) / s_l, (c2 - c1) / s_c, big_dh / s_h);
    (dl * dl + dc * dc + dhh * dhh + r_t * dc * dhh).sqrt()
}

/// The CIEDE2000 distance between two colours as seen with `deficiency` (normal vision when `None`).
pub fn colour_difference(a: Color32, b: Color32, deficiency: Option<Deficiency>) -> f64 {
    delta_e_2000(lab_as_seen(a, deficiency), lab_as_seen(b, deficiency))
}

/// Two colours that mean different things and so must stay apart under every deficiency.
#[derive(Clone, Debug)]
pub struct Separation {
    /// For failure messages: `"ok vs danger"`.
    pub name: String,
    /// One colour.
    pub a: Color32,
    /// The other.
    pub b: Color32,
}

impl Separation {
    /// The smallest distance over the three deficiencies, and the deficiency that gives it.
    pub fn worst(&self) -> (Deficiency, f64) {
        Deficiency::ALL
            .into_iter()
            .map(|d| (d, colour_difference(self.a, self.b, Some(d))))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap_or((Deficiency::Protanopia, 0.0))
    }

    /// Whether the pair keeps [`MIN_CVD_DELTA_E`] in every deficiency.
    pub fn passes(&self) -> bool {
        self.worst().1 >= MIN_CVD_DELTA_E
    }
}

/// The colours of `theme` that must not be confused (`design-system.md` section 2.3):
/// - every pair of status colours (`info` is `text2`);
/// - agent, person (`text`, the colour of a person's own words) and accent;
/// - each of agent and accent against each status hue, since a chip or a ring of either sits beside a status;
/// - the two ends of a meter fill against its track.
///
/// The meter's own ramp is not held apart from itself: the value is the bar's length, and the brightening is a second
/// cue that the light theme leaves flat on purpose (`meter_floor` 100).
pub fn separations(theme: &Theme) -> Vec<Separation> {
    let statuses = [("info", theme.text2), ("ok", theme.ok), ("warn", theme.warn), ("danger", theme.danger)];
    let identities = [("person", theme.text), ("agent", theme.agent), ("accent", theme.accent)];
    let mut out = Vec::new();
    let mut add = |a: (&str, Color32), b: (&str, Color32)| {
        out.push(Separation { name: format!("{} vs {}", a.0, b.0), a: a.1, b: b.1 });
    };
    for (i, a) in statuses.iter().enumerate() {
        for b in &statuses[i + 1..] {
            add(*a, *b);
        }
    }
    for (i, a) in identities.iter().enumerate() {
        for b in &identities[i + 1..] {
            add(*a, *b);
        }
    }
    for hue in &identities[1..] {
        for status in &statuses[1..] {
            add(*hue, *status);
        }
    }
    let dimmest = mix(theme.accent, theme.bg4, theme.meter_floor);
    add(("meter dimmest fill", dimmest), ("meter track", theme.bg4));
    add(("meter full fill", theme.accent), ("meter track", theme.bg4));
    out
}

#[cfg(test)]
#[allow(clippy::unreadable_literal, reason = "6-digit hex colours, as in the spec")]
mod tests {
    use super::*;
    use crate::ds::tokens::{DARK, HIGH_CONTRAST, LIGHT, hex};

    #[test]
    fn black_on_white_is_21_to_1() {
        assert!((contrast_ratio(hex(0x000000), hex(0xFFFFFF)) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(hex(0xFFFFFF), hex(0xFFFFFF)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ratio_is_symmetric() {
        let (a, b) = (hex(0xE6C36A), hex(0x16141A));
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-12);
    }

    /// The check CI runs: every pair of every theme meets its threshold.
    #[test]
    fn every_token_pair_meets_its_threshold() {
        let mut failures = Vec::new();
        for theme in [&DARK, &LIGHT, &HIGH_CONTRAST] {
            for pair in pairs(theme) {
                if !pair.passes() {
                    failures.push(format!(
                        "{:?}: {} is {:.2}:1, needs {:.1}:1",
                        theme.kind,
                        pair.name,
                        pair.ratio(),
                        pair.min
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "contrast failures:\n{}", failures.join("\n"));
    }

    fn ratio_of(theme: &Theme, name: &str) -> f64 {
        let found = pairs(theme).into_iter().find(|p| p.name.contains(name));
        found.map_or(0.0, |p| p.ratio())
    }

    /// The four pairs the review of the first UI1 PR found in use and failing, which the old table did not list.
    #[test]
    fn the_pairs_found_failing_in_review_are_in_the_table_and_pass() {
        // Light: the dimmest meter fill on its track (was 1.9:1).
        assert!(ratio_of(&LIGHT, "dimmest fill on the track") >= 3.0);
        // Light: the label of a primary button's key cap (was 3.77:1).
        assert!(ratio_of(&LIGHT, "Primary rest key cap") >= 4.5);
        // Light: the variant badge's letter on its tint (was 4.43:1).
        assert!(ratio_of(&LIGHT, "variant badge letter") >= 4.5);
        // Dark and light: a pressed danger button's label (was 4.2:1 and 4.1:1).
        assert!(ratio_of(&DARK, "Danger pressed label") >= 4.5);
        assert!(ratio_of(&LIGHT, "Danger pressed label") >= 4.5);
        for theme in [&DARK, &LIGHT, &HIGH_CONTRAST] {
            for name in
                ["dimmest fill on the track", "Primary rest key cap", "variant badge letter", "Danger pressed label"]
            {
                assert!(ratio_of(theme, name) > 0.0, "{:?}: the table has no pair named like '{name}'", theme.kind);
            }
        }
    }

    /// Components register their own pairs, so a new one is added next to its colours and not to a list.
    #[test]
    fn the_components_that_choose_colours_each_contribute_to_the_table() {
        let registered: std::collections::BTreeSet<&str> =
            inventory::iter::<Contribution>.into_iter().map(|c| c.component).collect();
        for component in ["button", "channel_meter", "chip", "param_slider", "toggle", "tree_row"] {
            assert!(registered.contains(component), "{component} does not register its pairs: {registered:?}");
        }
    }

    /// The ratios `design-system.md` quotes, recomputed (a drift in a hex value or in the doc shows up here).
    #[test]
    fn spec_quoted_ratios_hold() {
        let near = |got: f64, want: f64| (got - want).abs() < 0.06;
        // 2.1: border.control on bg1 and bg2.
        assert!(near(contrast_ratio(DARK.border_control, DARK.bg1), 4.2));
        assert!(near(contrast_ratio(DARK.border_control, DARK.bg2), 3.9));
        assert!(near(contrast_ratio(LIGHT.border_control, LIGHT.bg1), 3.5));
        assert!(near(contrast_ratio(LIGHT.border_control, LIGHT.bg2), 3.9));
        // 2.2: text on bg1, bg2, bg3.
        assert!(near(contrast_ratio(DARK.text, DARK.bg1), 15.0));
        assert!(near(contrast_ratio(DARK.text2, DARK.bg2), 8.2));
        assert!(near(contrast_ratio(LIGHT.text3, LIGHT.bg3), 5.3));
        assert!(near(contrast_ratio(DARK.text3, DARK.bg4), 4.8));
        // 2.3: accent and on.accent.
        assert!(near(contrast_ratio(DARK.accent, DARK.bg1), 10.8));
        assert!(near(contrast_ratio(LIGHT.accent, LIGHT.bg1), 4.2));
        assert!(near(contrast_ratio(LIGHT.accent_text, LIGHT.bg1), 5.8));
        assert!(near(contrast_ratio(DARK.on_accent, DARK.accent), 10.6));
        assert!(near(contrast_ratio(LIGHT.on_accent, LIGHT.accent), 4.7));
        // 2.4: high contrast.
        assert!(near(contrast_ratio(HIGH_CONTRAST.accent, HIGH_CONTRAST.bg1), 14.9));
        assert!(near(contrast_ratio(HIGH_CONTRAST.agent, HIGH_CONTRAST.bg1), 11.8));
        assert!(near(contrast_ratio(HIGH_CONTRAST.danger, HIGH_CONTRAST.bg1), 9.5));
    }

    /// The selected gold row the spec quotes: `#443A2C`, with `text` at 9.1:1 on it.
    #[test]
    fn selection_is_the_quoted_gold_row() {
        assert_eq!(DARK.selection, hex(0x443A2C));
        assert!((contrast_ratio(DARK.text, DARK.selection) - 9.1).abs() < 0.06);
    }

    /// Sharma, Wu and Dalal (2005) test data for CIEDE2000, which pins every branch of the formula we use.
    #[test]
    fn delta_e_2000_matches_the_published_test_data() {
        let cases = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 2.5, 0.0], [50.0, 0.0, -2.5], 4.3065),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
        ];
        for (a, b, want) in cases {
            for (x, y) in [(a, b), (b, a)] {
                assert!((delta_e_2000(x, y) - want).abs() < 1e-4, "{x:?} {y:?}: {}", delta_e_2000(x, y));
            }
        }
    }

    #[test]
    fn simulation_leaves_greys_alone_and_collapses_red_green() {
        let grey = hex(0x808080);
        for d in Deficiency::ALL {
            assert!(colour_difference(grey, hex(0x808080), Some(d)) < 1e-9);
            let seen = lab_as_seen(grey, Some(d));
            let normal = lab_as_seen(grey, None);
            assert!((seen[0] - normal[0]).abs() < 0.5 && seen[1].abs() < 1.0 && seen[2].abs() < 1.0, "{d:?}: {seen:?}");
        }
        let (red, green) = (hex(0xC04040), hex(0x309030));
        assert!(colour_difference(red, green, None) > 40.0);
        assert!(colour_difference(red, green, Some(Deficiency::Deuteranopia)) < 3.0);
    }
}
