//! `render.compare`: two stored renders side by side, as their difference, or as a heatmap of where they differ, with
//! the numbers an agent needs to tell whether an edit changed what it meant to (spec v3 §11). The usual loop is
//! `render.preview`, an edit, `render.preview` again, then `render.compare` of the two URIs.
//!
//! Either side may instead be a state of the project (#400, `docs/design/branches.md` 6.3): `"now"`, a transaction,
//! a snapshot, a point or another branch. Both states then render through `render.preview` with one `shot` (the same
//! character, camera, framing, pose, expression, light rig and size), so the only thing that differs between the two
//! pictures is the state. A character missing on one side renders as an empty frame, and `missing` says which.

use omuzaki_core::glam::UVec2;
use omuzaki_history_api::state::StateRef;
use omuzaki_ops_api::{CostHint, ErrorCode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{AlphaEncoding, Image};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::history::state_doc;
use super::picture::{canvas, load, paste, store, straight};
use super::preview::{Preview, PreviewResult};

/// The gap between the two pictures of a side-by-side, in pixels.
pub const GUTTER: u32 = 8;
/// How much the difference picture brightens a channel's difference, so a small change shows.
pub const DIFF_GAIN: u16 = 4;

fn default_threshold() -> u8 {
    8
}

/// How the comparison is drawn.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareMode {
    /// `a`, a gap, then `b`.
    SideBySide,
    /// Each channel's difference, brightened 4 times, on black: black where they agree.
    Diff,
    /// `a` dimmed to grey, with the pixels that differ by more than the threshold in yellow (a little) to red (a lot).
    #[default]
    Heatmap,
}

/// Compares two stored renders and stores the comparison as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"a": "omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png", "b": "omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png"},
        {"a": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "b": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "mode": "side_by_side"},
        {"a": "now", "b": "now", "shot": {"character": "kit", "framing": "face", "size": [256, 256], "quality": "live"}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct Compare {
    /// The first side: a stored render (its `omuzaki://render/<hash>.png` URI or its hash), or a state of the project
    /// (`"now"`, `{"tx": 7}`, `{"snapshot": …}`, `{"point": …}`, `{"branch": "aria/oval-face"}`) rendered with `shot`.
    pub a: Side,
    /// The second side, the same size as the first.
    pub b: Side,
    /// How a state is rendered: `render.preview`'s arguments (character, camera, framing, yaw, pitch, zoom, pose,
    /// expression, light rig, size, quality), the same for both sides. Needed when a side is a state; its own `state`
    /// is replaced by the side's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<Preview>,
    /// `heatmap` (the default), `diff` or `side_by_side`.
    #[serde(default)]
    pub mode: CompareMode,
    /// A pixel counts as changed when any channel (alpha included) differs by more than this, 0 to 255. The default, 8,
    /// ignores the rounding a GPU adds between runs.
    #[serde(default = "default_threshold")]
    pub threshold: u8,
}

/// One side of a comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Side {
    /// A state of the project, rendered with the comparison's `shot`.
    State(StateRef),
    /// A stored render: its `omuzaki://render/<hash>.png` URI or its hash.
    Render(String),
}

impl From<String> for Side {
    fn from(reference: String) -> Self {
        Self::Render(reference)
    }
}

impl From<&str> for Side {
    fn from(reference: &str) -> Self {
        Self::Render(reference.to_owned())
    }
}

/// What a comparison found, and where it is stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareResult {
    /// The pictures compared, `[a, b]`: each side's render URI (a state's is the picture it rendered to; a missing
    /// character's side is empty).
    #[serde(default)]
    pub sides: [String; 2],
    /// The sides (`a`, `b`) whose state has no such character: each rendered as an empty, transparent frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// `omuzaki://render/<hash>.png` of the comparison picture.
    pub uri: String,
    /// Its content hash.
    pub hash: String,
    /// Its width and height in pixels.
    pub size: [u32; 2],
    /// True when no pixel differs at all.
    pub identical: bool,
    /// How many pixels differ by more than the threshold.
    pub changed_pixels: u64,
    /// That count as a share of the picture, 0 to 1.
    pub changed: f32,
    /// The box around the changed pixels, `[x, y, width, height]`, or none when nothing changed.
    pub bounds: Option<[u32; 4]>,
    /// The largest difference in any channel, 0 to 255.
    pub max_difference: u8,
    /// The mean difference over every channel of every pixel, 0 to 255.
    pub mean_difference: f32,
}

/// What two same-sized pictures differ by, pixel by pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Difference {
    /// The largest channel difference of each pixel.
    pub per_pixel: Vec<u8>,
    /// Pixels over the threshold.
    pub changed: u64,
    /// `[x, y, width, height]` around them.
    pub bounds: Option<[u32; 4]>,
    /// The largest channel difference.
    pub max: u8,
    /// The mean channel difference.
    pub mean: f32,
}

impl Difference {
    /// The difference of `a` and `b`, which have the same size; changed means over `threshold`. Colours are compared
    /// premultiplied by their alpha, as they composite: a colour change under zero alpha is no change.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "a render's channel total is far below 2^52; the mean is at most 255")]
    pub fn of(a: &Image, b: &Image, threshold: u8) -> Self {
        let width = a.size.x as usize;
        let (mut changed, mut max, mut total) = (0_u64, 0_u8, 0_u64);
        let (mut lo, mut hi) = (UVec2::MAX, UVec2::ZERO);
        let mut per_pixel = Vec::with_capacity(a.rgba8.len() / 4);
        for (i, (in_a, in_b)) in a.rgba8.as_chunks::<4>().0.iter().zip(b.rgba8.as_chunks::<4>().0).enumerate() {
            let (from, to) = (premultiplied(*in_a, a.alpha), premultiplied(*in_b, b.alpha));
            let mut worst = 0;
            for (x, y) in from.iter().zip(&to) {
                let d = x.abs_diff(*y);
                worst = worst.max(d);
                total += u64::from(d);
            }
            per_pixel.push(worst);
            max = max.max(worst);
            if worst > threshold {
                changed += 1;
                let at = UVec2::new(u32::try_from(i % width).unwrap_or(u32::MAX), u32::try_from(i / width).unwrap_or(u32::MAX));
                (lo, hi) = (lo.min(at), hi.max(at));
            }
        }
        let bounds = (changed > 0).then(|| [lo.x, lo.y, hi.x - lo.x + 1, hi.y - lo.y + 1]);
        let mean = if a.rgba8.is_empty() { 0.0 } else { (total as f64 / a.rgba8.len() as f64) as f32 };
        Self { per_pixel, changed, bounds, max, mean }
    }
}

/// A pixel with its colour multiplied by its alpha, rounded; a premultiplied one as it is.
fn premultiplied(pixel: [u8; 4], alpha: AlphaEncoding) -> [u8; 4] {
    if alpha == AlphaEncoding::Premultiplied {
        return pixel;
    }
    let a = u16::from(pixel[3]);
    let times = |c: u8| u8::try_from((u16::from(c) * a + 127) / 255).unwrap_or(u8::MAX);
    [times(pixel[0]), times(pixel[1]), times(pixel[2]), pixel[3]]
}

/// The picture `mode` draws for `a` and `b`, which have the same size.
///
/// # Errors
/// What [`canvas`] reports for a side-by-side too large to hold.
pub fn draw(mode: CompareMode, a: &Image, b: &Image, difference: &Difference, threshold: u8) -> Result<Image, OpError> {
    match mode {
        CompareMode::SideBySide => {
            let mut out = canvas("b", UVec2::new(a.size.x + GUTTER + b.size.x, a.size.y.max(b.size.y)))?;
            paste(&mut out, a, UVec2::ZERO);
            paste(&mut out, b, UVec2::new(a.size.x + GUTTER, 0));
            Ok(out)
        }
        CompareMode::Diff => {
            let mut rgba = Vec::with_capacity(a.rgba8.len());
            for (p, q) in a.rgba8.as_chunks::<4>().0.iter().zip(b.rgba8.as_chunks::<4>().0) {
                for c in 0..3 {
                    let d = u16::from(p[c].abs_diff(q[c])).max(u16::from(p[3].abs_diff(q[3])));
                    rgba.push(u8::try_from((d * DIFF_GAIN).min(255)).unwrap_or(u8::MAX));
                }
                rgba.push(255);
            }
            straight(a.size, rgba)
        }
        CompareMode::Heatmap => {
            let mut rgba = Vec::with_capacity(a.rgba8.len());
            for (p, &d) in a.rgba8.as_chunks::<4>().0.iter().zip(&difference.per_pixel) {
                if d > threshold {
                    // Yellow at the threshold, red at the largest difference.
                    let t = u16::from(d - threshold) * 255 / u16::from(255 - threshold).max(1);
                    rgba.extend([255, u8::try_from(220 - t.min(255) * 220 / 255).unwrap_or(0), 0, 255]);
                } else {
                    // The picture as a dim grey over mid grey, so its shape shows and its colours don't compete.
                    let luma = (u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000;
                    let lit = (luma * u32::from(p[3]) + 128 * (255 - u32::from(p[3]))) / 255;
                    let grey = u8::try_from(lit * 35 / 100 + 40).unwrap_or(u8::MAX);
                    rgba.extend([grey, grey, grey, 255]);
                }
            }
            straight(a.size, rgba)
        }
    }
}

impl Op for Compare {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.compare",
            summary: "Compare two stored renders (their omuzaki://render/ URIs): a heatmap of where they differ, their \
                      difference, or side by side, stored as a PNG; returns how many pixels changed and the box around \
                      them.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (the PNG goes to the content store), so its empty patch is undone
            // exactly and nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut cost = CostHint { seconds: Some(0.2), gpu_mib: Some(0), usd: None };
        let mut missing = Vec::new();
        for (field, side) in [("a", &self.a), ("b", &self.b)] {
            match side {
                Side::Render(reference) => {
                    super::picture::hash_of(field, reference)?;
                }
                Side::State(state) => {
                    match self.shot_of(field, state)?.plan(ctx) {
                        Ok(plan) => {
                            let render = plan.cost.unwrap_or_default();
                            cost.seconds = Some(cost.seconds.unwrap_or(0.0) + render.seconds.unwrap_or(0.0));
                            cost.gpu_mib = cost.gpu_mib.max(render.gpu_mib);
                        }
                        Err(e) if e.code == ErrorCode::NotFound && state_doc(ctx.services(), Some(state)).is_ok() => {
                            missing.push(field);
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        if missing.len() == 2 {
            let character = self.shot.as_ref().map(|s| s.character.to_string()).unwrap_or_default();
            return Err(OpError::not_found(format!("neither state has a character '{character}'"))
                .hint("render a character that exists in at least one of them"));
        }
        Ok(Plan { cost: Some(cost), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let (a, a_uri) = self.side(job, "a", &self.a)?;
        let (b, b_uri) = self.side(job, "b", &self.b)?;
        let mut missing = Vec::new();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            (None, Some(b)) => {
                missing.push("a".to_owned());
                (canvas("a", b.size)?, b)
            }
            (Some(a), None) => {
                missing.push("b".to_owned());
                let empty = canvas("b", a.size)?;
                (a, empty)
            }
            (None, None) => {
                return Err(OpError::not_found("neither state has the character").hint("render one that exists"));
            }
        };
        if a.size != b.size {
            return Err(OpError::invalid(
                "b",
                format!("a is {}x{} and b is {}x{}; only pictures of one size compare", a.size.x, a.size.y, b.size.x, b.size.y),
            )
            .hint("render both with the same size, framing and camera"));
        }
        job.checkpoint("comparing", 0.3)?;
        let difference = Difference::of(&a, &b, self.threshold);
        let picture = draw(self.mode, &a, &b, &difference, self.threshold)?;
        let stored = store(job, &picture)?;
        job.checkpoint("done", 1.0)?;
        #[allow(clippy::cast_precision_loss, reason = "a share of the picture needs no more than f32 holds")]
        let changed = difference.changed as f32 / (a.size.x as f32 * a.size.y as f32).max(1.0);
        let result = CompareResult {
            sides: [a_uri, b_uri],
            missing,
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: [picture.size.x, picture.size.y],
            identical: difference.max == 0,
            changed_pixels: difference.changed,
            changed,
            bounds: difference.bounds,
            max_difference: difference.max,
            mean_difference: difference.mean,
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the comparison: {e}")))
    }
}

register_op!(Compare);

impl Compare {
    /// The preview that renders `state` for side `field`.
    fn shot_of(&self, field: &str, state: &StateRef) -> Result<Preview, OpError> {
        let shot = self.shot.as_ref().ok_or_else(|| {
            OpError::invalid("shot", format!("{field} is a state, so the comparison needs a shot to render it with"))
                .hint("give shot render.preview's arguments, at least the character: {\"character\": \"kit\"}")
        })?;
        Ok(Preview { state: Some(state.clone()), ..shot.clone() })
    }

    /// Side `field`'s picture and its URI: a stored render as it is, a state rendered with the shot, or `None` when the
    /// state has no such character.
    fn side(&self, job: &JobCtx<'_>, field: &str, side: &Side) -> Result<(Option<Image>, String), OpError> {
        match side {
            Side::Render(reference) => Ok((Some(load(job, field, reference)?), reference.clone())),
            Side::State(state) => {
                let preview = self.shot_of(field, state)?;
                let has = state_doc(job.services(), Some(state))?.map_or_else(
                    || job.read(|doc| doc.get(&omuzaki_char_api::paths::character(&preview.character)).is_some()),
                    |doc| omuzaki_doc_api::DocRead::get(&doc, &omuzaki_char_api::paths::character(&preview.character)).is_some(),
                );
                if !has {
                    return Ok((None, String::new()));
                }
                let rendered: PreviewResult = serde_json::from_value(preview.work(job)?)
                    .map_err(|e| OpError::internal(format!("reading the render back: {e}")))?;
                Ok((Some(load(job, field, &rendered.uri)?), rendered.uri))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(size: UVec2, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
        let rgba = (0..size.y).flat_map(|y| (0..size.x).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect();
        straight(size, rgba).unwrap()
    }

    #[test]
    fn a_picture_against_itself_is_identical_and_draws_no_change() {
        let a = picture(UVec2::new(6, 4), |x, y| [u8::try_from(x * 40).unwrap(), u8::try_from(y * 60).unwrap(), 7, 255]);
        let d = Difference::of(&a, &a, 8);
        assert_eq!((d.changed, d.bounds, d.max, d.mean), (0, None, 0, 0.0));
        let heat = draw(CompareMode::Heatmap, &a, &a, &d, 8).unwrap();
        assert!(heat.rgba8.chunks(4).all(|p| p[0] == p[1] && p[1] == p[2]), "all grey");
    }

    #[test]
    fn a_changed_square_is_boxed_counted_and_drawn_red() {
        let size = UVec2::new(10, 8);
        let a = picture(size, |_, _| [100, 100, 100, 255]);
        let b = picture(size, |x, y| if (3..6).contains(&x) && (2..4).contains(&y) { [250, 100, 100, 255] } else { [103, 100, 100, 255] });
        let d = Difference::of(&a, &b, 8);
        assert_eq!(d.changed, 6, "the 3x2 square; the 3-level noise is under the threshold");
        assert_eq!(d.bounds, Some([3, 2, 3, 2]));
        assert_eq!(d.max, 150);
        let heat = draw(CompareMode::Heatmap, &a, &b, &d, 8).unwrap();
        let at = |x: usize, y: usize| &heat.rgba8[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4];
        assert_eq!(at(4, 3)[0], 255);
        assert!(at(4, 3)[1] < 220, "towards red for a large change");
        assert_eq!(at(0, 0)[0], at(0, 0)[1], "grey outside");
        let diff = draw(CompareMode::Diff, &a, &b, &d, 8).unwrap();
        assert_eq!(&diff.rgba8[(3 * 10 + 4) * 4..(3 * 10 + 4) * 4 + 4], &[255, 0, 0, 255]);
        assert_eq!(&diff.rgba8[..4], &[12, 0, 0, 255], "small differences brightened four times");
    }

    #[test]
    fn a_change_counts_above_the_threshold_and_only_where_it_shows() {
        let size = UVec2::new(4, 4);
        let a = picture(size, |x, _| if x == 3 { [10, 200, 30, 0] } else { [100, 100, 100, 255] });
        // Row 0: exactly the threshold; row 1: one over it; row 2: one over it, but only in alpha; column 3: a new
        // colour under zero alpha, which shows nowhere.
        let b = picture(size, |x, y| match (x, y) {
            (3, _) => [250, 0, 250, 0],
            (0, 0) => [108, 100, 100, 255],
            (0, 1) => [109, 100, 100, 255],
            (0, 2) => [100, 100, 100, 246],
            _ => [100, 100, 100, 255],
        });
        let d = Difference::of(&a, &b, 8);
        assert_eq!(d.changed, 2, "{:?}", d.per_pixel);
        assert_eq!(d.bounds, Some([0, 1, 1, 2]));
        assert_eq!(d.per_pixel[3], 0, "a colour under zero alpha doesn't count");
    }

    #[test]
    fn side_by_side_puts_b_after_a_gap() {
        let size = UVec2::new(4, 3);
        let (a, b) = (picture(size, |_, _| [1, 1, 1, 255]), picture(size, |_, _| [2, 2, 2, 255]));
        let out = draw(CompareMode::SideBySide, &a, &b, &Difference::of(&a, &b, 0), 0).unwrap();
        assert_eq!(out.size, UVec2::new(4 + GUTTER + 4, 3));
        let first_of_b = ((4 + GUTTER) * 4) as usize;
        assert_eq!(&out.rgba8[first_of_b..first_of_b + 4], &[2, 2, 2, 255]);
        assert_eq!(&out.rgba8[16..20], &[0, 0, 0, 0], "the gap is transparent");
    }
}
