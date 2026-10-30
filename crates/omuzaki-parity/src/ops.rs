//! The `parity.*` ops: pixel parity of a render against a reference, frame for frame, until the exact difference map
//! is 0 (see the crate's notes).
//!
//! - `parity.compare`: one render against one reference: the exact map's count, the metrics, every region, the worst
//!   regions and tiles with hints, and optionally how far the camera is off.
//! - `parity.overlay`: an overlay (onion, exact, difference, delta E, signed, wipe, checker, edges, silhouette, regions,
//!   sheet), stored as a PNG with the reference recorded as its input.
//! - `parity.align`: the transform and camera change that would line the render up with the reference.
//! - `parity.timing`: how two frame sequences line up in time.
//! - `parity.series`: signal parity for tracking, rig and motion curves.
//! - `parity.provenance`: the recreate-never-copy check on its own.
//! - `parity.target.set` and `parity.target.remove`: a named reference (frames, points, regions, backdrop) under
//!   `parity/targets/<name>`, so the loop is one call.
//! - `parity.loop`: the call an agent makes every iteration: the check, the timing, every frame's score, the worst
//!   frames and regions with what to change, the evidence sheet, the change since the previous run, and `done`.

use std::collections::BTreeSet;
use std::io::Read as _;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, ContentHash, Provenance as StoreProvenance, Source, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::align::{AlignMethod, align};
use crate::colour::parse_hex;
use crate::image::{ImageRef, Rgba8, load};
use crate::metrics::{border_colour, silhouette};
use crate::overlay::{Mode, Picture, draw};
use crate::provenance::{self, Provenance, RULE, TileHash, tile_hashes};
use crate::raster::LabImage;
use crate::regions::{Points, Region, RegionSpec, Shape, fill, found_regions, merge_given};
use crate::score::{AlignedScore, Metric, PairScore, ScoreOptions, Worst, round, score_pair, srgb};
use crate::series::{Series, TimeAlign, score_series};
use crate::timing::{descriptor, descriptor_cost, timing};

/// Checks a target name: a slug, since it is a key in the document.
fn check_name(name: &str, field: &str) -> Result<(), OpError> {
    omuzaki_core::id::Slug::new(name).map(|_| ()).map_err(|e| {
        OpError::invalid(field, format!("'{name}' can't name a parity target: {e}")).hint("use a name like eye-macro")
    })
}

/// The most frames one call reads.
pub const MAX_FRAMES: usize = 4000;

/// One image, a list, or a numbered file pattern.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Frames {
    /// Images in order.
    List {
        /// The images, `{"asset": …}` or `{"path": …}` each.
        frames: Vec<ImageRef>,
    },
    /// Numbered files, such as `ours/%04d.png` (`%d` or `%0Nd`), from `start`, `count` of them, every `step`th.
    Pattern {
        /// The path with one `%d` or `%0Nd`.
        pattern: String,
        /// The first number (default 0).
        #[serde(default)]
        start: u32,
        /// How many frames.
        count: u32,
        /// The step between numbers (default 1).
        #[serde(default = "one")]
        step: u32,
    },
    /// One image.
    One(ImageRef),
}

fn one() -> u32 {
    1
}

impl Frames {
    /// The frames' image references.
    fn refs(&self, field: &str) -> Result<Vec<ImageRef>, OpError> {
        let out = match self {
            Self::One(r) => vec![r.clone()],
            Self::List { frames } => frames.clone(),
            Self::Pattern { pattern, start, count, step } => {
                let (head, width, tail) = split_pattern(pattern).ok_or_else(|| {
                    OpError::invalid(field, format!("'{pattern}' has no %d or %0Nd for the frame number"))
                        .hint("write the pattern like frames/%04d.png")
                })?;
                (0..*count)
                    .map(|k| ImageRef::File {
                        path: format!(
                            "{head}{:0width$}{tail}",
                            u64::from(*start) + u64::from(k) * u64::from((*step).max(1))
                        ),
                    })
                    .collect()
            }
        };
        if out.is_empty() {
            return Err(OpError::invalid(field, "no frames"));
        }
        if out.len() > MAX_FRAMES {
            return Err(OpError::invalid(field, format!("{} frames is more than {MAX_FRAMES}", out.len()))
                .hint("score a part, or use a stride"));
        }
        Ok(out)
    }
}

/// `(before, zero-padded width, after)` of a `%d` or `%0Nd` pattern.
fn split_pattern(p: &str) -> Option<(String, usize, String)> {
    let at = p.find('%')?;
    let rest = &p[at + 1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    let tail = after.strip_prefix('d')?;
    let width = digits.trim_start_matches('0').parse().unwrap_or(0);
    Some((p[..at].to_owned(), width, tail.to_owned()))
}

/// Points for one frame or for each frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FramePoints {
    /// One set of points for every frame.
    Same(Points),
    /// One set per frame, in order.
    Each(Vec<Points>),
}

impl FramePoints {
    fn get(&self, i: usize) -> Option<&Points> {
        match self {
            Self::Same(p) => Some(p),
            Self::Each(v) => v.get(i).or_else(|| v.last()),
        }
    }
}

/// What every image comparison takes beside the two images.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Setup {
    /// Points on our render (`eye_right`, `eye_left`, `mouth`, `chin`, as `render.preview` returns in
    /// `face_landmarks`; any other names only align).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ours_points: Option<Points>,
    /// The same points on the reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_points: Option<Points>,
    /// Regions given by hand, in the reference's pixels; each replaces a found region of its name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionSpec>,
    /// Regions left out of every score (a watermark, a caption that isn't part of what is matched). Reported as
    /// `ignored`; parity holds only for what is scored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<RegionSpec>,
    /// The colour transparent pixels are composited over, `#rrggbb` (default: the reference's border colour when it is
    /// opaque, else black).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<String>,
    /// How far (CIE76) from the backdrop an opaque pixel must be to count as the character (default 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silhouette_key: Option<f32>,
}

/// A pair ready to score.
struct Prepared {
    ours: LabImage,
    reference: LabImage,
    silhouettes: (Vec<bool>, Vec<bool>),
    regions: Vec<Region>,
    keep: Vec<bool>,
    backdrop: [u8; 3],
}

impl Prepared {
    fn silhouettes(&self) -> (&[bool], &[bool]) {
        (&self.silhouettes.0, &self.silhouettes.1)
    }
}

/// Reads a mask image as a region mask of the reference's size.
fn mask_region(ctx: &OpCtx<'_>, image: &ImageRef, w: usize, h: usize, field: &str) -> Result<Vec<bool>, OpError> {
    let m = load(ctx, image, field)?;
    if (m.width as usize, m.height as usize) != (w, h) {
        return Err(OpError::invalid(field, format!("the mask is {}x{}, the reference {w}x{h}", m.width, m.height)));
    }
    Ok(m.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3] > 127 && 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]) > 127.0)
        .collect())
}

fn given(ctx: &OpCtx<'_>, specs: &[RegionSpec], w: usize, h: usize, field: &str) -> Result<Vec<Region>, OpError> {
    specs
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let mask = match &s.shape {
                Shape::Mask { image } => mask_region(ctx, image, w, h, &format!("{field}[{k}].image"))?,
                other => fill(other, w, h),
            };
            Ok(Region { name: s.name.clone(), mask, source: "given" })
        })
        .collect()
}

/// Composites, keys, finds and merges the regions of a pair.
fn prepare(
    ctx: &OpCtx<'_>,
    ours: &Rgba8,
    reference: &Rgba8,
    setup: &Setup,
    ours_points: Option<&Points>,
    reference_points: Option<&Points>,
) -> Result<Prepared, OpError> {
    if (ours.width, ours.height) != (reference.width, reference.height) {
        return Err(OpError::invalid(
            "ours",
            format!(
                "ours is {}x{} and the reference {}x{}: pixel parity compares one pixel grid",
                ours.width, ours.height, reference.width, reference.height
            ),
        )
        .hint(format!(
            "render at the reference's size (render.preview size: [{}, {}])",
            reference.width, reference.height
        )));
    }
    let backdrop = if let Some(text) = &setup.backdrop {
        parse_hex(text).ok_or_else(|| {
            OpError::invalid("backdrop", format!("'{text}' is not a colour")).hint("give it as #rrggbb")
        })?
    } else {
        let probe = LabImage::from_rgba(reference, [0, 0, 0]);
        if probe.has_alpha { [0, 0, 0] } else { srgb(border_colour(&probe).map(f64::from)) }
    };
    let reference = LabImage::from_rgba(reference, backdrop);
    let ours = LabImage::from_rgba(ours, backdrop);
    let (w, h) = (reference.w, reference.h);
    let key = setup.silhouette_key.unwrap_or(6.0);
    #[allow(clippy::cast_possible_truncation)] // CIELAB fits an f32
    let back = crate::colour::lab(backdrop).map(|v| v as f32);
    let sil_o = silhouette(&ours, back, key);
    let sil_r = silhouette(&reference, back, key);
    #[allow(clippy::cast_precision_loss)] // the image side
    let outline = (w.max(h) as f32 / 400.0).max(2.0);
    let mut regions = found_regions(w, h, &sil_o, &sil_r, ours_points, reference_points, outline);
    merge_given(&mut regions, given(ctx, &setup.regions, w, h, "regions")?);
    let mut keep = vec![true; w * h];
    for r in given(ctx, &setup.ignore, w, h, "ignore")? {
        for (k, m) in keep.iter_mut().zip(&r.mask) {
            *k &= !m;
        }
    }
    Ok(Prepared { ours, reference, silhouettes: (sil_o, sil_r), regions, keep, backdrop })
}

/// The score with its alignment diagnosis, when asked.
fn scored(
    p: &Prepared,
    options: &ScoreOptions,
    align_method: Option<AlignMethod>,
    ours_points: Option<&Points>,
    reference_points: Option<&Points>,
) -> PairScore {
    let mut s = score_pair(&p.ours, &p.reference, &p.keep, p.silhouettes(), &p.regions, options);
    if let Some(method) = align_method {
        let a = align(&p.ours, &p.reference, &p.silhouettes.0, &p.silhouettes.1, ours_points, reference_points, method);
        let moved = p.ours.warped(&a.transform, p.reference.w, p.reference.h, p.backdrop);
        let de = crate::metrics::delta_e_map(&moved, &p.reference);
        let keep = Some(p.keep.as_slice());
        let after = crate::metrics::stats(&de, keep).mean;
        let before = s.metrics.iter().find(|m| m.name == "delta_e_mean").map_or(0.0, |m| m.value);
        s.alignment = Some(AlignedScore {
            within_jnd_aligned: round(crate::metrics::share(&de, keep, |d| d <= 2.3)),
            delta_e_mean_aligned: round(after),
            framing_explains: round(if before > 0.0 { ((before - after) / before).max(0.0) } else { 0.0 }),
            alignment: a,
        });
    }
    s
}

/// The provenance check of the open document (and the named files) against these reference images.
fn check_provenance(
    ctx: &OpCtx<'_>,
    references: &[(String, Rgba8)],
    built_from: &[ImageRef],
) -> Result<Provenance, OpError> {
    let hashes: BTreeSet<ContentHash> = references.iter().map(|(_, r)| r.hash).collect();
    let tiles: Vec<TileHash> = references.iter().flat_map(|(name, r)| tile_hashes(r, name)).collect();
    let files = built_from
        .iter()
        .enumerate()
        .map(|(k, r)| {
            let name = match r {
                ImageRef::Asset { asset } => format!("asset {asset}"),
                ImageRef::File { path } => path.clone(),
                ImageRef::Data { .. } => format!("inline image {k}"),
            };
            load(ctx, r, &format!("built_from[{k}]")).map(|img| (name, img))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let doc = ctx.doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null);
    let store = ctx.service::<dyn Store>().ok();
    Ok(provenance::check(&doc, store, &hashes, &tiles, &files))
}

/// The refusal of a score built from reference pixels.
fn refused(p: &Provenance) -> OpError {
    OpError::invalid(
        "ours",
        format!("refused: the render is built from the reference's own pixels: {}", p.violations.join("; ")),
    )
    .hint(RULE)
}

/// Encodes a picture as a PNG and stores it as evidence made from the references (so the check refuses any render
/// that uses it).
fn store_evidence(
    ctx: &OpCtx<'_>,
    picture: &Picture,
    inputs: Vec<ContentHash>,
    what: &str,
) -> Result<ContentHash, OpError> {
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    #[allow(clippy::cast_possible_truncation)] // within the 8192 px the measures read
    image::ImageEncoder::write_image(
        encoder,
        &picture.rgba(),
        picture.w as u32,
        picture.h as u32,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| OpError::internal(format!("encoding the {what}: {e}")))?;
    store_bytes(ctx, &png, inputs, what)
}

fn store_bytes(ctx: &OpCtx<'_>, bytes: &[u8], inputs: Vec<ContentHash>, what: &str) -> Result<ContentHash, OpError> {
    let store = ctx.service::<dyn Store>()?;
    let private = inputs.iter().any(|h| store.provenance(h).is_ok_and(|p| p.private_test));
    let provenance = StoreProvenance {
        source: Source::Generated {
            tool: "parity".to_owned(),
            model: what.to_owned(),
            plan: None,
            prompt: None,
            seed: None,
            inputs,
        },
        author: None,
        licence: "LicenseRef-Omuzaki-Evidence".to_owned(),
        attribution: None,
        commercial: Commercial::No,
        consent: None,
        private_test: private,
    };
    store.put(bytes, provenance).map_err(|e| OpError::internal(format!("storing the {what}: {e}")))
}

/// The options shared by the scoring ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScoreArgs {
    /// The largest per-channel byte difference that still counts as a match (default 0: exact, the stop rule).
    #[serde(default)]
    pub tolerance_levels: u8,
    /// How many worst tiles to list (default 8).
    #[serde(default = "eight")]
    pub tiles: usize,
}

impl Default for ScoreArgs {
    fn default() -> Self {
        Self { tolerance_levels: 0, tiles: 8 }
    }
}

fn eight() -> usize {
    8
}

impl ScoreArgs {
    fn options(&self) -> ScoreOptions {
        ScoreOptions { tolerance_levels: self.tolerance_levels, tiles: self.tiles, tile_px: 0 }
    }
}

// ---------------------------------------------------------------------------------------------------------------------

/// Scores one render against one reference, pixel for pixel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}},
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, "align": "auto", "ours_points": {"eye_right": [5.0, 7.0], "eye_left": [8.0, 7.0]}, "reference_points": {"eye_right": [4.0, 7.0], "eye_left": [7.0, 7.0]}}
]))]
pub struct Compare {
    /// Our render.
    pub ours: ImageRef,
    /// The reference (only measured, never used).
    pub reference: ImageRef,
    /// Points, regions, ignored areas, backdrop.
    #[serde(flatten)]
    pub setup: Setup,
    /// Also find how far the camera is off and score again lined up (a diagnosis: `done` is always on the render as
    /// rendered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<AlignMethod>,
    /// Tolerance and tiles.
    #[serde(flatten)]
    pub score: ScoreArgs,
}

impl Op for Compare {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.compare",
            summary: "Score a render against a reference pixel for pixel (same size): done only when the exact \
                      difference map is 0; pixels off, CIEDE2000, SSIM, PSNR, GMSD, silhouette IoU and edge offset, \
                      for the frame and each region (found: silhouette, background, outline, and from eye points the \
                      eyes, mouth, face, hair and body; or given), the worst regions and tiles with what to change, \
                      and with `align` how far the camera is off. Refuses renders built from reference pixels",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let ours = load(ctx, &self.ours, "ours")?;
        let reference = load(ctx, &self.reference, "reference")?;
        let check = check_provenance(ctx, &[("the reference".to_owned(), reference.clone())], &[])?;
        if !check.clean {
            return Err(refused(&check));
        }
        let (op, rp) = (self.setup.ours_points.as_ref(), self.setup.reference_points.as_ref());
        let p = prepare(ctx, &ours, &reference, &self.setup, op, rp)?;
        let mut s = serde_json::to_value(scored(&p, &self.score.options(), self.align, op, rp))
            .map_err(|e| OpError::internal(format!("writing the score: {e}")))?;
        s["backdrop"] = json!(crate::colour::hex(p.backdrop));
        s["provenance"] = json!(check);
        s["exact_textured_tiles"] = json!(provenance::exact_textured_tiles(&p.ours, &p.reference).map(round));
        Ok(Plan::answer(s))
    }
}

register_op!(Compare);

/// Draws an overlay of a render on its reference and stores it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, "overlay": {"mode": "sheet"}},
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, "overlay": {"mode": "wipe", "position": 0.3}}
]))]
pub struct Overlay {
    /// Our render.
    pub ours: ImageRef,
    /// The reference.
    pub reference: ImageRef,
    /// Which overlay.
    pub overlay: Mode,
    /// Points, regions, ignored areas, backdrop.
    #[serde(flatten)]
    pub setup: Setup,
    /// Draw ours lined up with the reference first (by this method), to see what is left once the camera matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<AlignMethod>,
}

impl Op for Overlay {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.overlay",
            summary: "Draw a render over its reference and store the PNG: onion (opacity), exact (black only where \
                      every byte matches: the stop rule's map), difference, delta_e heat, signed lightness, wipe, \
                      checker, edges (reference cyan, ours magenta), silhouette (ours-only red, reference-only blue), \
                      regions, or a six-panel sheet. Evidence only: it holds reference pixels, and a render that uses \
                      it is refused",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let ours = load(ctx, &self.ours, "ours")?;
        let reference = load(ctx, &self.reference, "reference")?;
        let (op, rp) = (self.setup.ours_points.as_ref(), self.setup.reference_points.as_ref());
        let mut p = prepare(ctx, &ours, &reference, &self.setup, op, rp)?;
        let mut transform = None;
        if let Some(method) = self.align {
            let a = align(&p.ours, &p.reference, &p.silhouettes.0, &p.silhouettes.1, op, rp, method);
            p.ours = p.ours.warped(&a.transform, p.reference.w, p.reference.h, p.backdrop);
            p.silhouettes.0 = p.ours.alpha.v.iter().map(|a| *a > 0.5).collect();
            transform = Some(a);
        }
        let picture = draw(&self.overlay, &p.ours, &p.reference, p.silhouettes(), &p.regions);
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "dry_run": true, "size": [picture.w, picture.h] })));
        }
        let asset = store_evidence(ctx, &picture, vec![reference.hash, ours.hash], "overlay")?;
        Ok(Plan::answer(json!({
            "asset": asset.to_string(),
            "size": [picture.w, picture.h],
            "aligned": transform,
            "regions": p.regions.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        })))
    }
}

register_op!(Overlay);

/// Finds how far the camera is off: the transform that lines the render up with the reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}},
    {"ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, "method": "points", "ours_points": {"eye_right": [5.0, 7.0], "eye_left": [8.0, 7.0]}, "reference_points": {"eye_right": [4.0, 7.0], "eye_left": [7.0, 7.0]}}
]))]
pub struct Align {
    /// Our render.
    pub ours: ImageRef,
    /// The reference.
    pub reference: ImageRef,
    /// How (default `auto`: points, else silhouettes, then a pixel search).
    #[serde(default)]
    pub method: AlignMethod,
    /// Points, regions, ignored areas, backdrop.
    #[serde(flatten)]
    pub setup: Setup,
}

impl Op for Align {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.align",
            summary: "Find the similarity transform (shift, scale, roll) that lines a render up with its reference, \
                      from matching points, the silhouettes or a coarse-to-fine pixel search, with the residual before \
                      and after and the camera change it asks for in pixels and in words. Change the camera, render \
                      again and compare: parity is scored on the render as rendered",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let ours = load(ctx, &self.ours, "ours")?;
        let reference = load(ctx, &self.reference, "reference")?;
        let (op, rp) = (self.setup.ours_points.as_ref(), self.setup.reference_points.as_ref());
        let p = prepare(ctx, &ours, &reference, &self.setup, op, rp)?;
        let a = align(&p.ours, &p.reference, &p.silhouettes.0, &p.silhouettes.1, op, rp, self.method);
        Ok(Plan::answer(serde_json::to_value(a).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(Align);

/// Lines two frame sequences up in time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"ours": {"frames": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALMicffsWoUk7OcURaxLp1okwcJFv0mBUxEhMtAAACyoD/XmB8mkAAAAASUVORK5CYII="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALGj8ffsWoYk4OcURZRJZ1okwcJFp0mBUxEhMtAAAAdAD/Z3LGz8AAAAASUVORK5CYII="}]}, "reference": {"frames": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALMicffsWoUk7OcURaxLp1okwcJFv0mBUxEhMtAAACyoD/XmB8mkAAAAASUVORK5CYII="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALGj8ffsWoYk4OcURZRJZ1okwcJFp0mBUxEhMtAAAAdAD/Z3LGz8AAAAASUVORK5CYII="}]}, "max_lag": 2}
]))]
pub struct TimingOp {
    /// Our frames.
    pub ours: Frames,
    /// The reference's frames.
    pub reference: Frames,
    /// The largest fixed offset tried, in frames (default 120).
    #[serde(default = "lag")]
    pub max_lag: usize,
}

fn lag() -> usize {
    120
}

/// The timing descriptors of a sequence, read one frame at a time.
fn descriptors(ctx: &OpCtx<'_>, refs: &[ImageRef], field: &str) -> Result<Vec<Vec<f32>>, OpError> {
    refs.iter()
        .enumerate()
        .map(|(k, r)| {
            load(ctx, r, &format!("{field}[{k}]")).map(|img| descriptor(&LabImage::from_rgba(&img, [0, 0, 0])))
        })
        .collect()
}

impl Op for TimingOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.timing",
            summary: "Line two frame sequences up in time: the fixed offset that fits best, and a time warp that maps \
                      every frame of ours to a reference frame, with the drift and the playback rate it shows, so \
                      frames compare 1:1 and the animation's timing can be fixed",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let a = descriptors(ctx, &self.ours.refs("ours")?, "ours")?;
        let b = descriptors(ctx, &self.reference.refs("reference")?, "reference")?;
        let cost = |i: usize, j: usize| descriptor_cost(&a[i], &b[j]);
        let t = timing(&cost, a.len(), b.len(), self.max_lag);
        Ok(Plan::answer(serde_json::to_value(t).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(TimingOp);

/// A series inline, or a JSON file of the same shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SeriesSource {
    /// The series itself.
    Inline(Series),
    /// A JSON file holding `{"rate_hz": …, "channels": {…}}`.
    File {
        /// The file's path.
        path: String,
    },
}

impl SeriesSource {
    fn read(&self, ctx: &OpCtx<'_>, field: &str) -> Result<Series, OpError> {
        match self {
            Self::Inline(s) => Ok(s.clone()),
            Self::File { path } => {
                let checked = ctx.fs_access(path, omuzaki_ops_api::FsMode::Read)?;
                let mut text = String::new();
                std::io::Read::read_to_string(&mut checked.open_read()?.take(256 << 20), &mut text)
                    .map_err(|e| OpError::internal(format!("reading {checked}: {e}")))?;
                serde_json::from_str(&text).map_err(|e| OpError::invalid(field, format!("{path} is not a series: {e}")))
            }
        }
    }
}

/// Signal parity: tracking, rig and motion curves, any per-frame numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"ours": {"rate_hz": 60.0, "channels": {"jawOpen": [0.0, 0.2, 0.5]}}, "reference": {"rate_hz": 60.0, "channels": {"jawOpen": [0.0, 0.25, 0.5]}}}
]))]
pub struct SeriesOp {
    /// Ours.
    pub ours: SeriesSource,
    /// The reference.
    pub reference: SeriesSource,
    /// The largest difference that still counts as a match (default 0: exact).
    #[serde(default)]
    pub tolerance: f64,
    /// How to line them up in time (default `offset`).
    #[serde(default)]
    pub align: TimeAlign,
    /// The largest offset tried, seconds (default 2).
    #[serde(default = "two")]
    pub max_lag_s: f64,
}

fn two() -> f64 {
    2.0
}

impl Op for SeriesOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.series",
            summary: "Signal parity for tracking, rig and motion curves (blendshapes, head pose, joint angles, any \
                      per-frame numbers): lines ours up with the reference in time (offset or warp), then per channel \
                      samples off, RMSE, max error, correlation, amplitude and lag, the worst channels and half-second \
                      windows with what to change; done only when every sample matches within the tolerance",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let ours = self.ours.read(ctx, "ours")?;
        let reference = self.reference.read(ctx, "reference")?;
        if !(ours.rate_hz > 0.0 && reference.rate_hz > 0.0) {
            return Err(OpError::invalid("rate_hz", "the sample rates must be above 0"));
        }
        if (ours.rate_hz - reference.rate_hz).abs() > 1e-9 {
            return Err(OpError::invalid(
                "ours.rate_hz",
                format!("ours is at {} Hz, the reference at {} Hz", ours.rate_hz, reference.rate_hz),
            )
            .hint("resample one to the other's rate first"));
        }
        let s = score_series(&ours, &reference, self.tolerance.max(0.0), self.align, self.max_lag_s.max(0.0));
        Ok(Plan::answer(serde_json::to_value(s).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(SeriesOp);

/// The recreate-never-copy check on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}},
    {"reference": {"frames": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALMicffsWoUk7OcURaxLp1okwcJFv0mBUxEhMtAAACyoD/XmB8mkAAAAASUVORK5CYII="}]}, "built_from": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALGj8ffsWoYk4OcURZRJZ1okwcJFp0mBUxEhMtAAAAdAD/Z3LGz8AAAAASUVORK5CYII="}]}
]))]
pub struct ProvenanceOp {
    /// The reference's frames.
    pub reference: Frames,
    /// Image files or assets the render was built from (textures, layers), checked for copies of the reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub built_from: Vec<ImageRef>,
}

impl Op for ProvenanceOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.provenance",
            summary: "Check that no reference pixel went into the project: refuses when the document uses a \
                      reference image, an asset generated from one (overlays included), or an image (on its own or \
                      inside a model) that is a near-copy of a reference frame or a tile of one; also checks the \
                      image files named in built_from",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let refs = self.reference.refs("reference")?;
        let frames = refs
            .iter()
            .enumerate()
            .map(|(k, r)| load(ctx, r, &format!("reference[{k}]")).map(|img| (format!("frame {k}"), img)))
            .collect::<Result<Vec<_>, _>>()?;
        let p = check_provenance(ctx, &frames, &self.built_from)?;
        Ok(Plan::answer(serde_json::to_value(p).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(ProvenanceOp);

// ---------------------------------------------------------------------------------------------------------------------

/// A named reference: what `parity.loop` scores against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetDef {
    /// The reference's frames.
    pub reference: Frames,
    /// Points on the reference: one set for every frame, or one per frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<FramePoints>,
    /// Regions given by hand, in the reference's pixels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<RegionSpec>,
    /// Regions left out of every score.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<RegionSpec>,
    /// The backdrop, `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<String>,
    /// The frames a second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// What the reference is, in words (never its pixels).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn target_path(name: &str) -> DocPath {
    DocPath::root().join("parity").join("targets").join(name)
}

/// Sets a named parity target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"name": "bust", "target": {"reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, "points": {"eye_right": [4.0, 7.0], "eye_left": [7.0, 7.0]}, "note": "a 12 px synthetic bust"}}
]))]
pub struct TargetSet {
    /// The target's name, such as `eye-macro` or `t1-short`.
    pub name: String,
    /// What it is.
    pub target: TargetDef,
}

impl Op for TargetSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.target.set",
            summary: "Name a parity target under parity/targets/<name>: the reference frames (assets or files), the \
                      points on them, the regions and ignored areas, the backdrop and the frame rate, so parity.loop \
                      scores every iteration with one call. The reference is only measured",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        check_name(&self.name, "name")?;
        self.target.reference.refs("target.reference")?;
        let value = serde_json::to_value(&self.target).map_err(|e| OpError::internal(e.to_string()))?;
        let parity = DocPath::root().join("parity");
        let targets = parity.join("targets");
        let patch = match (ctx.doc.get(&parity), ctx.doc.get(&targets)) {
            (None, _) => Patch::new().with(parity, None, Some(json!({ "targets": { self.name.clone(): value } }))),
            (Some(Value::Object(_)), None) => {
                Patch::new().with(targets, None, Some(json!({ self.name.clone(): value })))
            }
            (Some(Value::Object(_)), Some(Value::Object(_))) => {
                let path = target_path(&self.name);
                let before = ctx.doc.get(&path).cloned();
                Patch::new().with(path, before, Some(value))
            }
            _ => return Err(OpError::conflict("parity/targets is not a table of targets")),
        };
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "name": self.name, "set": true }));
        Ok(plan)
    }
}

register_op!(TargetSet);

/// Removes a named parity target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"name": "bust"}
], "x-omuzaki-fixture" = "parity"))]
pub struct TargetRemove {
    /// The target's name.
    pub name: String,
}

impl Op for TargetRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.target.remove",
            summary: "Remove a named parity target",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = target_path(&self.name);
        let before = ctx
            .doc
            .get(&path)
            .cloned()
            .ok_or_else(|| OpError::not_found(format!("no parity target '{}'", self.name)))?;
        let mut plan = Plan::edit(Patch::new().with(path, Some(before), None));
        plan.output = Some(json!({ "name": self.name, "removed": true }));
        Ok(plan)
    }
}

register_op!(TargetRemove);

// ---------------------------------------------------------------------------------------------------------------------

/// What `parity.loop` scores against: a named target, or one given inline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TargetArg {
    /// A target set with `parity.target.set`.
    Named {
        /// Its name.
        name: String,
    },
    /// A target given here.
    Inline(TargetDef),
}

/// One iteration of the parity loop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {"target": {"reference": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}}, "ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "made_with": "the kit, render.preview at 12 px"},
    {"target": {"reference": {"frames": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAALklEQVR42mPUkJNhIASYGIgALHDWvn2L0OScnOJIMIlE60QYuCgyaTAqYiQmWgAUhAP9dSFYgQAAAABJRU5ErkJggg=="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALMicffsWoUk7OcURaxLp1okwcJFv0mBUxEhMtAAACyoD/XmB8mkAAAAASUVORK5CYII="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALGj8ffsWoYk4OcURZRJZ1okwcJFp0mBUxEhMtAAAAdAD/Z3LGz8AAAAASUVORK5CYII="}]}, "fps": 30.0}, "ours": {"frames": [{"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALMicffsWoUk7OcURaxLp1okwcJFv0mBUxEhMtAAACyoD/XmB8mkAAAAASUVORK5CYII="}, {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAL0lEQVR42mPUkJNhIASYGIgALGj8ffsWoYk4OcURZRJZ1okwcJFp0mBUxEhMtAAAAdAD/Z3LGz8AAAAASUVORK5CYII="}]}, "made_with": "two frames drawn for the example"},
    {"target": {"name": "bust"}, "ours": {"data": "iVBORw0KGgoAAAANSUhEUgAAAAwAAAAMCAIAAADZF8uwAAAAMElEQVR42mPUkJNhIASYGIgALMicBRVBaNIJHeuINYl064zcotDlqWwdnRUxEhMtALzXBed3OqrBAAAAAElFTkSuQmCC"}, "made_with": "the kit"}
], "x-omuzaki-fixture" = "parity"))]
pub struct Loop {
    /// The reference to match.
    pub target: TargetArg,
    /// Our render: one frame or a sequence.
    pub ours: Frames,
    /// How the render was made, in a line: the tools, ops and assets. Kept in the report.
    pub made_with: String,
    /// Points on our frames (one set for all, or one per frame), from `render.preview`'s `face_landmarks`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ours_points: Option<FramePoints>,
    /// Image files or assets the render was built from (textures, layers), checked for copies of the reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub built_from: Vec<ImageRef>,
    /// The report asset of the previous run, to report what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<ContentHash>,
    /// Score every `stride`th frame of ours (default 1: every frame).
    #[serde(default = "one_usize")]
    pub stride: usize,
    /// How to line the sequences up in time (default `offset`; `none` compares frame i with frame i).
    #[serde(default)]
    pub timing: TimeAlign,
    /// Tolerance and tiles.
    #[serde(flatten)]
    pub score: ScoreArgs,
}

fn one_usize() -> usize {
    1
}

/// One scored frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameRow {
    /// Our frame.
    pub ours: usize,
    /// The reference frame it was compared with.
    pub reference: usize,
    /// Pixels off.
    pub pixels_off: usize,
    /// The share within CIEDE2000 2.3.
    pub within_jnd: f64,
    /// The mean CIEDE2000.
    pub delta_e_mean: f64,
    /// The mean SSIM.
    pub ssim: f64,
}

impl Loop {
    fn target(&self, ctx: &OpCtx<'_>) -> Result<(String, TargetDef), OpError> {
        match &self.target {
            TargetArg::Inline(t) => Ok(("inline".to_owned(), t.clone())),
            TargetArg::Named { name } => {
                let v = ctx.doc.get(&target_path(name)).ok_or_else(|| {
                    OpError::not_found(format!("no parity target '{name}'")).hint("set it with parity.target.set")
                })?;
                let t = serde_json::from_value(v.clone())
                    .map_err(|e| OpError::internal(format!("parity/targets/{name} doesn't read: {e}")))?;
                Ok((name.clone(), t))
            }
        }
    }
}

fn metric(s: &PairScore, name: &str) -> f64 {
    s.metrics.iter().find(|m| m.name == name).map_or(0.0, |m| m.value)
}

impl Op for Loop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "parity.loop",
            summary: "One iteration of the parity loop, the call to make after every change: refuses renders built \
                      from reference pixels, lines our frames up with the target's in time, scores every frame pixel \
                      for pixel, and returns done (true only when the exact difference map is 0 on every frame), the \
                      totals, the worst frames, the worst regions with what to change, the worst frame's tiles and \
                      camera correction, an evidence sheet, what changed since `previous`, and a stored report to \
                      pass as `previous` next time",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    #[allow(clippy::too_many_lines, clippy::cast_precision_loss, clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.made_with.trim().is_empty() {
            return Err(OpError::invalid("made_with", "say how the render was made").hint(RULE));
        }
        let (name, target) = self.target(ctx)?;
        let ref_refs = target.reference.refs("target.reference")?;
        let our_refs = self.ours.refs("ours")?;
        // The check first, against every reference frame.
        let ref_frames = ref_refs
            .iter()
            .enumerate()
            .map(|(k, r)| {
                load(ctx, r, &format!("target.reference[{k}]")).map(|img| (format!("reference frame {k}"), img))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let check = check_provenance(ctx, &ref_frames, &self.built_from)?;
        if !check.clean {
            return Err(refused(&check));
        }
        // Time: which reference frame each of ours is compared with.
        let (map, timing_out): (Vec<usize>, Value) = if our_refs.len() == 1 && ref_refs.len() == 1 {
            (vec![0], Value::Null)
        } else {
            let a = descriptors(ctx, &our_refs, "ours")?;
            let b: Vec<Vec<f32>> =
                ref_frames.iter().map(|(_, img)| descriptor(&LabImage::from_rgba(img, [0, 0, 0]))).collect();
            let cost = |i: usize, j: usize| descriptor_cost(&a[i], &b[j]);
            let t = timing(&cost, a.len(), b.len(), 240);
            let map: Vec<usize> = match self.timing {
                TimeAlign::None => (0..a.len()).map(|i| i.min(b.len() - 1)).collect(),
                TimeAlign::Offset => (0..a.len())
                    .map(|i| usize::try_from((i as i64 + t.offset_frames).clamp(0, b.len() as i64 - 1)).unwrap_or(0))
                    .collect(),
                TimeAlign::Warp => t.map.clone(),
            };
            let fps = target.fps.unwrap_or(30.0);
            let mut out = serde_json::to_value(&t).map_err(|e| OpError::internal(e.to_string()))?;
            if let Value::Object(o) = &mut out {
                o.remove("map");
                o.insert("offset_ms".into(), json!(round(t.offset_frames as f64 * 1000.0 / fps)));
                o.insert("used".into(), json!(self.timing));
            }
            (map, out)
        };
        let setup = Setup {
            ours_points: None,
            reference_points: None,
            regions: target.regions.clone(),
            ignore: target.ignore.clone(),
            backdrop: target.backdrop.clone(),
            silhouette_key: None,
        };
        let options = self.score.options();
        let mut rows = Vec::new();
        let mut worst_frame: Option<(usize, usize, PairScore)> = None;
        let mut region_error: std::collections::BTreeMap<String, (f64, usize, String)> =
            std::collections::BTreeMap::new();
        let mut totals = (0usize, 0usize, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let stride = self.stride.max(1);
        for i in (0..our_refs.len()).step_by(stride) {
            let j = map[i];
            let ours = load(ctx, &our_refs[i], &format!("ours[{i}]"))?;
            let reference = &ref_frames[j].1;
            let op = self.ours_points.as_ref().and_then(|p| p.get(i));
            let rp = target.points.as_ref().and_then(|p| p.get(j));
            let p = prepare(ctx, &ours, reference, &setup, op, rp)?;
            let s = scored(&p, &options, None, op, rp);
            totals.0 += s.pixels_off;
            totals.1 += s.pixels;
            totals.2 += metric(&s, "within_jnd");
            totals.3 += metric(&s, "delta_e_mean");
            totals.4 += metric(&s, "ssim");
            totals.5 += metric(&s, "silhouette_iou");
            for r in s.regions.iter().filter(|r| r.name != "silhouette") {
                let e = region_error.entry(r.name.clone()).or_insert((0.0, 0, String::new()));
                e.0 += r.share_of_error;
                e.1 += r.pixels_off;
            }
            rows.push(FrameRow {
                ours: i,
                reference: j,
                pixels_off: s.pixels_off,
                within_jnd: metric(&s, "within_jnd"),
                delta_e_mean: metric(&s, "delta_e_mean"),
                ssim: metric(&s, "ssim"),
            });
            if worst_frame.as_ref().is_none_or(|w| {
                s.pixels_off > w.2.pixels_off
                    || (s.pixels_off == w.2.pixels_off && metric(&s, "delta_e_mean") > metric(&w.2, "delta_e_mean"))
            }) {
                worst_frame = Some((i, j, s));
            }
        }
        let n = rows.len().max(1) as f64;
        let Some((wi, wj, _)) = worst_frame else {
            return Err(OpError::invalid("ours", "no frames were scored"));
        };
        // The worst frame again, with the camera diagnosis, its sheet, and the hints of its regions.
        let wop = self.ours_points.as_ref().and_then(|p| p.get(wi));
        let wrp = target.points.as_ref().and_then(|p| p.get(wj));
        let wours = load(ctx, &our_refs[wi], "ours")?;
        let wp = prepare(ctx, &wours, &ref_frames[wj].1, &setup, wop, wrp)?;
        let worst = scored(&wp, &options, Some(AlignMethod::Auto), wop, wrp);
        for r in &worst.regions {
            if let Some(e) = region_error.get_mut(&r.name) {
                e.2.clone_from(&r.hint);
            }
        }
        let mut worst_regions: Vec<Worst> = region_error
            .iter()
            .filter(|(_, e)| e.1 > 0)
            .map(|(name, e)| Worst {
                scope: name.clone(),
                metric: "pixels_off".to_owned(),
                value: e.1 as f64,
                target: 0.0,
                share_of_error: round(e.0 / n),
                hint: e.2.clone(),
            })
            .collect();
        worst_regions.sort_by(|a, b| b.share_of_error.total_cmp(&a.share_of_error));
        let mut worst_frames = rows.clone();
        worst_frames.sort_by(|a, b| b.pixels_off.cmp(&a.pixels_off).then(b.delta_e_mean.total_cmp(&a.delta_e_mean)));
        worst_frames.truncate(5);
        let frames_done = rows.iter().filter(|r| r.pixels_off == 0).count();
        let done = totals.0 == 0 && check.clean;
        let metrics = vec![
            Metric::new("pixels_off", totals.0 as f64, "px", 0.0, true, "all frames"),
            Metric::new("off_share", totals.0 as f64 / totals.1.max(1) as f64, "share", 0.0, true, "all frames"),
            Metric::new("frames_at_zero", frames_done as f64 / n, "share", 1.0, false, "all frames"),
            Metric::new("within_jnd", totals.2 / n, "share", 1.0, false, "mean of frames"),
            Metric::new("delta_e_mean", totals.3 / n, "CIEDE2000", 0.0, true, "mean of frames"),
            Metric::new("ssim", totals.4 / n, "SSIM", 1.0, false, "mean of frames"),
            Metric::new("silhouette_iou", totals.5 / n, "IoU", 1.0, false, "mean of frames"),
        ];
        // What to do next, in order: framing first (it moves every pixel), then the worst regions.
        let mut next = Vec::new();
        if let Some(a) = &worst.alignment
            && a.framing_explains >= 0.1
            && !a.alignment.transform.is_identity()
        {
            next.push(format!(
                "camera: {} (removes {:.0}% of the colour error on frame {wi})",
                a.alignment.camera.summary,
                a.framing_explains * 100.0
            ));
        }
        if let Some(t) = timing_out.get("offset_frames").and_then(Value::as_i64).filter(|o| *o != 0) {
            next.push(format!(
                "timing: ours starts {t} frames {} the reference",
                if t > 0 { "into" } else { "before" }
            ));
        }
        next.extend(worst_regions.iter().take(5).map(|w| w.hint.clone()));
        // Evidence: the worst frame's sheet.
        let mut evidence = Vec::new();
        let mut report_asset = None;
        if !ctx.dry_run {
            let sheet = draw(&Mode::Sheet, &wp.ours, &wp.reference, wp.silhouettes(), &wp.regions);
            let asset = store_evidence(ctx, &sheet, vec![ref_frames[wj].1.hash, wours.hash], "parity sheet")?;
            evidence.push(json!({ "kind": "image", "asset": asset.to_string(), "what": format!("sheet of our frame {wi} against reference frame {wj}: ours, reference, onion; exact map, delta E, edges") }));
        }
        let progress = match &self.previous {
            Some(hash) => Some(progress(ctx, hash, totals.0, &region_error)?),
            None => None,
        };
        let mut out = json!({
            "track": "image",
            "done": done,
            "target": name,
            "made_with": self.made_with,
            "frames_scored": rows.len(),
            "provenance": check,
            "timing": timing_out,
            "metrics": metrics,
            "worst": worst_regions,
            "worst_frames": worst_frames,
            "worst_frame": { "ours": wi, "reference": wj, "score": worst },
            "next": next,
            "evidence": evidence,
            "progress": progress,
            "frames": rows,
        });
        if !ctx.dry_run {
            let bytes = serde_json::to_vec(&out).map_err(|e| OpError::internal(e.to_string()))?;
            let inputs = ref_frames.iter().map(|(_, r)| r.hash).collect();
            report_asset = Some(store_bytes(ctx, &bytes, inputs, "parity report")?.to_string());
        }
        out["report"] = json!(report_asset);
        Ok(Plan::answer(out))
    }
}

register_op!(Loop);

/// What changed since the previous report.
fn progress(
    ctx: &OpCtx<'_>,
    hash: &ContentHash,
    pixels_off: usize,
    regions: &std::collections::BTreeMap<String, (f64, usize, String)>,
) -> Result<Value, OpError> {
    let store = ctx.service::<dyn Store>()?;
    let bytes = store.get(hash).map_err(|e| match e {
        StoreError::Missing(_) => OpError::not_found(format!("no report {hash}")),
        other => OpError::internal(other.to_string()),
    })?;
    let before: Value = serde_json::from_slice(&bytes)
        .map_err(|e| OpError::invalid("previous", format!("{hash} is not a parity report: {e}")))?;
    let off_before = before["metrics"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["name"] == "pixels_off"))
        .and_then(|m| m["value"].as_f64())
        .unwrap_or(f64::NAN);
    let mut changed = Vec::new();
    if let Some(list) = before["worst"].as_array() {
        for w in list {
            let (Some(scope), Some(v)) = (w["scope"].as_str(), w["value"].as_f64()) else { continue };
            #[allow(clippy::cast_precision_loss)] // pixel counts
            let now = regions.get(scope).map_or(0.0, |r| r.1 as f64);
            changed.push(json!({ "region": scope, "pixels_off_before": v, "pixels_off_now": now, "better": now < v }));
        }
    }
    #[allow(clippy::cast_precision_loss)] // pixel counts
    let now = pixels_off as f64;
    Ok(json!({
        "previous": hash.to_string(),
        "pixels_off_before": off_before,
        "pixels_off_now": now,
        "better": now < off_before,
        "regions": changed,
    }))
}
