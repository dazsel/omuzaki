//! The face-shape space the quality metrics are sampled over, the named profiles of the contact sheets, and the two
//! local face profiles read from a local assets folder.

use std::path::PathBuf;

use omuzaki_char_api::{Chin, Cranium, HeadOutline, HeadParams};
use omuzaki_core::rng::Rng;
use serde::Deserialize;

/// Which part of the shape space a sample comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Population {
    /// Every outline the contract validates (`HeadOutline::check`), on crania within 10% of the default's extents: what
    /// any slider position, script or blend can hand the head. (The cranium used to be held within 5%.)
    Valid,
    /// The ranges the face-shape presets live in (round U, egg oval, mochi, heart, long), on crania within 10%.
    Presets,
}

impl Population {
    /// The short name in tables.
    pub fn name(self) -> &'static str {
        match self {
            Self::Valid => "valid range",
            Self::Presets => "preset band",
        }
    }
}

fn uniform(rng: &mut Rng, lo: f64, hi: f64) -> f64 {
    lo + (hi - lo) * f64::from(rng.next_f32())
}

/// A cranium whose six extents are the default's scaled by `1 +- spread` each, with cheek fullness `0..=0.03`.
fn cranium(rng: &mut Rng, spread: f64) -> Cranium {
    let d = Cranium::default();
    let mut s = || uniform(rng, 1.0 - spread, 1.0 + spread);
    let (w, df, db, up, dnb, dnf) = (s(), s(), s(), s(), s(), s());
    Cranium {
        half_width: d.half_width * w,
        half_depth_front: d.half_depth_front * df,
        half_depth_back: d.half_depth_back * db,
        half_height_up: d.half_height_up * up,
        half_height_down_back: d.half_height_down_back * dnb,
        half_height_down_front: d.half_height_down_front * dnf,
        cheek_fullness: uniform(rng, 0.0, 0.03),
        ..d
    }
}

/// Sample `index` of `population` (seeded by both, so samples never shift when the count changes).
pub fn sample(population: Population, index: u64) -> HeadParams {
    let mut rng = Rng::new(0x703_0000 + index * 2 + u64::from(population == Population::Presets));
    match population {
        Population::Valid => {
            let cranium = cranium(&mut rng, 0.1);
            let half_width = uniform(&mut rng, 0.05, 0.4);
            let top = uniform(&mut rng, -0.3, 0.2);
            let height = uniform(&mut rng, 0.02, 0.4);
            let exponent = uniform(&mut rng, 1.0, 4.0);
            let cheek_puff = uniform(&mut rng, 0.0, 0.05);
            let chin = (rng.next_f32() < 0.5).then(|| {
                let y = uniform(&mut rng, -0.5, -0.001);
                let stretch_top = uniform(&mut rng, y + 0.001, 0.2_f64.min(y + 0.6)).max(-0.4);
                Chin { y, stretch_top, min_ratio: uniform(&mut rng, 0.2, 1.0) }
            });
            HeadParams {
                cranium,
                outline: HeadOutline { half_width, top, height, exponent, chin, cheek_puff },
                ..HeadParams::default()
            }
        }
        Population::Presets => {
            let cranium = cranium(&mut rng, 0.1);
            let half_width = uniform(&mut rng, 0.12, 0.18);
            let top = uniform(&mut rng, -0.13, -0.07);
            let height = uniform(&mut rng, 0.07, 0.16);
            let exponent = uniform(&mut rng, 1.4, 2.8);
            let cheek_puff = uniform(&mut rng, 0.0, 0.025);
            let chin = (rng.next_f32() < 0.5).then(|| Chin {
                y: uniform(&mut rng, -0.32, -0.19),
                stretch_top: uniform(&mut rng, -0.10, -0.05),
                min_ratio: uniform(&mut rng, 0.4, 0.9),
            });
            HeadParams {
                cranium,
                outline: HeadOutline { half_width, top, height, exponent, chin, cheek_puff },
                ..HeadParams::default()
            }
        }
    }
}

/// The public named face profiles of the contact sheets: the contract's default and four extremes of the slider space.
pub fn named() -> Vec<(&'static str, HeadParams)> {
    let o = HeadOutline::default;
    let with = |outline: HeadOutline| HeadParams { outline, ..HeadParams::default() };
    vec![
        ("default", with(o())),
        (
            "narrow diamond, deep chin",
            with(HeadOutline {
                half_width: 0.08,
                top: -0.08,
                height: 0.2,
                exponent: 1.0,
                chin: Some(Chin { y: -0.35, stretch_top: -0.05, min_ratio: 0.3 }),
                ..o()
            }),
        ),
        ("wide square", with(HeadOutline { half_width: 0.25, top: -0.12, height: 0.2, exponent: 4.0, ..o() })),
        (
            "chubby puff",
            with(HeadOutline { half_width: 0.16, top: -0.1, height: 0.1, exponent: 2.2, cheek_puff: 0.045, ..o() }),
        ),
        (
            "long chin",
            with(HeadOutline {
                half_width: 0.14,
                top: -0.09,
                height: 0.14,
                exponent: 1.7,
                chin: Some(Chin { y: -0.40, stretch_top: -0.06, min_ratio: 0.5 }),
                ..o()
            }),
        ),
    ]
}

/// The prototype's head centre: the local profiles' heights are measured from it.
const PROTOTYPE_CENTRE: [f64; 3] = [0.0, 0.778, -0.022];

#[derive(Deserialize)]
struct Constants {
    #[serde(rename = "W0")]
    half_width: f64,
    #[serde(rename = "TOP")]
    top: f64,
    #[serde(rename = "H")]
    height: f64,
    #[serde(rename = "P")]
    exponent: f64,
    puff: f64,
    chin: Option<f64>,
    #[serde(rename = "stretchTop")]
    stretch_top: Option<f64>,
    #[serde(rename = "minRatio")]
    min_ratio: Option<f64>,
}

#[derive(Deserialize)]
struct Set {
    constants: Constants,
}

#[derive(Deserialize)]
struct Private {
    sets: std::collections::BTreeMap<String, Set>,
}

/// Where the prototype's generator script writes the local face profiles (`OMUZAKI_JS_REF` overrides the dump root, as for `JsRef`).
fn private_path() -> PathBuf {
    let root = std::env::var_os("OMUZAKI_JS_REF")
        .map_or_else(|| omuzaki_core::assets::assets_root().join("goldens/js"), PathBuf::from);
    root.join("geometry/char_head/profiles.local.json")
}

/// The two local face profiles by profile name (`sheet`, the game head, and `close-up`), in the head-local frame
/// on the prototype's cranium, or `None` without the local assets. Optional local test material: never commit their numbers.
pub fn private() -> Option<Vec<(&'static str, HeadParams)>> {
    let text = std::fs::read_to_string(private_path()).ok()?;
    let private: Private = serde_json::from_str(&text).ok()?;
    let params = |set: &str| {
        let c = &private.sets.get(set)?.constants;
        let y0 = PROTOTYPE_CENTRE[1];
        let chin = match (c.chin, c.stretch_top, c.min_ratio) {
            (Some(chin), Some(line), Some(ratio)) => {
                Some(Chin { y: chin - y0, stretch_top: line - y0, min_ratio: ratio })
            }
            _ => None,
        };
        Some(HeadParams {
            cranium: Cranium { centre: PROTOTYPE_CENTRE, ..Cranium::default() },
            outline: HeadOutline {
                half_width: c.half_width,
                top: c.top - y0,
                height: c.height,
                exponent: c.exponent,
                chin,
                cheek_puff: c.puff,
            },
            ..HeadParams::default()
        })
    };
    Some(vec![("sheet", params("game-trace0")?), ("close-up", params("close-up-trace0")?)])
}
