//! The contrast checker (LK-03): value separation, WCAG ratio and colour-blind simulation for a character's palette,
//! with a fix suggestion for each pair that fails in any view, and the query op `palette.check_contrast` that reports them.
//!
//! # The maths
//!
//! - **WCAG 2.x relative luminance** `Y = 0.2126 R + 0.7152 G + 0.0722 B` over linear light, and the contrast ratio
//!   `(L1 + 0.05) / (L2 + 0.05)` with `L1` the lighter (W3C WCAG 2.x, section 1.4.3). Linearisation is the sRGB
//!   transfer function with threshold 0.04045, which is [`Colour::to_linear`].
//! - **`OKLab`** (Björn Ottosson, "A perceptual color space for image processing", 2020). The conversion is
//!   [`Oklch::from_colour`] from `palette::skin_tone`, the one `OKLCH` conversion in the crate; this module takes its
//!   Cartesian `a` and `b` from the polar chroma and hue. `delta-L` is the difference of `OKLab` lightness, and
//!   `delta-E` the Euclidean `OKLab` distance in plain units (so black against white is 1.0).
//! - **Colour-blind simulation**: Machado, Oliveira and Fernandes (2009), "A Physiologically-based Model for Simulation
//!   of Color Vision Deficiency", IEEE TVCG 15(6). Their 3x3 matrices at severity 1.0 act on linear RGB; the result is
//!   clamped to `0..=1` and encoded back to sRGB.
//!
//! # Fixes
//!
//! A pair that fails in any view is fixed by moving the lightness of one slot in `OKLCH` at constant hue and chroma, the smallest move
//! that reaches the threshold. Chroma is then lowered only where the colour leaves the sRGB gamut
//! ([`Oklch::to_colour`]). The fix is checked on the 8-bit colour it would store, so applying it passes.
//!
//! A fix must pass in normal vision *and* under the protan, deutan and tritan simulations, so a palette fixed for
//! readability works for everyone ([`lift_to_pass_all_vision`]). When no lightness satisfies all four views (the fixed
//! side can sit between the two ends in one view and at the other end in another), the suggestion falls back to the
//! smallest normal-vision fix and names the views it still fails.
//!
//! The lightness search scans a fine grid from the colour towards each end and bisects the first crossing, so it does
//! not assume the pass region is a single half-line: a gamut-mapped path that passes, fails and passes again is handled,
//! down to the grid resolution (`SCAN_STEPS`), which is finer than one 8-bit step of the stored colour.

use omuzaki_char_api::{CharId, CharacterDef};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::PaletteSlot;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use omuzaki_core::colour::{Deficiency, simulate};

use super::skin_tone::{Oklch, delta_e as skin_delta_e};
use crate::ops::character::access;

/// The default `delta-L` a pair of adjacent masses must reach (`OKLab` lightness, 0..1).
pub const DEFAULT_DELTA_L: f64 = 0.10;
/// The default WCAG ratio a line or an eye must reach (3:1 is the WCAG 2.x threshold for large or graphical contrast).
pub const DEFAULT_WCAG: f64 = 3.0;

/// The pass threshold for each kind of pair.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    /// The least `delta-L` a [`PairKind::Mass`] pair reaches.
    pub delta_l: f64,
    /// The least WCAG ratio a [`PairKind::Line`] pair reaches.
    pub wcag: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { delta_l: DEFAULT_DELTA_L, wcag: DEFAULT_WCAG }
    }
}

/// What a pair is, which decides which measure it passes on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairKind {
    /// Two adjacent masses (hair and skin, skin and an outfit): judged by `delta-L`.
    Mass,
    /// A line or an eye (an iris against the sclera, a lash against skin): judged by the WCAG ratio.
    Line,
}

impl PairKind {
    /// The name a report uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mass => "mass",
            Self::Line => "line",
        }
    }

    /// Whether a measured pair reaches this kind's threshold.
    pub fn passes(self, measure: &Measure, thresholds: &Thresholds) -> bool {
        match self {
            Self::Mass => measure.delta_l >= thresholds.delta_l,
            Self::Line => measure.wcag >= thresholds.wcag,
        }
    }
}

/// The WCAG 2.x relative luminance of a colour (alpha ignored), `0.0` for black and `1.0` for white.
pub fn relative_luminance(colour: Colour) -> f64 {
    let [r, g, b] = colour.to_linear().to_array().map(f64::from);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The WCAG 2.x contrast ratio of two colours, from `1.0` (identical) to `21.0` (black on white).
pub fn wcag_ratio(a: Colour, b: Colour) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The `OKLab` `[L, a, b]` of a colour (alpha ignored).
pub fn oklab(colour: Colour) -> [f64; 3] {
    let lch = Oklch::from_colour(colour);
    let (sin, cos) = lch.h.to_radians().sin_cos();
    [lch.l, lch.c * cos, lch.c * sin]
}

/// The `OKLab` lightness difference of two colours.
pub fn delta_l(a: Colour, b: Colour) -> f64 {
    (oklab(a)[0] - oklab(b)[0]).abs()
}

/// The Euclidean `OKLab` distance of two colours in plain units. `skin_tone::delta_e` is the same distance scaled by 100
/// for its tuning, so it is divided back here rather than re-deriving the `OKLab` maths.
pub fn delta_e(a: Colour, b: Colour) -> f64 {
    skin_delta_e(a, b) / 100.0
}

/// Every measure of a pair, in the units a report shows.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Measure {
    /// The WCAG 2.x contrast ratio.
    pub wcag: f64,
    /// The `OKLab` lightness difference.
    pub delta_l: f64,
    /// The `OKLab` distance in plain units.
    pub delta_e: f64,
}

/// The three measures of two colours.
pub fn measure(a: Colour, b: Colour) -> Measure {
    Measure { wcag: wcag_ratio(a, b), delta_l: delta_l(a, b), delta_e: delta_e(a, b) }
}

/// A colour after its lightness moved, as the palette would store it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Lift {
    /// The moved colour as the palette would store it.
    pub colour: Colour,
    /// The change in `OKLab` lightness of the stored colour (negative: darker).
    pub shift: f64,
}

/// Steps of the grid scanned from the colour towards black and towards white, before the threshold is refined. A step is
/// at most 1/256 of `OKLab` lightness, finer than one 8-bit step of the stored colour, so a pass region the grid skips
/// is narrower than the palette can store.
const SCAN_STEPS: u32 = 256;
/// Bisection steps once a passing grid point brackets the threshold.
const REFINE_STEPS: u32 = 24;

/// The smallest lightness move of `movable` (`OKLCH`, constant hue and chroma) after which the pair with `fixed` reaches
/// `kind`'s threshold in normal vision. Moves towards black and towards white are both tried; the smaller wins, and a tie
/// goes to the lighter. A pair that already passes returns `movable` unchanged with shift `0.0`. `None` means no
/// lightness of `movable` reaches the threshold (for any `fixed`, black or white always does, so this is rare).
pub fn lift_to_pass(movable: Colour, fixed: Colour, kind: PairKind, thresholds: &Thresholds) -> Option<Lift> {
    lift_where(movable, |c| kind.passes(&measure(c, fixed), thresholds))
}

/// As [`lift_to_pass`], but the pair must reach the threshold in normal vision *and* under each of the protan, deutan
/// and tritan simulations ([`simulate`] is applied to both colours). `None` when no lightness satisfies all four views.
pub fn lift_to_pass_all_vision(
    movable: Colour,
    fixed: Colour,
    kind: PairKind,
    thresholds: &Thresholds,
) -> Option<Lift> {
    lift_where(movable, |c| passes_all_vision(c, fixed, kind, thresholds))
}

/// Whether a pair reaches `kind`'s threshold in normal vision and under every simulated deficiency.
pub fn passes_all_vision(a: Colour, b: Colour, kind: PairKind, thresholds: &Thresholds) -> bool {
    kind.passes(&measure(a, b), thresholds)
        && Deficiency::ALL.into_iter().all(|d| kind.passes(&measure(simulate(a, d), simulate(b, d)), thresholds))
}

/// The smallest lightness move of `movable` after which `passes` holds for the colour it would store.
fn lift_where(movable: Colour, passes: impl Fn(Colour) -> bool) -> Option<Lift> {
    if passes(movable) {
        return Some(Lift { colour: movable, shift: 0.0 });
    }
    let start = Oklch::from_colour(movable);
    let at = |l: f64| Oklch { l, ..start }.to_colour();
    // Signed moves (`+` lighter), each the smallest that passes in its direction.
    let moves = [1.0_f64, -1.0]
        .into_iter()
        .filter_map(|dir| first_passing_move(start.l, dir, |x| passes(at(start.l + dir * x))).map(|x| dir * x));
    let best = moves.min_by(|a, b| a.abs().total_cmp(&b.abs()))?;
    let colour = at(start.l + best);
    Some(Lift { colour, shift: oklab(colour)[0] - oklab(movable)[0] })
}

/// The smallest distance `x` from `from` in direction `dir` (`+1` towards white, `-1` towards black) where `passes(x)`
/// holds. Found by a grid over the way to the edge, then bisected between the last failing and the first passing point.
/// The grid is walked from the nearest point out, so the first crossing found is the nearest one at grid resolution
/// even when `passes` is not monotonic. `None` when no grid point passes.
fn first_passing_move(from: f64, dir: f64, passes: impl Fn(f64) -> bool) -> Option<f64> {
    let span = if dir > 0.0 { 1.0 - from } else { from };
    if span <= 0.0 {
        return None;
    }
    let mut failing = 0.0;
    for step in 1..=SCAN_STEPS {
        let passing = span * f64::from(step) / f64::from(SCAN_STEPS);
        if passes(passing) {
            let (mut lo, mut hi) = (failing, passing);
            for _ in 0..REFINE_STEPS {
                let mid = f64::midpoint(lo, hi);
                if passes(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return Some(hi);
        }
        failing = passing;
    }
    None
}

/// The pairs checked when a call names none: adjacent masses, and the lines and eyes (the iris against the sclera, the
/// lash and the mouth line against skin). The slots are the standard colourway slots (`omuzaki_char_api::paths::colourway`
/// documents them); the mouth line is drawn in `lip`. A slot the character lacks skips its pair with a note.
const DEFAULT_PAIRS: [(&str, &str, PairKind); 7] = [
    ("hair_root", "skin", PairKind::Mass),
    ("skin", "outfit_a", PairKind::Mass),
    ("outfit_a", "outfit_b", PairKind::Mass),
    ("outfit_a", BACKGROUND, PairKind::Mass),
    ("eyes", "sclera", PairKind::Line),
    ("lash", "skin", PairKind::Line),
    ("lip", "skin", PairKind::Line),
];

/// The side name that stands for the background colour (not a palette slot, so no fix ever moves it).
const BACKGROUND: &str = "background";
/// The background when a call gives none: white, the colour a character sheet sits on.
const DEFAULT_BACKGROUND: Colour = Colour::WHITE;

/// One pair named in a call: two sides (palette slots, or `background`) and what kind of pair they are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContrastPair {
    /// The first side: a palette slot such as `hair_root`, or `background`.
    pub a: String,
    /// The second side, as `a`. The two must differ.
    pub b: String,
    /// `mass` (judged by `delta-L`) or `line` (judged by the WCAG ratio).
    pub kind: PairKind,
}

/// Checks the palette contrast of a character and suggests fixes for the pairs that fail. Read-only: it reports, and
/// `palette.set` applies a suggestion.
///
/// The default pairs are the adjacent masses (hair against skin, skin against outfit, the two outfit colours, the outfit
/// against the background) and the lines and eyes (the iris against the sclera, the lash and the mouth line against skin).
/// Each pair is measured in normal vision and under each of the three dichromacies (Machado et al. 2009). A pair that
/// fails in any of those views gets a suggestion: the smallest move of one unlocked slot's lightness that makes it pass in
/// normal vision and under all three simulations (or, when no lightness does, in normal vision alone, flagged
/// `colour_blind_safe: false` and naming the views it still fails). `locked` slots never get a suggestion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "pairs": [{"a": "skin", "b": "skin_shade", "kind": "mass"}, {"a": "skin_line", "b": "skin", "kind": "line"}], "background": "#ffffff"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaletteCheckContrast {
    /// The character.
    pub character: CharId,
    /// The pairs to check, in place of the defaults. Must not be empty.
    #[serde(default)]
    pub pairs: Option<Vec<ContrastPair>>,
    /// The background colour, `#rrggbb` or an opaque `#rrggbbff` (default white). A translucent background is refused:
    /// the colour behind it is unknown, so its contrast cannot be measured.
    #[serde(default)]
    pub background: Option<String>,
    /// The pass thresholds; each field defaults to `delta_l` 0.10 and `wcag` 3.0.
    #[serde(default)]
    pub thresholds: Option<Thresholds>,
    /// Palette slots the user has locked: a suggestion never changes them.
    #[serde(default)]
    pub locked: Vec<String>,
}

impl Op for PaletteCheckContrast {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.check_contrast",
            summary: "Check a character's palette contrast (WCAG ratio, OKLab delta-L and delta-E, normal vision and \
                      three colour-blind simulations) and suggest a lightness fix for each failing pair.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let thresholds = checked_thresholds(self.thresholds.unwrap_or_default())?;
        let background = match &self.background {
            Some(text) => opaque_background(text)?,
            None => DEFAULT_BACKGROUND,
        };
        let locked = self.locked_slots()?;
        let pairs = self.pairs_to_check()?;

        let mut reports = Vec::new();
        let mut skipped = Vec::new();
        for pair in &pairs {
            let resolved = [&pair.a, &pair.b].map(|name| resolve_side(name, &def, background, &locked));
            match resolved {
                [Ok(a), Ok(b)] => reports.push(evaluate(&a, &b, pair.kind, &thresholds)),
                resolved => {
                    let missing: Vec<&str> =
                        resolved.iter().filter_map(|r| r.as_ref().err().map(String::as_str)).collect();
                    skipped.push(skipped_entry(&self.character, pair, &missing));
                }
            }
        }

        let all_pass = reports.iter().all(|r| r.pass);
        let output = json!({
            "character": self.character.to_string(),
            "background": background.to_string(),
            "thresholds": { "delta_l": thresholds.delta_l, "wcag": thresholds.wcag },
            "pass": all_pass,
            "pairs": reports.iter().map(PairReport::to_json).collect::<Vec<_>>(),
            "skipped": skipped,
        });
        Ok(Plan::answer(output))
    }
}

register_op!(PaletteCheckContrast);

/// One side of a pair as the op resolves it: a palette slot (a fix may move it unless it is locked) or the background.
struct Side {
    name: String,
    colour: Colour,
    /// The slot a fix may move; `None` for the background and for a locked slot.
    movable: Option<PaletteSlot>,
}

/// Resolves one side name. `Err` carries the name when the character has no such palette slot.
fn resolve_side(name: &str, def: &CharacterDef, background: Colour, locked: &[PaletteSlot]) -> Result<Side, String> {
    if name == BACKGROUND {
        return Ok(Side { name: name.to_owned(), colour: background, movable: None });
    }
    let slot = PaletteSlot::new(name).map_err(|_| name.to_owned())?;
    let colour = *def.palette.0.get(&slot).ok_or_else(|| name.to_owned())?;
    let movable = (!locked.contains(&slot)).then_some(slot);
    Ok(Side { name: name.to_owned(), colour, movable })
}

/// The entry a pair the character cannot be checked on leaves in `skipped`, with a note naming the missing slots.
fn skipped_entry(character: &CharId, pair: &ContrastPair, missing: &[&str]) -> Value {
    let names = missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(" and ");
    json!({
        "a": pair.a,
        "b": pair.b,
        "kind": pair.kind.as_str(),
        "note": format!("skipped: character `{character}` has no palette slot {names}"),
    })
}

/// Parses the background. The background is the bottom layer of the page, so a translucent one has nothing to composite
/// over and its WCAG ratio would depend on an unknown colour; it is refused rather than measured with its alpha dropped.
///
/// # Errors
/// `invalid`, naming `background`, for text that isn't a colour or has alpha below 255.
fn opaque_background(text: &str) -> Result<Colour, OpError> {
    let colour = text.parse::<Colour>().map_err(|_| {
        OpError::invalid("background", format!("`{text}` isn't a colour: use #rrggbb or #rrggbbaa"))
            .hint("give the background as a fixed colour such as #ffffff")
    })?;
    if colour.a != u8::MAX {
        return Err(OpError::invalid(
            "background",
            format!("`{text}` is translucent (alpha {}/255): contrast needs the colour behind it", colour.a),
        )
        .hint("give the opaque colour it sits on, such as #ffffff"));
    }
    Ok(colour)
}

/// The fixed thresholds: a `delta-L` is an `OKLab` lightness difference (0 to 1), a WCAG ratio runs from 1 to 21.
///
/// # Errors
/// `invalid`, naming `thresholds/delta_l` or `thresholds/wcag`, for a value outside its range.
fn checked_thresholds(t: Thresholds) -> Result<Thresholds, OpError> {
    if !(t.delta_l.is_finite() && (0.0..=1.0).contains(&t.delta_l)) {
        return Err(OpError::invalid(
            "thresholds/delta_l",
            format!("the delta-L threshold {} isn't in 0..=1", t.delta_l),
        )
        .hint("delta-L is an OKLab lightness difference; the default is 0.10"));
    }
    if !(t.wcag.is_finite() && (1.0..=21.0).contains(&t.wcag)) {
        return Err(OpError::invalid("thresholds/wcag", format!("the WCAG threshold {} isn't in 1..=21", t.wcag))
            .hint("a WCAG contrast ratio runs from 1 (no contrast) to 21 (black on white); the default is 3"));
    }
    Ok(t)
}

impl PaletteCheckContrast {
    /// The locked slot names, each a valid palette slot.
    ///
    /// # Errors
    /// `invalid`, naming `locked/<index>`, for a name that cannot be a palette slot.
    fn locked_slots(&self) -> Result<Vec<PaletteSlot>, OpError> {
        self.locked
            .iter()
            .enumerate()
            .map(|(i, name)| {
                PaletteSlot::new(name.as_str()).map_err(|e| {
                    OpError::invalid(format!("locked/{i}"), format!("`{name}` isn't a palette slot name: {e}"))
                        .hint("list the slots a palette holds, for example hair_root or skin")
                })
            })
            .collect()
    }

    /// The pairs to check: the given ones, or the defaults.
    ///
    /// # Errors
    /// `invalid`, naming `pairs` (empty), `pairs/<i>/a` or `pairs/<i>/b` (a side that is neither a slot name nor
    /// `background`, or two equal sides).
    fn pairs_to_check(&self) -> Result<Vec<ContrastPair>, OpError> {
        let Some(pairs) = &self.pairs else {
            return Ok(DEFAULT_PAIRS
                .iter()
                .map(|&(a, b, kind)| ContrastPair { a: a.to_owned(), b: b.to_owned(), kind })
                .collect());
        };
        if pairs.is_empty() {
            return Err(OpError::invalid("pairs", "a check needs at least one pair")
                .hint("omit pairs to check the default set"));
        }
        for (i, pair) in pairs.iter().enumerate() {
            for (side, name) in [("a", &pair.a), ("b", &pair.b)] {
                if name.as_str() != BACKGROUND && PaletteSlot::new(name.as_str()).is_err() {
                    return Err(OpError::invalid(
                        format!("pairs/{i}/{side}"),
                        format!("`{name}` isn't a palette slot name or `{BACKGROUND}`"),
                    )
                    .hint("name a palette slot such as hair_root, or background"));
                }
            }
            if pair.a == pair.b {
                return Err(OpError::invalid(format!("pairs/{i}/b"), format!("both sides are `{}`", pair.a))
                    .hint("a pair compares two different slots, or a slot and the background"));
            }
        }
        Ok(pairs.clone())
    }
}

/// The measure a pair is judged on, its label and its threshold: `(label, minimum, reached)`.
fn gauge(kind: PairKind, m: &Measure, t: &Thresholds) -> (&'static str, f64, f64) {
    match kind {
        PairKind::Mass => ("delta-L", t.delta_l, m.delta_l),
        PairKind::Line => ("WCAG", t.wcag, m.wcag),
    }
}

/// A suggested change to one slot.
struct Fix {
    slot: PaletteSlot,
    colour: Colour,
    shift: f64,
    why: String,
    /// Whether the fixed pair passes in normal vision and under all three simulations.
    colour_blind_safe: bool,
}

/// The views in which the pair `a`/`b` falls short of `kind`'s threshold, by name: normal vision first, then each
/// dichromacy. Empty when every view passes.
fn failing_views(a: Colour, b: Colour, kind: PairKind, t: &Thresholds) -> Vec<&'static str> {
    let mut views = Vec::new();
    if !kind.passes(&measure(a, b), t) {
        views.push("normal vision");
    }
    for d in Deficiency::ALL {
        if !kind.passes(&measure(simulate(a, d), simulate(b, d)), t) {
            views.push(d.as_str());
        }
    }
    views
}

/// The smallest lightness move of an unlocked slot that makes the pair pass, or `None` if neither side can move. A move
/// of zero (the mover already passes) is no fix, so it is never offered.
fn suggest(a: &Side, b: &Side, kind: PairKind, t: &Thresholds) -> Option<Fix> {
    let smallest = |lift: fn(Colour, Colour, PairKind, &Thresholds) -> Option<Lift>| {
        [(a, b), (b, a)]
            .into_iter()
            .filter_map(|(mover, other)| {
                let slot = mover.movable.clone()?;
                let lift = lift(mover.colour, other.colour, kind, t)?;
                (lift.shift != 0.0).then_some((slot, lift, other.colour))
            })
            .min_by(|x, y| x.1.shift.abs().total_cmp(&y.1.shift.abs()))
    };
    // Prefer a fix that also holds under every colour-blind simulation; otherwise the normal-vision fix, flagged.
    let (slot, lift, other, safe) = if let Some((slot, lift, other)) = smallest(lift_to_pass_all_vision) {
        (slot, lift, other, true)
    } else {
        let (slot, lift, other) = smallest(lift_to_pass)?;
        (slot, lift, other, false)
    };

    let verb = if lift.shift < 0.0 { "darken" } else { "lighten" };
    let (label, minimum, _) = gauge(kind, &measure(a.colour, b.colour), t);
    let now = failing_views(a.colour, b.colour, kind, t).join(", ");
    let mut why = format!(
        "{verb} {slot} by {:.3} L: {} vs {} fails {now} (needs {label} >= {minimum:.2})",
        lift.shift.abs(),
        a.name,
        b.name,
    );
    if safe {
        why.push_str(", and passes every view after the fix");
    } else {
        let still = failing_views(lift.colour, other, kind, t).join(", ");
        why =
            format!("{why}; no lightness passes every view, so this fixes normal vision only and still fails {still}");
    }
    Some(Fix { slot, colour: lift.colour, shift: lift.shift, why, colour_blind_safe: safe })
}

/// A pair measured in normal vision and under each dichromacy, with its fix if it fails.
struct PairReport {
    a: String,
    b: String,
    kind: PairKind,
    thresholds: Thresholds,
    measure: Measure,
    pass: bool,
    simulated: Vec<(Deficiency, Measure, bool)>,
    fix: Option<Fix>,
    note: Option<&'static str>,
}

/// Measures one pair in normal vision and under each dichromacy, and suggests a fix when normal vision fails.
fn evaluate(a: &Side, b: &Side, kind: PairKind, t: &Thresholds) -> PairReport {
    let m = measure(a.colour, b.colour);
    let pass = kind.passes(&m, t);
    let simulated: Vec<(Deficiency, Measure, bool)> = Deficiency::ALL
        .into_iter()
        .map(|d| {
            let sm = measure(simulate(a.colour, d), simulate(b.colour, d));
            (d, sm, kind.passes(&sm, t))
        })
        .collect();
    // A pair can pass in normal vision and still fail a dichromacy, so a suggestion is owed when any view fails.
    let all_pass = pass && simulated.iter().all(|(_, _, view_pass)| *view_pass);
    let fix = if all_pass { None } else { suggest(a, b, kind, t) };
    let note = (!all_pass && fix.is_none()).then_some(if pass {
        "passes in normal vision but fails a colour-blind view, and no lightness of an unlocked side fixes it"
    } else {
        "neither side can move: both are locked or the background"
    });
    PairReport { a: a.name.clone(), b: b.name.clone(), kind, thresholds: *t, measure: m, pass, simulated, fix, note }
}

impl PairReport {
    /// The report entry for this pair.
    fn to_json(&self) -> Value {
        let (label, minimum, _) = gauge(self.kind, &self.measure, &self.thresholds);
        let mut simulated = Map::new();
        for (d, m, pass) in &self.simulated {
            simulated.insert(d.as_str().to_owned(), measure_json(m, *pass));
        }
        let suggestion = self.fix.as_ref().map_or(Value::Null, |f| {
            json!({
                "slot": f.slot.as_str(),
                "new_colour": f.colour.to_string(),
                "shift": f.shift,
                "colour_blind_safe": f.colour_blind_safe,
                "why": f.why,
            })
        });
        let mut entry = Map::new();
        entry.insert("a".to_owned(), json!(self.a));
        entry.insert("b".to_owned(), json!(self.b));
        entry.insert("kind".to_owned(), json!(self.kind.as_str()));
        entry.insert("pass".to_owned(), json!(self.pass));
        entry.insert("wcag".to_owned(), json!(self.measure.wcag));
        entry.insert("delta_l".to_owned(), json!(self.measure.delta_l));
        entry.insert("delta_e".to_owned(), json!(self.measure.delta_e));
        entry.insert("threshold".to_owned(), json!({ "measure": label, "min": minimum }));
        entry.insert("simulated".to_owned(), Value::Object(simulated));
        entry.insert("suggestion".to_owned(), suggestion);
        entry.insert("note".to_owned(), self.note.map_or(Value::Null, |n| json!(n)));
        Value::Object(entry)
    }
}

/// The three measures and the pass flag, as an object.
fn measure_json(m: &Measure, pass: bool) -> Value {
    json!({ "wcag": m.wcag, "delta_l": m.delta_l, "delta_e": m.delta_e, "pass": pass })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_finds_the_nearest_crossing_of_a_non_monotonic_path() {
        // Passes on [0.10, 0.20] and again from 0.50: the nearest passing move is 0.10, not the far region.
        let passes = |x: f64| (0.10..=0.20).contains(&x) || x >= 0.50;
        let x = first_passing_move(0.0, 1.0, passes).expect("a crossing exists");
        assert!((0.10..0.10 + 1.0 / f64::from(SCAN_STEPS)).contains(&x), "x = {x}");
        // A narrow window (two grid cells wide) before a wide one is still the nearest.
        let narrow = |x: f64| (0.30..=0.31).contains(&x) || x >= 0.80;
        let x = first_passing_move(0.0, 1.0, narrow).expect("a crossing exists");
        assert!((0.30..=0.31).contains(&x), "x = {x}");
    }

    #[test]
    fn the_search_walks_towards_black_and_reports_no_crossing() {
        let x = first_passing_move(0.8, -1.0, |x| x >= 0.25).expect("crossing");
        assert!((x - 0.25).abs() < 1e-6, "x = {x}");
        assert_eq!(first_passing_move(0.5, 1.0, |_| false), None);
        assert_eq!(first_passing_move(1.0, 1.0, |_| true), None, "no room to move");
    }
}
