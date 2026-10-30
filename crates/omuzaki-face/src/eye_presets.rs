//! Eye presets and the seeded random eye (#2469), behind `face.eyes.preset` and `face.eyes.randomise`.
//!
//! **Presets** are data, in `library/eyes.toml`: each names what differs from the default eye, and [`preset_eyes`] gives
//! the whole eye the preset makes (the default with the preset merged over it), so applying one never leaves an
//! earlier design's fields behind.
//!
//! **The random eye** starts from a preset and draws every part of the design from ranges that always read as a
//! good eye: the opening's shape and the iris's size, the iris colours (a hue with a dark top, a mid tone, a light lit
//! crescent and a darker ring, sometimes a different right eye), the catchlights, the iris detail, the lashes and the
//! eye white. It is a pure function of the seed, and each group of the design draws from its own stream, so keeping one
//! group (say the colours) and re-rolling the rest never changes the kept group, and re-rolling one group never moves
//! the others. The ranges are the priors: they were set by rendering contact sheets of many seeds and narrowing every
//! range that produced an eye that didn't read (see `tests/eye_presets.rs`).

use omuzaki_face_api::{ColourRef, EyeStyle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::painter::marks::Rng;

/// The preset library.
const LIBRARY: &str = include_str!("../library/eyes.toml");

/// The preset the random eye starts from when the caller names none.
pub const DEFAULT_STYLE: &str = "kit";

/// One eye preset.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EyePreset {
    /// Its id, as `face.eyes.preset` takes it.
    pub id: String,
    /// One line on what it looks like.
    pub summary: String,
    /// What it changes of the default eye, as a partial `eyes` section.
    pub eyes: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    preset: Vec<EyePreset>,
}

/// Every preset, in the library's order.
///
/// # Errors
/// The library's parse error, which would be a bug in `library/eyes.toml` (a test reads it).
pub fn presets() -> Result<Vec<EyePreset>, String> {
    toml::from_str::<Library>(LIBRARY).map(|l| l.preset).map_err(|e| format!("library/eyes.toml: {e}"))
}

/// The whole eye section preset `id` makes: the default eye with the preset merged over it.
///
/// # Errors
/// An unknown id (with the ids there are), or a preset that doesn't read as an eye.
pub fn preset_eyes(id: &str) -> Result<Value, String> {
    let all = presets()?;
    let Some(preset) = all.iter().find(|p| p.id == id) else {
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        return Err(format!("there is no eye preset '{id}'; the presets are {}", ids.join(", ")));
    };
    let mut eyes = serde_json::to_value(EyeStyle::default()).map_err(|e| e.to_string())?;
    merge(&mut eyes, &preset.eyes);
    // Read it back, so a preset with a typo fails here and not in a face.
    serde_json::from_value::<EyeStyle>(eyes.clone()).map_err(|e| format!("eye preset '{id}': {e}"))?;
    Ok(eyes)
}

/// Merges `over` into `base`, object by object; anything else replaces.
pub(crate) fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// A group of the eye's design that a random eye can keep.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EyeGroup {
    /// The opening (roundness, tilt, lid heights, corners), the eye's size and the iris's size and place.
    Shape,
    /// The iris colours (both eyes'), the pupil's colour and the lash colour.
    Colours,
    /// The catchlights, the shimmer and the caustic glow.
    Highlights,
    /// The iris's structure and the pupil's shape and size.
    Detail,
    /// The lash line and the strand lashes.
    Lashes,
    /// The eye white and the waterline.
    Sclera,
}

impl EyeGroup {
    /// Every group.
    pub const ALL: [Self; 6] = [Self::Shape, Self::Colours, Self::Highlights, Self::Detail, Self::Lashes, Self::Sclera];

    /// Each group's own stream, mixed into the seed.
    fn stream(self) -> u32 {
        match self {
            Self::Shape => 0x243f_6a88,
            Self::Colours => 0x85a3_08d3,
            Self::Highlights => 0x1319_8a2e,
            Self::Detail => 0x0370_7344,
            Self::Lashes => 0xa409_3822,
            Self::Sclera => 0x299f_31d0,
        }
    }
}

/// A seeded random eye: `base` (a whole eye section) with every group not in `keep` drawn from the priors.
pub fn randomise(base: &Value, seed: u32, keep: &[EyeGroup]) -> Value {
    let mut eyes = base.clone();
    for group in EyeGroup::ALL {
        if keep.contains(&group) {
            continue;
        }
        let mut draw = Draw(Rng::new(seed.wrapping_mul(0x9e37_79b9) ^ group.stream()));
        match group {
            EyeGroup::Shape => shape(&mut eyes, &mut draw),
            EyeGroup::Colours => colours(&mut eyes, &mut draw),
            EyeGroup::Highlights => highlights(&mut eyes, &mut draw),
            EyeGroup::Detail => {
                detail(&mut eyes, &mut draw);
                // The detail's pattern is the seed's too.
                set(&mut eyes, &["seed"], json!(seed));
            }
            EyeGroup::Lashes => lashes(&mut eyes, &mut draw),
            EyeGroup::Sclera => sclera(&mut eyes, &mut draw),
        }
    }
    eyes
}

/// Uniform draws, rounded to three decimals so a random eye reads cleanly in a face file.
struct Draw(Rng);

impl Draw {
    fn unit(&mut self) -> f64 {
        f64::from(self.0.unit())
    }

    fn range(&mut self, lo: f64, hi: f64) -> Value {
        json!(round(lo + (hi - lo) * self.unit()))
    }

    fn count(&mut self, lo: u32, hi: u32) -> Value {
        // The product is within lo..=hi, far inside u32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = lo + ((f64::from(hi - lo) + 1.0) * self.unit()).floor().min(f64::from(hi - lo)) as u32;
        json!(n)
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// One of `options`, by weight.
    fn pick<'a>(&mut self, options: &[(&'a str, f64)]) -> &'a str {
        let total: f64 = options.iter().map(|o| o.1).sum();
        let mut at = self.unit() * total;
        for (name, weight) in options {
            if at < *weight {
                return name;
            }
            at -= weight;
        }
        options.last().map_or("", |o| o.0)
    }
}

fn round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Writes `value` at `keys` under `node`, making the objects on the way.
fn set(node: &mut Value, keys: &[&str], value: Value) {
    let Some((last, path)) = keys.split_last() else { return };
    let mut at = node;
    for key in path {
        if !at.get(*key).is_some_and(Value::is_object)
            && let Value::Object(map) = at
        {
            map.insert((*key).to_owned(), Value::Object(Map::new()));
        }
        let Some(next) = at.get_mut(*key) else { return };
        at = next;
    }
    if let Value::Object(map) = at {
        map.insert((*last).to_owned(), value);
    }
}

fn shape(eyes: &mut Value, d: &mut Draw) {
    set(eyes, &["scale"], d.range(0.94, 1.12));
    set(eyes, &["aperture", "roundness"], d.range(0.35, 0.85));
    set(eyes, &["aperture", "tilt"], d.range(-0.12, 0.18));
    set(eyes, &["aperture", "upper_height"], d.range(0.62, 0.84));
    set(eyes, &["aperture", "lower_depth"], d.range(0.48, 0.72));
    set(eyes, &["aperture", "inner_sharpness"], d.range(0.25, 0.8));
    set(eyes, &["aperture", "outer_bluntness"], d.range(0.2, 0.7));
    let width = 0.56 + 0.16 * d.unit();
    set(eyes, &["iris", "width"], json!(round(width)));
    set(eyes, &["iris", "height"], json!(round((width * (1.25 + 0.2 * d.unit())).min(1.05))));
    set(eyes, &["iris", "drop"], d.range(-0.02, 0.06));
}

/// An iris colour family from a hue (degrees) and a saturation: top, mid, low, ring.
fn iris_family(hue: f64, sat: f64, d: &mut Draw) -> [ColourRef; 4] {
    let shift = (d.unit() - 0.5) * 50.0;
    [
        hsl(hue, sat, 0.2 + 0.08 * d.unit(), 255),
        hsl(hue, sat, 0.38 + 0.1 * d.unit(), 255),
        hsl(hue + shift, sat * 0.8, 0.66 + 0.12 * d.unit(), 255),
        hsl(hue, sat * 0.9, 0.12, 255),
    ]
}

fn colours(eyes: &mut Value, d: &mut Draw) {
    let (hue, sat) = (360.0 * d.unit(), 0.35 + 0.5 * d.unit());
    let [top, mid, low, ring] = iris_family(hue, sat, d);
    for (key, colour) in [("top", top), ("mid", mid), ("low", low), ("ring", ring)] {
        set(eyes, &["iris", key], colour_json(colour));
    }
    set(eyes, &["iris", "pupil", "colour"], colour_json(hsl(hue, sat, 0.09, 150)));
    // Lashes: dark browns to near black.
    set(
        eyes,
        &["lash", "colour"],
        colour_json(hsl(10.0 + 25.0 * d.unit(), 0.25 + 0.3 * d.unit(), 0.08 + 0.12 * d.unit(), 255)),
    );
    let odd = d.chance(0.08);
    // Always draw the second family, so whether the eyes differ never shifts the other draws.
    let other = (hue + 90.0 + 180.0 * d.unit()) % 360.0;
    let [top, mid, low, ring] = iris_family(other, sat, d);
    if let Value::Object(map) = eyes {
        if odd {
            map.insert(
                "right_iris".to_owned(),
                json!({ "top": colour_json(top), "mid": colour_json(mid), "low": colour_json(low), "ring": colour_json(ring) }),
            );
        } else {
            map.remove("right_iris");
        }
    }
}

fn highlights(eyes: &mut Value, d: &mut Draw) {
    let shape = d.pick(&[("blob", 0.55), ("window", 0.25), ("star", 0.1), ("heart", 0.04), ("crescent", 0.06)]);
    set(eyes, &["highlights", "shape"], json!(shape));
    set(eyes, &["highlights", "size"], d.range(0.2, 0.34));
    set(eyes, &["highlights", "offset"], json!([round(0.15 + 0.25 * d.unit()), round(0.15 + 0.25 * d.unit())]));
    set(eyes, &["highlights", "second"], d.range(0.3, 0.8));
    let shimmer = d.chance(0.3);
    let amount = d.range(0.2, 0.7);
    set(eyes, &["highlights", "shimmer"], if shimmer { amount } else { json!(0.0) });
    set(eyes, &["highlights", "caustic"], d.range(0.3, 0.8));
    let n = d.count(0, 3);
    let n = n.as_u64().unwrap_or(0);
    let mut extra = Vec::new();
    for i in 0..3 {
        let shape = d.pick(&[("blob", 0.5), ("star", 0.25), ("ring", 0.15), ("window", 0.1)]);
        let light = json!({
            "shape": shape,
            "offset": [round(-0.5 + 0.35 * d.unit()), round(-0.55 + 0.4 * d.unit())],
            "size": round(0.06 + 0.1 * d.unit()),
            "aspect": 1.0,
            "angle_deg": round(-20.0 + 40.0 * d.unit()),
            "opacity": round(0.55 + 0.4 * d.unit()),
            "colour": "#ffffff",
        });
        if i < n {
            extra.push(light);
        }
    }
    set(eyes, &["highlights", "extra"], Value::Array(extra));
}

fn detail(eyes: &mut Value, d: &mut Draw) {
    set(eyes, &["iris", "detail", "fibres", "strength"], d.range(0.3, 0.8));
    set(eyes, &["iris", "detail", "fibres", "count"], d.count(48, 160));
    set(eyes, &["iris", "detail", "fibres", "waviness"], d.range(0.15, 0.6));
    set(eyes, &["iris", "detail", "collarette", "strength"], d.range(0.0, 0.7));
    set(eyes, &["iris", "detail", "collarette", "teeth"], d.count(12, 28));
    set(eyes, &["iris", "detail", "flecks", "strength"], d.range(0.0, 0.6));
    set(eyes, &["iris", "detail", "limbal", "strength"], d.range(0.4, 0.9));
    set(eyes, &["iris", "detail", "pupil_glow"], d.range(0.1, 0.6));
    let pupil = d.pick(&[("round", 0.92), ("slit", 0.06), ("heart", 0.02)]);
    set(eyes, &["iris", "pupil", "shape"], json!(pupil));
    set(eyes, &["iris", "pupil", "size"], d.range(0.85, 1.15));
}

fn lashes(eyes: &mut Value, d: &mut Draw) {
    set(eyes, &["lash", "thickness"], d.range(0.0045, 0.0075));
    set(eyes, &["lash", "flick"], d.range(0.2, 1.0));
    set(eyes, &["lash", "strands", "upper", "count"], d.count(14, 40));
    set(eyes, &["lash", "strands", "upper", "length"], d.range(0.2, 0.38));
    set(eyes, &["lash", "strands", "upper", "curl"], d.range(0.3, 0.7));
    set(eyes, &["lash", "strands", "upper", "lift"], d.range(0.3, 0.9));
    set(eyes, &["lash", "strands", "upper", "clumping"], d.range(0.2, 0.6));
    set(eyes, &["lash", "strands", "lower", "count"], d.count(0, 16));
    set(eyes, &["lash", "strands", "lower", "length"], d.range(0.1, 0.2));
    set(eyes, &["lash", "strands", "lower", "opacity"], d.range(0.5, 0.85));
}

fn sclera(eyes: &mut Value, d: &mut Draw) {
    set(eyes, &["sclera", "corner_shade"], d.range(0.4, 0.7));
    set(eyes, &["sclera", "caruncle"], d.range(0.3, 0.7));
    set(eyes, &["sclera", "waterline"], d.range(0.4, 0.8));
}

fn colour_json(colour: ColourRef) -> Value {
    serde_json::to_value(colour).unwrap_or(Value::Null)
}

/// A colour from hue (degrees), saturation and lightness (`0..=1`), and alpha.
#[allow(clippy::many_single_char_names)] // the standard HSL conversion's names
fn hsl(hue: f64, s: f64, l: f64, a: u8) -> ColourRef {
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let h = hue.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h {
        h if h < 1.0 => (c, x, 0.0),
        h if h < 2.0 => (x, c, 0.0),
        h if h < 3.0 => (0.0, c, x),
        h if h < 4.0 => (0.0, x, c),
        h if h < 5.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    // Each channel is within 0..=1 before scaling.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f64| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    ColourRef::rgba(byte(r), byte(g), byte(b), a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_reads_as_an_eye_and_validates() {
        use omuzaki_face_api::Validate;
        for preset in presets().unwrap() {
            let eyes: EyeStyle = serde_json::from_value(preset_eyes(&preset.id).unwrap()).unwrap();
            eyes.validate().unwrap_or_else(|e| panic!("{}: {e}", preset.id));
        }
    }

    #[test]
    fn a_random_eye_is_the_seeds_and_validates() {
        use omuzaki_face_api::Validate;
        let base = preset_eyes(DEFAULT_STYLE).unwrap();
        for seed in 0..200 {
            let a = randomise(&base, seed, &[]);
            assert_eq!(a, randomise(&base, seed, &[]), "seed {seed}");
            let eyes: EyeStyle = serde_json::from_value(a).unwrap();
            eyes.validate().unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
    }

    #[test]
    fn a_kept_group_never_changes_and_rerolling_one_group_moves_only_it() {
        let base = preset_eyes(DEFAULT_STYLE).unwrap();
        let a = randomise(&base, 1, &[]);
        let b = randomise(&a, 2, &[EyeGroup::Colours]);
        assert_eq!(a["iris"]["mid"], b["iris"]["mid"]);
        assert_eq!(a["lash"]["colour"], b["lash"]["colour"]);
        assert_ne!(a["aperture"], b["aperture"]);
        // Everything but the shape kept: only the shape moves.
        let all_but_shape: Vec<EyeGroup> = EyeGroup::ALL.into_iter().filter(|g| *g != EyeGroup::Shape).collect();
        let c = randomise(&a, 3, &all_but_shape);
        assert_ne!(a["aperture"], c["aperture"]);
        for key in ["highlights", "sclera", "lash"] {
            assert_eq!(a[key], c[key], "{key}");
        }
        assert_eq!(a["iris"]["detail"], c["iris"]["detail"]);
    }

    #[test]
    fn the_kits_default_character_has_the_kit_eye() {
        // The starter is the char crate's; its eyes must be this library's `kit` preset.
        let starter: toml::Value = toml::from_str(include_str!("../../omuzaki-char/library/starter.toml")).unwrap();
        let starter_eyes = serde_json::to_value(&starter["face"]["eyes"]).unwrap();
        let mut eyes = serde_json::to_value(EyeStyle::default()).unwrap();
        merge(&mut eyes, &starter_eyes);
        assert_eq!(eyes, preset_eyes("kit").unwrap());
    }

    #[test]
    fn hsl_hits_the_primaries() {
        assert_eq!(hsl(0.0, 1.0, 0.5, 255), ColourRef::rgb(255, 0, 0));
        assert_eq!(hsl(120.0, 1.0, 0.5, 255), ColourRef::rgb(0, 255, 0));
        assert_eq!(hsl(240.0, 1.0, 0.5, 255), ColourRef::rgb(0, 0, 255));
    }
}
