//! Looking at a painting: `paint.render` makes a picture of it, `paint.sample` reads colours back off it.

use std::io::Write as _;

use omuzaki_ops_api::{ErrorCode, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_paint::{Canvas, Colour, Painting, layer_pixels, render};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::doc::{from_paint, load};

/// The smallest and largest `scale` of a render.
pub const SCALE_RANGE: (f64, f64) = (0.02, 8.0);
/// The largest side of a rendered picture, pixels.
pub const MAX_OUT_SIDE: u32 = 8192;
/// The most points one `paint.sample` reads.
pub const MAX_SAMPLES: usize = 4096;

/// Render a painting to a PNG you can look at: the whole picture or one layer alone, optionally cropped, scaled
/// (larger to inspect pixels, smaller for an overview), over another backdrop, with layers hidden. The PNG goes to the
/// asset store (`omuzaki://render/<hash>.png`) and, with `out`, to a file in the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "painting": "bust" },
    { "painting": "bust", "crop": [380, 540, 220, 120], "scale": 4 },
    { "painting": "bust", "layer": "lines", "background": "#ffffff", "out": "renders/lines.png" }
]))]
pub struct Render {
    /// The painting.
    pub painting: String,
    /// Render only this paint layer's own pixels (its marks and mask, not its blend or opacity), over clear unless
    /// `background` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Layers to hide in this render only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide: Vec<String>,
    /// A rectangle `[x, y, width, height]` in canvas pixels to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<[u32; 4]>,
    /// Scale the picture by this (0.02 to 8): averaged when smaller, crisp pixels when larger. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Draw over this colour instead of the painting's own backdrop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Colour>,
    /// Also write the PNG to this file in the project (it must end in `.png`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
}

impl Render {
    fn check(&self, p: &Painting) -> Result<(u32, u32), OpError> {
        if let Some(l) = &self.layer {
            p.layer(l).map_err(from_paint)?;
        }
        for (i, h) in self.hide.iter().enumerate() {
            p.layer(h).map_err(|_| OpError::invalid(format!("hide/{i}"), format!("there is no layer {h:?}")))?;
        }
        let (mut w, mut h) = (p.width, p.height);
        if let Some([x, y, cw, ch]) = self.crop {
            let fits = cw > 0
                && ch > 0
                && x.checked_add(cw).is_some_and(|r| r <= p.width)
                && y.checked_add(ch).is_some_and(|b| b <= p.height);
            if !fits {
                return Err(OpError::invalid(
                    "crop",
                    format!("[{x}, {y}, {cw}, {ch}] is not inside the {}x{} canvas", p.width, p.height),
                ));
            }
            (w, h) = (cw, ch);
        }
        let scale = self.scale.unwrap_or(1.0);
        if !(SCALE_RANGE.0..=SCALE_RANGE.1).contains(&scale) {
            return Err(OpError::invalid(
                "scale",
                format!("{scale} is outside {} to {}", SCALE_RANGE.0, SCALE_RANGE.1),
            ));
        }
        let (ow, oh) = (scaled(w, scale), scaled(h, scale));
        if ow > MAX_OUT_SIDE || oh > MAX_OUT_SIDE {
            return Err(OpError::invalid(
                "scale",
                format!("the picture would be {ow}x{oh}; at most {MAX_OUT_SIDE} a side"),
            ));
        }
        if let Some(out) = &self.out
            && !std::path::Path::new(out).extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            return Err(OpError::invalid("out", "the file name must end in .png"));
        }
        Ok((ow, oh))
    }

    fn picture(&self, p: &Painting) -> Result<Canvas, OpError> {
        let mut shown = p.clone();
        for h in &self.hide {
            if let Some(l) = shown.layers.get_mut(h) {
                l.visible = false;
            }
        }
        if let Some(bg) = self.background {
            shown.background = bg;
        }
        let full = match &self.layer {
            Some(l) => {
                let mut pixels = layer_pixels(&shown).map_err(from_paint)?;
                let own = pixels.remove(l).ok_or_else(|| OpError::invalid("layer", format!("{l:?} is a group")))?;
                // A layer alone shows its own alpha unless a backdrop is asked for.
                let under = self.background.unwrap_or(Colour::CLEAR);
                let mut c = Canvas::filled(p.width, p.height, under).map_err(from_paint)?;
                c.composite_from(&own, 1.0, omuzaki_paint::BlendMode::Normal, omuzaki_paint::blend::Keep::Over)
                    .map_err(from_paint)?;
                c
            }
            None => render(&shown).map_err(from_paint)?,
        };
        let cropped = match self.crop {
            Some(r) => crop(&full, r)?,
            None => full,
        };
        resample(&cropped, self.scale.unwrap_or(1.0))
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // checked ranges
fn scaled(side: u32, scale: f64) -> u32 {
    ((f64::from(side) * scale).round() as u32).max(1)
}

#[allow(clippy::cast_possible_wrap)] // sides are at most 8192
fn crop(c: &Canvas, [left, top, w, h]: [u32; 4]) -> Result<Canvas, OpError> {
    let mut out = Canvas::new(w, h).map_err(from_paint)?;
    for oy in 0..h {
        for ox in 0..w {
            let px = c.get((left + ox) as i32, (top + oy) as i32);
            if let Some(d) = out.at_mut(ox as i32, oy as i32) {
                *d = px;
            }
        }
    }
    Ok(out)
}

/// `c` scaled by `scale`: each output pixel averages the source pixels under it when shrinking, and takes the nearest
/// when growing (so a close-up shows the real pixels).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)] // canvas-sized values
fn resample(src: &Canvas, scale: f64) -> Result<Canvas, OpError> {
    if (scale - 1.0).abs() < 1e-12 {
        return Ok(src.clone());
    }
    let (w, h) = (scaled(src.width(), scale), scaled(src.height(), scale));
    let mut out = Canvas::new(w, h).map_err(from_paint)?;
    let (kx, ky) = (f64::from(src.width()) / f64::from(w), f64::from(src.height()) / f64::from(h));
    for oy in 0..h {
        for ox in 0..w {
            let px = if scale >= 1.0 {
                src.get((f64::from(ox) * kx) as i32, (f64::from(oy) * ky) as i32)
            } else {
                let (x0, x1) = ((f64::from(ox) * kx) as i32, (f64::from(ox + 1) * kx).ceil() as i32);
                let (y0, y1) = ((f64::from(oy) * ky) as i32, (f64::from(oy + 1) * ky).ceil() as i32);
                let mut sum = [0.0f32; 4];
                let mut n = 0.0f32;
                for y in y0..y1.max(y0 + 1) {
                    for x in x0..x1.max(x0 + 1) {
                        let p = src.get(x, y);
                        for i in 0..4 {
                            sum[i] += p[i];
                        }
                        n += 1.0;
                    }
                }
                sum.map(|v| v / n)
            };
            if let Some(d) = out.at_mut(ox as i32, oy as i32) {
                *d = px;
            }
        }
    }
    Ok(out)
}

/// A picture is the user's own work, made in Omuzaki from their marks.
fn provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".to_owned(),
        attribution: None,
        commercial: Commercial::Unknown,
        consent: None,
        private_test: false,
    }
}

/// Stores `png` in the asset store and answers its `{hash, uri}`, or a note when the app has none.
pub(crate) fn store_png(job: &JobCtx<'_>, png: &[u8]) -> Result<Value, OpError> {
    let store = match job.service::<dyn Store>() {
        Ok(store) => store,
        Err(e) if e.code == ErrorCode::Unavailable => {
            return Ok(json!({ "note": "no asset store is attached, so the picture was not kept" }));
        }
        Err(e) => return Err(e),
    };
    let hash =
        store.put(png, provenance()).map_err(|e| OpError::internal(format!("storing the picture failed: {e}")))?;
    Ok(json!({ "hash": hash.to_string(), "uri": format!("omuzaki://render/{hash}.png") }))
}

/// Writes `bytes` to project file `path`: a temporary file renamed over the old one once it is all on the disk.
pub(crate) fn write_file(job: &JobCtx<'_>, path: &str, bytes: &[u8]) -> Result<(), OpError> {
    let target = job.fs_access(path, FsMode::Write)?;
    if let Some(folder) = target.parent() {
        folder.create_dir_all()?;
    }
    let mut replacement = target.replace()?;
    replacement
        .write_all(bytes)
        .map_err(|e| OpError::invalid("out", format!("couldn't write {path}: {e}")).hint("check the disk has room"))?;
    replacement.commit()?;
    Ok(())
}

impl Op for Render {
    fn meta() -> OpMeta {
        OpMeta {
            name: "paint.render",
            summary: "Render a painting (or one layer) to a PNG to look at, optionally cropped, scaled, over another \
                      backdrop or with layers hidden; kept in the asset store and optionally written to a project file.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document; the picture goes to the content-addressed store or a file.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let p = load(ctx.doc, &self.painting)?;
        p.check().map_err(from_paint)?;
        let (w, h) = self.check(&p)?;
        if let Some(out) = &self.out {
            ctx.fs_access(out, FsMode::Write)?;
        }
        Ok(Plan::answer(json!({ "painting": self.painting, "width": w, "height": h, "marks": p.mark_count() })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        job.checkpoint("reading the painting", 0.0)?;
        let p = job.read(|doc| load(doc, &self.painting))?;
        self.check(&p)?;
        job.checkpoint("painting", 0.05)?;
        let picture = self.picture(&p)?;
        job.checkpoint("encoding", 0.9)?;
        let png = picture.to_png().map_err(from_paint)?;
        let mut answer = store_png(job, &png)?;
        answer["painting"] = json!(self.painting);
        answer["width"] = json!(picture.width());
        answer["height"] = json!(picture.height());
        if let Some(out) = &self.out {
            write_file(job, out, &png)?;
            answer["path"] = json!(out);
        }
        Ok(answer)
    }
}
register_op!(Render);

/// Read colours back off a painting at points, as the finished picture shows them (or one layer alone): an eyedropper
/// on your own work, to check a mark landed the colour you meant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "painting": "bust", "points": [[520, 700], [450, 690]] }]))]
pub struct Sample {
    /// The painting.
    pub painting: String,
    /// The points, `[x, y]` in canvas pixels (at most 4,096).
    pub points: Vec<[u32; 2]>,
    /// Sample this paint layer alone instead of the finished picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
}

impl Sample {
    fn check(&self, p: &Painting) -> Result<(), OpError> {
        if self.points.is_empty() || self.points.len() > MAX_SAMPLES {
            return Err(OpError::invalid("points", format!("give 1 to {MAX_SAMPLES} points")));
        }
        for (i, [x, y]) in self.points.iter().enumerate() {
            if *x >= p.width || *y >= p.height {
                return Err(OpError::invalid(
                    format!("points/{i}"),
                    format!("[{x}, {y}] is off the {}x{} canvas", p.width, p.height),
                ));
            }
        }
        if let Some(l) = &self.layer {
            p.layer(l).map_err(from_paint)?;
        }
        Ok(())
    }
}

impl Op for Sample {
    fn meta() -> OpMeta {
        OpMeta {
            name: "paint.sample",
            summary: "Read the colours of a painting (or one layer) at points, as #rrggbbaa.",
            kind: OpKind::Job,
            scope: Scope::Read,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let p = load(ctx.doc, &self.painting)?;
        p.check().map_err(from_paint)?;
        self.check(&p)?;
        Ok(Plan::answer(json!({ "painting": self.painting, "points": self.points.len() })))
    }

    #[allow(clippy::cast_possible_wrap)] // points are inside an 8192 canvas
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let p = job.read(|doc| load(doc, &self.painting))?;
        self.check(&p)?;
        let picture = Render {
            painting: self.painting.clone(),
            layer: self.layer.clone(),
            hide: Vec::new(),
            crop: None,
            scale: None,
            background: None,
            out: None,
        }
        .picture(&p)?;
        let colours: Vec<String> = self
            .points
            .iter()
            .map(|[x, y]| {
                let c = Colour::from_premul(picture.get(*x as i32, *y as i32));
                format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
            })
            .collect();
        Ok(json!({ "painting": self.painting, "colours": colours }))
    }
}
register_op!(Sample);
