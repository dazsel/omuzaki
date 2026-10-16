//! The `render.*` checks: a render op called on the fixture and on the result, and the two pictures compared.
//!
//! **The convention.** A render op puts its PNG in the asset store and names it in its answer at `/image`, as
//! `omuzaki://render/<hash>.png` (`omuzaki_mcp::RenderProvider` serves the same address). The checker fetches both
//! pictures from the one store it was given ([`Checker::with_store`]): the fixture's and the result's renders are
//! content-addressed, so one store holds both.
//!
//! Pixels are compared as the goldens are ([`compare`]: premultiplied, a pixel is *changed* when a channel differs by
//! more than [`CHANNEL_TOLERANCE`]). A region is read from the **result** project: that is where the agent's change
//! lives, and a region op on the fixture would describe the old shape.

use image::{ImageFormat, RgbaImage};
use omuzaki_store_api::{ContentHash, Store};
use serde_json::{Value, json};

use super::Checker;
use super::query::allowed;
use super::report::Verdict;
use super::value::{call, pointer_all, result_failure};
use crate::compare::{Tolerance, compare};
use crate::eval::task::{OpCall, Region, RegionOp, RenderDiffExpect};
use crate::regioncmp::{Rect, compare_region};

/// Where a render op's answer names its picture.
const IMAGE: &str = "/image";
/// The address scheme of a render in the store.
const RENDER_PREFIX: &str = "omuzaki://render/";
const PNG_SUFFIX: &str = ".png";
/// Per-channel tolerance of the render checks (design 5.3): rounding noise is not a change.
pub const CHANNEL_TOLERANCE: u8 = 2;
const TOLERANCE: Tolerance = Tolerance { per_channel: CHANNEL_TOLERANCE, max_bad_fraction: 0.0 };

/// One render, with the hash it is stored under.
struct Picture {
    hash: String,
    image: RgbaImage,
}

impl Checker {
    /// The same checker, reading renders from `store` (the asset store both projects' render ops write to).
    #[must_use]
    pub fn with_store(mut self, store: std::sync::Arc<dyn Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// `render.same`: the result's render equals the fixture's within the tolerance.
    pub(super) fn render_same(&self, render: &OpCall) -> Verdict {
        let (before, after) = match self.pair(render) {
            Ok(pair) => pair,
            Err(verdict) => return verdict,
        };
        let renders = vec![before.hash.clone(), after.hash.clone()];
        let report = match compare(&after.image, &before.image, TOLERANCE) {
            Ok(report) => report,
            Err(e) => return Verdict::fail(format!("the renders cannot be compared: {e}")).with_renders(renders),
        };
        Verdict::when(
            report.stats.bad_pixels == 0,
            format!("{} (tolerance {CHANNEL_TOLERANCE} per channel)", report.stats),
        )
        .with_renders(renders)
    }

    /// `render.diff`: the renders differ, and where and by how much `expect` says.
    pub(super) fn render_diff(&self, render: &OpCall, region: &Region, expect: &RenderDiffExpect) -> Verdict {
        let (before, after) = match self.pair(render) {
            Ok(pair) => pair,
            Err(verdict) => return verdict,
        };
        let renders = vec![before.hash.clone(), after.hash.clone()];
        let fail = |detail: String| Verdict::fail(detail).with_renders(renders.clone());
        let size = after.image.dimensions();
        let whole = match compare(&after.image, &before.image, TOLERANCE) {
            Ok(report) => report.stats,
            Err(e) => return fail(format!("the renders cannot be compared: {e}")),
        };
        let total = u64::from(size.0) * u64::from(size.1);
        let fraction = ratio(whole.bad_pixels, total);
        let mut inside = None;
        if expect.changed_inside_at_least.is_some() {
            let rect = match self.rect(region, size) {
                Ok(rect) => rect,
                Err(verdict) => return verdict.with_renders(renders),
            };
            match compare_region(&after.image, &before.image, rect, TOLERANCE) {
                Ok(report) => inside = Some((rect, report.stats.bad_pixels)),
                Err(e) => {
                    return Verdict::error(format!("the region {rect} cannot be compared: {e}")).with_renders(renders);
                }
            }
        }
        let share = inside.map(|(_, bad)| ratio(bad, whole.bad_pixels));
        let seen = json!({
            "changed_fraction": round6(fraction),
            "changed_inside": share.map(round6),
        });
        let verdict = if let Some(least) = expect.changed_fraction_at_least.filter(|least| fraction < *least) {
            fail(format!("{fraction:.4} of the pixels changed, at least {least} expected"))
        } else if let Some(most) = expect.changed_fraction_at_most.filter(|most| fraction > *most) {
            fail(format!("{fraction:.4} of the pixels changed, at most {most} expected"))
        } else if let (Some(least), Some((rect, bad))) = (expect.changed_inside_at_least, inside) {
            if whole.bad_pixels == 0 {
                fail("nothing changed, so none of it is inside the region".to_owned())
            } else if ratio(bad, whole.bad_pixels) < least {
                fail(format!(
                    "{:.4} of the changed pixels lie inside {rect}, at least {least} expected",
                    ratio(bad, whole.bad_pixels)
                ))
            } else {
                Verdict::pass(format!(
                    "{:.4} of the pixels changed, {:.4} of them inside {rect}",
                    fraction,
                    ratio(bad, whole.bad_pixels)
                ))
                .with_renders(renders.clone())
            }
        } else {
            Verdict::pass(format!("{fraction:.4} of the pixels changed")).with_renders(renders.clone())
        };
        verdict.seen(None, Some(seen))
    }

    /// The render op's picture on the fixture and on the result.
    fn pair(&self, render: &OpCall) -> Result<(Picture, Picture), Verdict> {
        allowed(&*self.result, &render.op)?;
        let store = self.store.as_deref().ok_or_else(|| {
            Verdict::error("a render check needs the asset store the render ops write to, and the checker has none")
        })?;
        let before = self.before(&render.op, &render.args)?;
        let before = picture(store, &before, "the fixture", Verdict::error)?;
        let after = call(&*self.result, &render.op, &render.args)
            .map_err(|e| result_failure(&format!("`{}`", render.op), &e))?;
        let after = picture(store, &after, "the result", Verdict::error)?;
        Ok((before, after))
    }

    /// The region as a pixel rectangle inside a picture of `size`.
    fn rect(&self, region: &Region, size: (u32, u32)) -> Result<Rect, Verdict> {
        let numbers = match region {
            Region::Rect(numbers) => numbers.clone(),
            Region::FromOp(from) => self.region_from_op(from)?,
        };
        outward(&numbers, size)
    }

    fn region_from_op(&self, from: &RegionOp) -> Result<Vec<f64>, Verdict> {
        allowed(&*self.result, &from.op)?;
        let output = call(&*self.result, &from.op, &from.args)
            .map_err(|e| result_failure(&format!("region `{}`", from.op), &e))?;
        let found = pointer_all(&output, &from.at)?;
        let [value] = found.as_slice() else {
            return Err(Verdict::fail(format!(
                "`{}` reached {} values; a region is exactly one rect",
                from.at,
                found.len()
            )));
        };
        rect_numbers(value).ok_or_else(|| {
            Verdict::error(format!(
                "`{}` at `{}` is {value}, not a rect `[x, y, w, h]` or `{{x, y, w, h}}`",
                from.op, from.at
            ))
        })
    }
}

/// The picture a render op's answer names, fetched from the store. `on_trouble` makes the verdict of anything wrong
/// with it: an op that answers without an image, or a render the store lacks, is the harness's trouble, not the model's.
fn picture(
    store: &dyn Store,
    answer: &Value,
    whose: &str,
    on_trouble: fn(String) -> Verdict,
) -> Result<Picture, Verdict> {
    let found = pointer_all(answer, IMAGE)?;
    let [Value::String(uri)] = found.as_slice() else {
        return Err(on_trouble(format!("the render op's answer on {whose} has no `{IMAGE}` address")));
    };
    let hash = uri
        .strip_prefix(RENDER_PREFIX)
        .and_then(|rest| rest.strip_suffix(PNG_SUFFIX))
        .and_then(|id| id.parse::<ContentHash>().ok())
        .ok_or_else(|| {
            on_trouble(format!("`{uri}` on {whose} is not an `{RENDER_PREFIX}<hash>{PNG_SUFFIX}` address"))
        })?;
    let bytes = store.get(&hash).map_err(|e| on_trouble(format!("the render on {whose}: {e}")))?;
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .map_err(|e| on_trouble(format!("the render {hash} on {whose} is not a readable PNG: {e}")))?;
    Ok(Picture { hash: hash.to_string(), image: image.to_rgba8() })
}

/// `[x, y, w, h]` or `{x, y, w, h}` (also `width` and `height`), as numbers.
fn rect_numbers(value: &Value) -> Option<Vec<f64>> {
    match value {
        Value::Array(items) if items.len() == 4 => items.iter().map(Value::as_f64).collect(),
        Value::Object(map) => {
            let get = |names: &[&str]| names.iter().find_map(|n| map.get(*n)).and_then(Value::as_f64);
            Some(vec![get(&["x"])?, get(&["y"])?, get(&["w", "width"])?, get(&["h", "height"])?])
        }
        _ => None,
    }
}

/// The smallest pixel rectangle that contains `[x, y, w, h]`, cut to the picture.
fn outward(numbers: &[f64], (width, height): (u32, u32)) -> Result<Rect, Verdict> {
    let &[x, y, w, h] = numbers else {
        return Err(Verdict::error(format!("a region is four numbers `[x, y, w, h]`, found {}", numbers.len())));
    };
    if ![x, y, w, h].iter().all(|n| n.is_finite() && *n >= 0.0) {
        return Err(Verdict::error(format!("a region needs finite, non-negative numbers, found [{x}, {y}, {w}, {h}]")));
    }
    let edge = |n: f64, limit: u32| pixel(n).min(limit);
    let (left, top) = (edge(x.floor(), width), edge(y.floor(), height));
    let (right, bottom) = (edge((x + w).ceil(), width), edge((y + h).ceil(), height));
    if left >= right || top >= bottom {
        return Err(Verdict::error(format!(
            "the region [{x}, {y}, {w}, {h}] is empty or outside the {width}x{height} render"
        )));
    }
    Ok(Rect { x: left, y: top, width: right - left, height: bottom - top })
}

/// A non-negative whole float as pixels, saturating: the callers cut it to the picture right after.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked finite and non-negative; saturating on purpose
fn pixel(n: f64) -> u32 {
    n as u32
}

#[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^52
fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 }
}

/// Six decimals, so a report is the same bytes whatever the platform's float printing.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rects_round_outward_and_are_cut_to_the_picture() {
        let rect = outward(&[1.5, 2.0, 3.0, 2.2], (10, 10)).unwrap();
        assert_eq!(rect, Rect { x: 1, y: 2, width: 4, height: 3 });
        assert_eq!(outward(&[8.0, 8.0, 50.0, 50.0], (10, 10)).unwrap(), Rect { x: 8, y: 8, width: 2, height: 2 });
        assert!(outward(&[20.0, 0.0, 5.0, 5.0], (10, 10)).is_err());
        assert!(outward(&[0.0, 0.0, 0.0, 5.0], (10, 10)).is_err());
        assert!(outward(&[0.0, 0.0, -1.0, 5.0], (10, 10)).is_err());
        assert!(outward(&[0.0, 0.0, f64::NAN, 5.0], (10, 10)).is_err());
    }

    #[test]
    fn a_rect_is_an_array_or_an_object() {
        assert_eq!(rect_numbers(&json!([1, 2, 3, 4])), Some(vec![1.0, 2.0, 3.0, 4.0]));
        assert_eq!(rect_numbers(&json!({"x": 1, "y": 2, "width": 3, "h": 4})), Some(vec![1.0, 2.0, 3.0, 4.0]));
        assert_eq!(rect_numbers(&json!([1, 2, 3])), None);
        assert_eq!(rect_numbers(&json!("rect")), None);
    }
}
