//! `measure.silhouette` and `measure.readability` (VF-06): queries that read images and change nothing, so an eval's
//! read-only checker may call them. The maths is in [`crate::readability`].

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{asset_private, round};
use crate::colour::{hex, parse_hex};
use crate::image::{ImageRef, load};
use crate::readability::emote::{self, MIN_CONTRAST, Region, SIMILAR, SIZES, STREAM_DARK, Seen};
use crate::readability::silhouette::{self, Ground, MIN_DISTINCT, SIDE};

/// The most images one call reads: every pair is compared, so the work grows with the square.
const MAX_IMAGES: usize = 32;
/// The most expressions one readability call reads: a face's whole library at once (the kit's holds 36 besides its
/// visemes), still a few thousand small pairs.
const MAX_EXPRESSIONS: usize = 64;

/// How many other characters the spec checks a silhouette against (§32).
const SPEC_OTHERS: usize = 20;

/// A colour argument, parsed.
fn colour(text: &str, field: &str) -> Result<[u8; 3], OpError> {
    parse_hex(text).ok_or_else(|| {
        OpError::invalid(field, format!("'{text}' is not a colour")).hint("give colours as #rrggbb, such as #f9d3c5")
    })
}

/// A threshold argument: a finite number at least zero.
fn threshold(value: Option<f64>, default: f64, field: &str) -> Result<f64, OpError> {
    match value {
        None => Ok(default),
        Some(v) if v.is_finite() && v >= 0.0 => Ok(v),
        Some(v) => Err(OpError::invalid(field, format!("{v} is not a number of at least 0"))),
    }
}

/// A named image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamedImage {
    /// What the image shows: a character's name, or an expression's.
    pub name: String,
    /// The image: `{"asset": "<hash>"}` or `{"path": "renders/front.png"}`.
    pub image: ImageRef,
}

/// Measures how readable a character's silhouette is: its filled outline at 64 px against other characters', the overlap
/// with each (intersection over union), and its distinctiveness (one minus the overlap with the closest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasureSilhouette {
    /// The character: a full-body front render with a transparent background, or on a plain one.
    pub image: ImageRef,
    /// The characters to compare against, rendered the same way (the spec's check uses 20).
    pub others: Vec<NamedImage>,
    /// The plain background of opaque images, `#rrggbb`; by default an image's alpha, or its top-left pixel's colour
    /// when it is opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// The least distinctiveness that passes (default 0.08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_distinctiveness: Option<f64>,
}

impl Op for MeasureSilhouette {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.silhouette",
            summary: "Measure how readable a character's silhouette is: the filled outline at 64 px against other \
                      characters' (full-body front renders, transparent or on a plain background), the overlap with \
                      each and a distinctiveness score (1 - the closest overlap). Use it after a hair, outfit or \
                      accessory edit; check `within` and the closest other; changes nothing",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let min = threshold(self.min_distinctiveness, MIN_DISTINCT, "min_distinctiveness")?;
        let ground = self.background.as_deref().map(|t| colour(t, "background")).transpose()?.map(Ground::Colour);
        if self.others.is_empty() || self.others.len() > MAX_IMAGES {
            return Err(OpError::invalid("others", format!("{} others; give 1 to {MAX_IMAGES}", self.others.len()))
                .hint("render the other characters' front views the same way and pass them here"));
        }
        let shape = |image: &ImageRef, field: &str| -> Result<silhouette::Silhouette, OpError> {
            let pixels = load(ctx, image, field)?;
            silhouette::silhouette(&pixels, ground.unwrap_or_else(|| Ground::of(&pixels))).ok_or_else(|| {
                OpError::invalid(field, "no pixel of the image is the character")
                    .hint("render with a transparent background, or give the background colour")
            })
        };
        let mine = shape(&self.image, "image")?;
        let mut private = asset_private(ctx, &self.image);
        let mut rows = Vec::with_capacity(self.others.len());
        let mut closest: Option<(f64, &str)> = None;
        let mut sum = 0.0;
        for (i, other) in self.others.iter().enumerate() {
            let theirs = shape(&other.image, &format!("others.{i}.image"))?;
            private |= asset_private(ctx, &other.image);
            let overlap = mine.overlap(&theirs);
            sum += overlap;
            if closest.is_none_or(|(o, _)| overlap > o) {
                closest = Some((overlap, &other.name));
            }
            rows.push(json!({ "name": other.name, "overlap": round(overlap) }));
        }
        let (top, name) = closest.unwrap_or((0.0, ""));
        let distinctiveness = 1.0 - top;
        let within = distinctiveness >= min;
        let mut warnings = Vec::new();
        if self.others.len() < SPEC_OTHERS {
            warnings.push(json!({
                "code": "few_others",
                "detail": format!("compared against {} others; the spec's check uses {SPEC_OTHERS}", self.others.len()),
            }));
        }
        #[allow(clippy::cast_precision_loss)] // at most 32 others
        let mean = sum / self.others.len() as f64;
        let mut out = json!({
            "size": SIDE,
            "fill": round(mine.fill()),
            "distinctiveness": round(distinctiveness),
            "within": within,
            "min_distinctiveness": min,
            "closest": { "name": name, "overlap": round(top) },
            "mean_overlap": round(mean),
            "others": rows,
            "private": private,
            "warnings": warnings,
        });
        if !within {
            out["hint"] = json!(format!(
                "reads like {name} at a glance: exaggerate the hair's shape, add an asymmetric accessory or a \
                 signature shape (an ahoge, a hat, wings)"
            ));
        }
        Ok(Plan::answer(out))
    }
}

register_op!(MeasureSilhouette);

/// Measures whether a face reads at emote size: the expressions at 112, 56 and 28 px over a stream's dark ground, the
/// pairs that read the same at each size, and the eyes' and mouth's contrast against the skin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasureReadability {
    /// The expressions, the resting face first, each a head or face render of one size (2 to 64).
    pub expressions: Vec<NamedImage>,
    /// The sizes, pixels (default 112, 56 and 28, Twitch's emote sizes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<u32>>,
    /// The ground behind the emote, `#rrggbb` (default Twitch dark, `#18181b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// The eyes' box in the images' pixels, `[left, top, right, bottom]`, brows and lashes included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eyes: Option<Region>,
    /// The mouth's box, the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouth: Option<Region>,
    /// The lit skin colour, `#rrggbb`: with the boxes, the eyes' and mouth's contrast is measured against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
    /// The similarity above which two expressions read the same (default 0.94).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similar: Option<f64>,
    /// The least contrast of the eyes and the mouth against the skin, `ΔL*` (default 15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_contrast: Option<f64>,
}

impl Op for MeasureReadability {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.readability",
            summary: "Measure whether a face reads at emote size: its expressions (renders, the resting face first) \
                      at 112, 56 and 28 px over Twitch's dark ground, the pairs that read the same at each size, how \
                      many read apart, and with the eye and mouth boxes and the skin colour their contrast against the \
                      skin. Use it before making emotes or after a line-weight edit; check `within`; changes nothing",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    #[allow(clippy::too_many_lines)] // one report, built in order
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let n = self.expressions.len();
        if !(2..=MAX_EXPRESSIONS).contains(&n) {
            return Err(OpError::invalid("expressions", format!("{n} expressions; give 2 to {MAX_EXPRESSIONS}"))
                .hint("render the resting face and each expression the same way"));
        }
        let sizes = self.sizes.clone().unwrap_or_else(|| SIZES.to_vec());
        if sizes.is_empty() || sizes.iter().any(|s| !(8..=1024).contains(s)) {
            return Err(OpError::invalid("sizes", "sizes are 8 to 1024 px").hint("emote sizes are 112, 56 and 28"));
        }
        let ground = self.background.as_deref().map(|t| colour(t, "background")).transpose()?.unwrap_or(STREAM_DARK);
        let skin = self.skin.as_deref().map(|t| colour(t, "skin")).transpose()?;
        let similar = threshold(self.similar, SIMILAR, "similar")?;
        let min_contrast = threshold(self.min_contrast, MIN_CONTRAST, "min_contrast")?;
        for (field, region) in [("eyes", self.eyes), ("mouth", self.mouth)] {
            if let Some(r) = region
                && !(r.iter().all(|v| v.is_finite()) && r[2] > r[0] && r[3] > r[1])
            {
                return Err(OpError::invalid(field, format!("{r:?} is not a box"))
                    .hint("give [left, top, right, bottom] in the images' pixels"));
            }
        }
        // Each image is read, seen at every size, and dropped: only the small pictures are kept.
        let mut seen: Vec<Vec<Seen>> = Vec::with_capacity(n);
        let mut first_size = None;
        let mut private = false;
        for (i, e) in self.expressions.iter().enumerate() {
            let field = format!("expressions.{i}.image");
            let image = load(ctx, &e.image, &field)?;
            let dims = (image.width, image.height);
            if *first_size.get_or_insert(dims) != dims {
                return Err(OpError::invalid(field, format!("{}x{} is not the first image's size", dims.0, dims.1))
                    .hint("render every expression with the same framing and size"));
            }
            private |= asset_private(ctx, &e.image);
            seen.push(sizes.iter().map(|&s| emote::see(&image, s, ground, self.eyes, self.mouth)).collect());
        }
        let mut within = true;
        let mut reports = Vec::with_capacity(sizes.len());
        for (s, &size) in sizes.iter().enumerate() {
            let mut too_similar = Vec::new();
            let mut closest: Option<(f64, usize, usize)> = None;
            let mut close = vec![vec![false; n]; n];
            for i in 0..n {
                for j in i + 1..n {
                    let score = emote::similarity(&seen[i][s], &seen[j][s]);
                    if closest.is_none_or(|(c, _, _)| score > c) {
                        closest = Some((score, i, j));
                    }
                    if score > similar {
                        close[i][j] = true;
                        close[j][i] = true;
                        too_similar.push(json!({
                            "a": self.expressions[i].name,
                            "b": self.expressions[j].name,
                            "similarity": round(score),
                        }));
                    }
                }
            }
            // Greedy in the order given: one counts when it reads apart from all counted before it.
            let mut kept: Vec<usize> = Vec::new();
            for (i, row) in close.iter().enumerate() {
                if kept.iter().all(|&k| !row[k]) {
                    kept.push(i);
                }
            }
            within &= too_similar.is_empty();
            let mut report = json!({
                "size": size,
                "distinct": kept.len(),
                "too_similar": too_similar,
                "closest": closest.map(|(c, i, j)| json!({
                    "a": self.expressions[i].name,
                    "b": self.expressions[j].name,
                    "similarity": round(c),
                })),
            });
            if let Some(skin) = skin {
                for (key, region) in [("eye_contrast", &seen[0][s].eyes), ("mouth_contrast", &seen[0][s].mouth)] {
                    if let Some(region) = region {
                        let c = emote::contrast(region, skin);
                        within &= c >= min_contrast;
                        report[key] = json!(round(c));
                    }
                }
            }
            reports.push(report);
        }
        let mut notes = Vec::new();
        if self.eyes.is_none() || self.mouth.is_none() {
            notes.push("without the eye and mouth boxes the whole picture is compared, and no contrast is measured");
        } else if skin.is_none() {
            notes.push("give the skin colour to measure the eyes' and mouth's contrast");
        }
        Ok(Plan::answer(json!({
            "sizes": reports,
            "within": within,
            "similar": similar,
            "min_contrast": min_contrast,
            "background": hex(ground),
            "private": private,
            "notes": notes,
            "hint": (!within).then_some(
                "for emotes: a thicker lash line, bigger mouth shapes, a stronger blush, or an emote line weight"
            ),
        })))
    }
}

register_op!(MeasureReadability);
