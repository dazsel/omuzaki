//! The measure ops (design 5 and 6.2): `measure.face_outline`, `measure.feature_spacing` and `measure.likeness` read an
//! image and change nothing; `measure.face_overlay` writes its evidence image to the store; `measure.target.set` and
//! `measure.target.remove` keep a character's likeness targets.
//!
//! Every measure op takes a subject: a face (an image and its landmarks), or a likeness target, by its asset's hash or
//! by the character and name that link it.

mod measure;
mod overlay;
mod target;

use omuzaki_core::id::Slug;
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{OpCtx, OpError};
use omuzaki_store_api::ContentHash;
use omuzaki_store_api::{Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::colour::lab;
use crate::face::likeness::Side;
use crate::face::outline::{ANGLES, CHIN, OutlineProfile, Sector};
use crate::face::spacing::{FeatureSpacing, spacing};
use crate::face::{FaceLandmarks, FaceMeasure, MEASURE_VERSION, MeasureInput, Refusal, Warning, measure_face};
use crate::image::{ImageRef, Rgba8, load};
use crate::messages;
use crate::target::Target;

pub use measure::{MeasureFaceOutline, MeasureFeatureSpacing, MeasureLikeness};
pub use overlay::FaceOverlay;
pub use target::{TargetRemove, TargetSet};

/// One face to measure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaceSubject {
    /// The image: `{"asset": "<hash>"}` (what `render.preview` returns) or `{"path": "refs/front.png"}`.
    pub image: ImageRef,
    /// The landmarks, in the image's pixels.
    pub landmarks: FaceLandmarks,
    /// Snap the landmarks to the drawn marks (default true).
    #[serde(default = "yes")]
    pub refine: bool,
    /// Sectors of the outline to leave out (hair the occlusion rule cannot see, a hand), degrees in the face frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Sector>,
    /// The lit skin colour, `#rrggbb`, when the patches below the eyes are not plain skin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
}

fn yes() -> bool {
    true
}

/// A likeness target, by its asset or by the character that links it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TargetRef {
    /// The target asset itself.
    Asset {
        /// The target's content hash.
        asset: ContentHash,
    },
    /// The target a character links under a name (`characters/<character>/likeness/<name>`).
    Named {
        /// The character's id.
        character: String,
        /// The target's name, such as `front`.
        name: String,
    },
}

/// What a measure op measures: a face, or a likeness target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Subject {
    /// A likeness target.
    Target {
        /// Which target.
        target: TargetRef,
    },
    /// A face: an image and its landmarks.
    Face(FaceSubject),
}

/// A subject, measured.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    /// The fresh measurement, when the image was read now; `None` for a target read from its cache.
    pub measure: Option<FaceMeasure>,
    /// The image, when it was read.
    pub image: Option<Rgba8>,
    /// The outline (the measurement's, or the cache's).
    pub outline: OutlineProfile,
    /// The spacing.
    pub spacing: FeatureSpacing,
    /// Whether the input is private test material.
    pub private: bool,
    /// Whether the numbers came from a target's cache.
    pub from_cache: bool,
}

impl Resolved {
    /// The face as the likeness comparison sees it.
    pub(crate) fn side(&self) -> Side<'_> {
        Side {
            outline: &self.outline,
            spacing: &self.spacing,
            frame: self.measure.as_ref().map(|m| &m.frame),
            mouth_height: self.measure.as_ref().and_then(|m| m.landmarks.mouth_height),
        }
    }

    /// The warnings of the measurement.
    pub(crate) fn warnings(&self) -> &[Warning] {
        self.measure.as_ref().map_or(&[], |m| m.warnings.as_slice())
    }
}

/// Parses `#rrggbb` to CIELAB.
fn skin_lab(text: &str, field: &str) -> Result<[f64; 3], OpError> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    let bad = || OpError::invalid(field, format!("'{text}' is not a colour")).hint("give the skin colour as #rrggbb");
    if hex.len() != 6 || !hex.is_ascii() {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
    Ok(lab([byte(0)?, byte(2)?, byte(4)?]))
}

/// The refusal of a measurement, as the op's error, with the field under `at`.
fn refusal(at: &str, r: &Refusal) -> OpError {
    let field = if at.is_empty() { r.field.to_owned() } else { format!("{at}.{}", r.field) };
    let hint = match r.field {
        "landmarks" | "landmarks.mouth" | "landmarks.eye_left" | "landmarks.eye_right" | "landmarks.nose" => {
            "check the landmarks against the image (pixels, origin top-left, the character's right eye first)"
        }
        _ => "render with the `measure` preset at 160 px of eye distance or more, or exclude the sector",
    };
    OpError::invalid(field, messages::english(&r.text)).hint(hint)
}

/// Whether the stored asset `hash` is private test material; a file or an asset without provenance is not.
fn asset_private(ctx: &OpCtx<'_>, image: &ImageRef) -> bool {
    match image {
        ImageRef::Asset { asset } => ctx
            .service::<dyn Store>()
            .ok()
            .and_then(|store| store.provenance(asset).ok())
            .is_some_and(|p| p.private_test),
        ImageRef::File { .. } => false,
    }
}

/// Measures a face subject.
pub(crate) fn measure_subject(ctx: &OpCtx<'_>, face: &FaceSubject, at: &str) -> Result<Resolved, OpError> {
    let field = |name: &str| if at.is_empty() { name.to_owned() } else { format!("{at}.{name}") };
    let skin = face.skin.as_deref().map(|s| skin_lab(s, &field("skin"))).transpose()?;
    let image = load(ctx, &face.image, &field("image"))?;
    let input = MeasureInput { landmarks: face.landmarks, refine: face.refine, exclude: face.exclude.clone(), skin };
    let measure = measure_face(&image, &input).map_err(|r| refusal(at, &r))?;
    let spacing = spacing(&measure);
    Ok(Resolved {
        outline: measure.outline.clone(),
        spacing,
        measure: Some(measure),
        image: Some(image),
        private: asset_private(ctx, &face.image),
        from_cache: false,
    })
}

/// The path of a character's likeness link.
pub(crate) fn link_path(character: &str, name: &str) -> DocPath {
    DocPath::root().join("characters").join(character).join("likeness").join(name)
}

/// Checks a target name: a slug, since it is a key in the character's file.
pub(crate) fn check_name(name: &str, field: &str) -> Result<(), OpError> {
    Slug::new(name).map(|_| ()).map_err(|e| {
        OpError::invalid(field, format!("'{name}' can't name a likeness target: {e}")).hint("use a name like front")
    })
}

/// Resolves a target reference to the target's hash.
fn target_hash(ctx: &OpCtx<'_>, target: &TargetRef, at: &str) -> Result<ContentHash, OpError> {
    match target {
        TargetRef::Asset { asset } => Ok(*asset),
        TargetRef::Named { character, name } => {
            if ctx.doc.get(&DocPath::root().join("characters").join(character.as_str())).is_none() {
                return Err(OpError::not_found(format!("no character '{character}'")).at(&format!("{at}.character")));
            }
            let link = ctx.doc.get(&link_path(character, name)).ok_or_else(|| {
                OpError::not_found(messages::english(&messages::target_not_found(name.clone())))
                    .at(&format!("{at}.name"))
                    .hint("set it first with measure.target.set")
            })?;
            let asset = link.get("asset").and_then(Value::as_str).unwrap_or_default();
            asset
                .parse()
                .map_err(|_| OpError::internal(format!("the likeness link of {character}/{name} names no asset hash")))
        }
    }
}

/// Reads a target asset from the store.
pub(crate) fn read_target(ctx: &OpCtx<'_>, hash: &ContentHash, field: &str) -> Result<(Target, bool), OpError> {
    let store = ctx.service::<dyn Store>()?;
    let bytes = store.get(hash).map_err(|e| match e {
        StoreError::Missing(_) => OpError::not_found(format!("no likeness target {hash}")).at(field),
        other => OpError::internal(format!("the store could not read likeness target {hash}: {other}")),
    })?;
    let target = Target::from_bytes(&bytes)
        .map_err(|why| OpError::invalid(field, format!("{hash} is not a likeness target: {why}")))?;
    let private = store.provenance(hash).is_ok_and(|p| p.private_test);
    Ok((target, private))
}

/// Measures a subject: a face, or a target (measured again from its image when the store has it, else read from its
/// current cache).
pub(crate) fn resolve(ctx: &OpCtx<'_>, subject: &Subject, at: &str) -> Result<Resolved, OpError> {
    match subject {
        Subject::Face(face) => measure_subject(ctx, face, at),
        Subject::Target { target } => {
            let field = format!("{at}.target");
            let hash = target_hash(ctx, target, &field)?;
            let (t, private) = read_target(ctx, &hash, &field)?;
            let store = ctx.service::<dyn Store>()?;
            if store.contains(&t.image.asset) {
                let face = FaceSubject {
                    image: ImageRef::Asset { asset: t.image.asset },
                    landmarks: t.landmarks,
                    refine: t.refine,
                    exclude: t.exclude.clone(),
                    skin: t.skin.clone(),
                };
                let mut resolved = measure_subject(ctx, &face, at)?;
                resolved.private |= private;
                return Ok(resolved);
            }
            if !t.cache_is_current() {
                return Err(OpError::invalid(field, messages::english(&messages::stale_target(hash.to_string())))
                    .hint("import the reference image into the store again"));
            }
            Ok(Resolved {
                measure: None,
                image: None,
                outline: t.cached_outline(),
                spacing: t.cached_spacing(),
                private,
                from_cache: true,
            })
        }
    }
}

/// Rounds to 6 decimals, so outputs read cleanly and stay byte-stable.
pub(crate) fn round(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The JSON of a measured face's frame, landmarks, outline and scalars (design 5's `measure.face_outline` output).
pub(crate) fn outline_json(r: &Resolved) -> Value {
    let o = &r.outline;
    let mut out = json!({
        "measure_version": MEASURE_VERSION,
        "private": r.private,
        "outline": {
            "from_deg": crate::face::outline::FROM_DEG,
            "step_deg": crate::face::outline::STEP_DEG,
            "r": o.r.iter().zip(o.state).map(|(r, s)| {
                (s == crate::face::outline::SampleState::Ok).then_some(r.map(round)).flatten()
            }).collect::<Vec<_>>(),
            "state": o.state.to_vec(),
            "beyond": o.beyond.to_vec(),
        },
        "width_at_eyes": r.spacing.width_at_eyes.map(round),
        "width_max": round(r.spacing.width_max),
        "aspect": round(r.spacing.aspect),
        "asymmetry": o.asymmetry().map(round),
        "coverage": round(o.coverage()),
        "line_width": round(o.line_width),
        "warnings": r.warnings(),
    });
    if r.from_cache {
        out["from_cache"] = json!(true);
    }
    if let Some(m) = &r.measure {
        let (drop, point) = m.chin();
        out["image"] = json!({ "hash": m.hash.to_string(), "width": m.size[0], "height": m.size[1] });
        out["frame"] = json!({
            "origin": m.frame.origin.map(round),
            "x_axis": m.frame.u.map(round),
            "iod_px": round(m.frame.iod_px),
            "roll_deg": round(m.frame.roll_deg()),
            "downscale": m.downscale,
        });
        out["landmarks"] = landmarks_json(m);
        out["chin"] = json!({ "drop": round(drop), "point": point.map(round) });
        out["line_contrast"] = json!(m.line_contrast.map(round));
    } else {
        out["chin"] = json!({ "drop": o.ok(CHIN).map(round) });
    }
    debug_assert_eq!(o.r.len(), ANGLES);
    out
}

/// The landmarks, as given and as used.
fn landmarks_json(m: &FaceMeasure) -> Value {
    let one = |s: &crate::face::snap::Snapped| json!({ "given": s.given.map(round), "used": s.used.map(round), "snapped": s.snapped });
    let lm = &m.landmarks;
    let mut out = json!({ "eye_right": one(&lm.eye_right), "eye_left": one(&lm.eye_left) });
    if let Some(mouth) = &lm.mouth {
        out["mouth"] = one(mouth);
    }
    if let Some(nose) = &lm.nose {
        out["nose"] = one(nose);
    }
    out
}

/// An op error with its field set, for errors built without one.
trait At {
    fn at(self, field: &str) -> Self;
}

impl At for OpError {
    fn at(mut self, field: &str) -> Self {
        self.field = Some(field.to_owned());
        self
    }
}
