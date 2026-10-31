//! `paint.export_layers`: a painting as layered art, the format the 2D puppet's auto-rig turns into a rigged puppet
//! (`omuzaki-puppet`'s `layers` module): one PNG per part, cropped to its paint, and a `*.layers.toml` manifest that
//! places each one on the canvas with its role, blend and opacity.

use omuzaki_ops_api::{FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_paint::{BlendMode, Canvas, LayerKind, Painting, Rect, render};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::render::write_file;
use crate::doc::{from_paint, id, load};

/// The roles the puppet's auto-rig knows (its `Role`, in snake case). A layer with another role is exported without
/// one, and the auto-rig reads the role from the layer's name.
pub const PUPPET_ROLES: [&str; 21] = [
    "body",
    "neck",
    "face",
    "ear",
    "hair_back",
    "hair_side",
    "hair_front",
    "brow",
    "eye_white",
    "iris",
    "eye_line",
    "eye_lower",
    "eye_closed",
    "mouth",
    "mouth_open",
    "nose",
    "face_detail",
    "expression",
    "head_accessory",
    "body_accessory",
    "static",
];

/// The puppet's name for a blend mode, where the puppet has it.
fn puppet_blend(mode: BlendMode) -> Option<&'static str> {
    Some(match mode {
        BlendMode::Normal => "normal",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color_dodge",
        BlendMode::Add => "linear_dodge",
        BlendMode::ColorBurn => "color_burn",
        BlendMode::HardLight => "hard_light",
        BlendMode::SoftLight => "soft_light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::LinearBurn | BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            return None;
        }
    })
}

/// One part of the export: a top-level layer with the layers clipped to it, or a top-level group, flattened.
struct Part {
    id: String,
    name: String,
    role: Option<String>,
    blend: &'static str,
    opacity: f64,
    /// The layers it is made of (the base, its clipped layers or the group's insides).
    members: Vec<String>,
}

/// The parts of `p`, back to front.
fn parts(p: &Painting) -> Result<Vec<Part>, OpError> {
    let mut out: Vec<Part> = Vec::new();
    for lid in p.children(None) {
        let l = p.layer(lid).map_err(from_paint)?;
        if !l.visible {
            continue;
        }
        if l.clip && l.kind == LayerKind::Paint {
            if let Some(last) = out.last_mut() {
                last.members.push(lid.to_owned());
            }
            continue;
        }
        let mode = l.blend.unwrap_or_default();
        let blend = puppet_blend(mode).ok_or_else(|| {
            OpError::invalid(format!("layers/{lid}/blend"), format!("the puppet has no {mode:?} blend"))
                .hint("set the layer to a blend the puppet has (normal, multiply, screen, overlay, add, ...) or merge it into a group")
        })?;
        let mut members = vec![lid.to_owned()];
        if l.kind == LayerKind::Group {
            members.extend(p.order.iter().filter(|x| inside(p, x, lid)).cloned());
        }
        out.push(Part {
            id: lid.to_owned(),
            name: l.name.clone(),
            role: l.role.clone().filter(|r| PUPPET_ROLES.contains(&r.as_str())),
            blend,
            opacity: l.opacity,
            members,
        });
    }
    Ok(out)
}

fn inside(p: &Painting, id: &str, group: &str) -> bool {
    let mut at = p.layers.get(id).and_then(|l| l.parent.as_deref());
    for _ in 0..=p.layers.len() {
        match at {
            Some(g) if g == group => return true,
            Some(g) => at = p.layers.get(g).and_then(|l| l.parent.as_deref()),
            None => return false,
        }
    }
    false
}

/// The part alone on a clear canvas, at full opacity and normal blend (the manifest carries those).
fn part_pixels(p: &Painting, part: &Part) -> Result<Canvas, OpError> {
    let mut alone = p.clone();
    alone.background = omuzaki_paint::Colour::CLEAR;
    for lid in p.children(None) {
        if let Some(l) = alone.layers.get_mut(lid)
            && !part.members.iter().any(|m| m == lid)
        {
            l.visible = false;
        }
    }
    if let Some(base) = alone.layers.get_mut(&part.id) {
        base.opacity = 1.0;
        base.blend = if base.kind == LayerKind::Group { Some(BlendMode::Normal) } else { None };
    }
    render(&alone).map_err(from_paint)
}

/// `c` cropped to `r`.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)] // canvas-sized values
fn cropped(c: &Canvas, r: Rect) -> Result<Canvas, OpError> {
    let (w, h) = (r.width() as u32, r.height() as u32);
    let mut out = Canvas::new(w, h).map_err(from_paint)?;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let px = c.get(r.x0 + x, r.y0 + y);
            if let Some(d) = out.at_mut(x, y) {
                *d = px;
            }
        }
    }
    Ok(out)
}

/// One `[[layer]]` of the manifest.
#[derive(Serialize)]
struct ManifestLayer {
    name: String,
    file: String,
    offset: [i32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    blend: &'static str,
    opacity: f64,
}

/// The manifest.
#[derive(Serialize)]
struct Manifest {
    name: String,
    canvas: [u32; 2],
    #[serde(rename = "layer")]
    layers: Vec<ManifestLayer>,
}

/// Export a painting as layered art for the 2D puppet: one PNG per part (each top-level layer with the layers
/// clipped to it, or a top-level group, flattened, cropped to its paint) and a `<name>.layers.toml` manifest with each
/// part's place, role, blend and opacity, in a project folder. Give layers puppet roles (`face`, `hair_front`,
/// `iris`, `eye_line`, `mouth`, ...) with `paint.set_layer` so the auto-rig moves them right.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "painting": "bust", "folder": "puppets/bust" }],
    "x-omuzaki-fixture" = "painting"
))]
pub struct ExportLayers {
    /// The painting.
    pub painting: String,
    /// The project folder to write into.
    pub folder: String,
    /// The manifest's (and the puppet's) name: letters, digits, `-`, `_` and `.`. Default: the painting's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ExportLayers {
    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.painting)
    }

    fn manifest_path(&self) -> String {
        format!("{}/{}.layers.toml", self.folder.trim_end_matches('/'), self.name())
    }
}

impl Op for ExportLayers {
    fn meta() -> OpMeta {
        OpMeta {
            name: "paint.export_layers",
            summary: "Export a painting as layered art for the 2D puppet: a PNG per part and a layers manifest with \
                      each part's place, role, blend and opacity.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document; the files are the result.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        id("name", self.name())?;
        let p = load(ctx.doc, &self.painting)?;
        p.check().map_err(from_paint)?;
        let parts = parts(&p)?;
        if parts.is_empty() {
            return Err(OpError::invalid("painting", "it has no visible layers to export"));
        }
        ctx.fs_access(&self.manifest_path(), FsMode::Write)?;
        let names: Vec<&str> = parts.iter().map(|x| x.id.as_str()).collect();
        Ok(Plan::answer(json!({ "painting": self.painting, "manifest": self.manifest_path(), "parts": names })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let p = job.read(|doc| load(doc, &self.painting))?;
        let parts = parts(&p)?;
        let folder = self.folder.trim_end_matches('/');
        let mut layers = Vec::new();
        let mut written = Vec::new();
        #[allow(clippy::cast_precision_loss)] // part counts are small
        let step = 1.0 / parts.len().max(1) as f32;
        for (i, part) in parts.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            job.checkpoint(&format!("painting {}", part.id), step * i as f32)?;
            let pixels = part_pixels(&p, part)?;
            let bounds = pixels.content_bounds();
            if bounds.is_empty() {
                continue;
            }
            let file = format!("{}.png", part.id);
            write_file(job, &format!("{folder}/{file}"), &cropped(&pixels, bounds)?.to_png().map_err(from_paint)?)?;
            written.push(format!("{folder}/{file}"));
            layers.push(ManifestLayer {
                name: part.name.clone(),
                file,
                offset: [bounds.x0, bounds.y0],
                role: part.role.clone(),
                blend: part.blend,
                opacity: part.opacity,
            });
        }
        let manifest = Manifest { name: self.name().to_owned(), canvas: [p.width, p.height], layers };
        let text =
            toml::to_string(&manifest).map_err(|e| OpError::internal(format!("writing the manifest failed: {e}")))?;
        write_file(job, &self.manifest_path(), text.as_bytes())?;
        Ok(json!({ "manifest": self.manifest_path(), "files": written, "parts": manifest.layers.len() }))
    }
}
register_op!(ExportLayers);
