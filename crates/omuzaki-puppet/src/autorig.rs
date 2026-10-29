//! The auto-rig: layered art in, a rigged puppet out, with the parameters a `VTuber` model is expected to have and a
//! channel mapping that drives them from the tracker, the performer and the anime extras.
//!
//! What it builds:
//! - **Meshes**: each layer is cropped to its paint and covered by a grid mesh, with cells nothing is painted in
//!   dropped, so the renderer fills only what shows.
//! - **The tree**: the body hangs from a pivot at its base, the head from a pivot where the neck meets the face.
//!   `ParamAngleZ` and `ParamBodyAngleZ` turn those pivots.
//! - **The head turn** (`ParamAngleX`, `ParamAngleY`): every head layer is moved as if painted on a rounded head
//!   turning in depth, by its role's depth: features in front slide further, hair behind slides the other way, the
//!   far side of the face compresses. This is the hand-rigged "fake 3D" of 2D `VTuber` models, computed per vertex.
//! - **Eyes**: `ParamEyeLOpen` and `ParamEyeROpen` close each eye onto its lower-lid line (a closed-eye layer, if the
//!   art has one, fades in at the end); `ParamEyeBallX` and `ParamEyeBallY` move the irises, clipped by the whites.
//! - **Brows, mouth**: brow raise and angle, `ParamMouthOpenY` (an open-mouth layer appears and grows; with none,
//!   the mouth stretches) and `ParamMouthForm` (the corners lift or drop).
//! - **Body**: `ParamBodyAngleX` (a turn, carrying the head), `ParamBreath`.
//! - **Hair**: `ParamHairFront`, `ParamHairSide` and `ParamHairBack` sway each hair layer from its roots, driven by
//!   pendulums hanging from the head, so hair follows every head motion.
//! - **Expressions**: a layer with `show_when` or `hide_when` gets an `Expr <channel>` parameter on its opacity.
//!
//! Model space is the canvas in pixels with the origin at its centre.
#![allow(
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel geometry: image sides are at most 16384, so the casts are exact, and formulas read best in their own letters"
)]

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec2;
use omuzaki_puppet_api::{
    BindTarget, ChannelSource, Interp, MapMode, MappingEntry, MaskMode, MaskRef, Mesh, NodeIx, ParamIx, ParamMapping,
    Pendulum, PendulumModel, PuppetDef, PuppetError, TexIx,
};

use crate::builder::PuppetBuilder;
use crate::layers::{Layer, LayeredArt, Role, Side};
use crate::texture::Texture;

/// Transparent pixels kept around each layer's paint, so bilinear filtering never reaches a hard crop edge.
pub const PAD: u32 = 3;
/// The most grid cells across a layer's longer side.
pub const MAX_CELLS: u32 = 24;
/// The smallest cell, pixels.
pub const MIN_CELL: f32 = 12.0;
/// The head turn's key angles, degrees.
pub const ANGLE_KEYS: [f32; 5] = [-30.0, -15.0, 0.0, 15.0, 30.0];

/// A rigged puppet, ready to save or perform.
#[derive(Clone, Debug)]
pub struct Rigged {
    /// The puppet.
    pub def: PuppetDef,
    /// Its channel mapping.
    pub mapping: ParamMapping,
    /// Each texture's straight RGBA8 pixels and size, in the order of `def.textures`.
    pub images: Vec<([u32; 2], Vec<u8>)>,
    /// What the rig decided, one line each (roles, sides, dropped layers).
    pub notes: Vec<String>,
}

impl Rigged {
    /// The textures, premultiplied with mips, for the renderer.
    #[must_use]
    pub fn textures(&self) -> Vec<Texture> {
        self.images.iter().filter_map(|(s, px)| Texture::from_straight(s[0], s[1], px)).collect()
    }
}

/// A layer cropped to its paint, in model space.
struct Cut {
    index: usize,
    role: Role,
    side: Side,
    depth: f32,
    /// The crop on the canvas: x, y, w, h (pixels).
    rect: [u32; 4],
    /// The crop's straight pixels.
    rgba: Vec<u8>,
}

impl Cut {
    fn lo(&self, half: Vec2) -> Vec2 {
        Vec2::new(self.rect[0] as f32, self.rect[1] as f32) - half
    }
    fn hi(&self, half: Vec2) -> Vec2 {
        Vec2::new((self.rect[0] + self.rect[2]) as f32, (self.rect[1] + self.rect[3]) as f32) - half
    }
    fn centre(&self, half: Vec2) -> Vec2 {
        (self.lo(half) + self.hi(half)) * 0.5
    }
}

/// The crop of `layer` on a canvas of `canvas`: its painted pixels' box, padded and clipped to the canvas.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, reason = "image sides are at most 16384")]
fn cut(layer: &Layer, canvas: [u32; 2]) -> Option<([u32; 4], Vec<u8>)> {
    let [w, h] = layer.size;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if layer.rgba[((y * w + x) * 4 + 3) as usize] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 >= x1 {
        return None;
    }
    let [ox, oy] = layer.def.offset;
    let to_canvas = |v: u32, o: i32, max: u32| (i64::from(v) + i64::from(o)).clamp(0, i64::from(max)) as u32;
    let cx0 = to_canvas(x0.saturating_sub(PAD), ox, canvas[0]);
    let cy0 = to_canvas(y0.saturating_sub(PAD), oy, canvas[1]);
    let cx1 = to_canvas(x1 + PAD, ox, canvas[0]);
    let cy1 = to_canvas(y1 + PAD, oy, canvas[1]);
    if cx0 >= cx1 || cy0 >= cy1 {
        return None;
    }
    let (cw, ch) = (cx1 - cx0, cy1 - cy0);
    let mut rgba = vec![0u8; (cw * ch * 4) as usize];
    for y in 0..ch {
        for x in 0..cw {
            // The source pixel under canvas pixel (cx0 + x, cy0 + y), if the layer covers it.
            let sx = i64::from(cx0 + x) - i64::from(ox);
            let sy = i64::from(cy0 + y) - i64::from(oy);
            if sx < 0 || sy < 0 || sx >= i64::from(w) || sy >= i64::from(h) {
                continue;
            }
            let s = ((sy as u32 * w + sx as u32) * 4) as usize;
            let d = ((y * cw + x) * 4) as usize;
            rgba[d..d + 4].copy_from_slice(&layer.rgba[s..s + 4]);
        }
    }
    Some(([cx0, cy0, cw, ch], rgba))
}

/// A grid mesh over a cut, in the coordinates of a node at `origin` (model space), with empty cells dropped.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel maths")]
fn cut_mesh(c: &Cut, half: Vec2, origin: Vec2) -> Mesh {
    let [_, _, w, h] = c.rect;
    let longest = w.max(h) as f32;
    let cell = (longest / MAX_CELLS as f32).max(MIN_CELL);
    let cols = ((w as f32 / cell).ceil() as u32).max(1);
    let rows = ((h as f32 / cell).ceil() as u32).max(1);
    // Which cells hold paint, grown by one cell so the mesh keeps a margin around it.
    let mut painted = vec![false; (cols * rows) as usize];
    for y in 0..h {
        for x in 0..w {
            if c.rgba[((y * w + x) * 4 + 3) as usize] > 0 {
                let cx = ((x as f32 / w as f32) * cols as f32).min(cols as f32 - 1.0) as u32;
                let cy = ((y as f32 / h as f32) * rows as f32).min(rows as f32 - 1.0) as u32;
                painted[(cy * cols + cx) as usize] = true;
            }
        }
    }
    let keep = |cx: i64, cy: i64| {
        (-1..=1).any(|dy| {
            (-1..=1).any(|dx| {
                let (x, y) = (cx + dx, cy + dy);
                x >= 0
                    && y >= 0
                    && x < i64::from(cols)
                    && y < i64::from(rows)
                    && painted[(y as u32 * cols + x as u32) as usize]
            })
        })
    };
    let lo = c.lo(half) - origin;
    let stride = cols + 1;
    let mut index = vec![u32::MAX; (stride * (rows + 1)) as usize];
    let mut mesh = Mesh::default();
    let mut vertex = |gx: u32, gy: u32, mesh: &mut Mesh| -> u32 {
        let slot = (gy * stride + gx) as usize;
        if index[slot] == u32::MAX {
            let (fx, fy) = (gx as f32 / cols as f32, gy as f32 / rows as f32);
            mesh.vertices.push([lo.x + fx * w as f32, lo.y + fy * h as f32]);
            mesh.uvs.push([fx, fy]);
            index[slot] = u32::try_from(mesh.vertices.len() - 1).unwrap_or(u32::MAX);
        }
        index[slot]
    };
    for cy in 0..rows {
        for cx in 0..cols {
            if !keep(i64::from(cx), i64::from(cy)) {
                continue;
            }
            let a = vertex(cx, cy, &mut mesh);
            let b = vertex(cx + 1, cy, &mut mesh);
            let cc = vertex(cx + 1, cy + 1, &mut mesh);
            let d = vertex(cx, cy + 1, &mut mesh);
            mesh.triangles.push([a, b, cc]);
            mesh.triangles.push([a, cc, d]);
        }
    }
    mesh
}

/// The rounded head the turn is computed on.
#[derive(Copy, Clone, Debug)]
struct HeadShape {
    centre: Vec2,
    radii: Vec2,
}

impl HeadShape {
    /// The offset of model point `p` on a layer of `depth` when the head turns by `yaw` and `pitch` (radians).
    fn turn(&self, p: Vec2, depth: f32, yaw: f32, pitch: f32) -> Vec2 {
        let q = p - self.centre;
        let u = q / self.radii;
        let surface = (1.0 - u.x * u.x - 0.6 * u.y * u.y).max(0.0).sqrt();
        let z = self.radii.x * (0.85 * surface + depth);
        let dx = q.x * (yaw.cos() - 1.0) + z * yaw.sin();
        let dy = q.y * (pitch.cos() - 1.0) - z * pitch.sin();
        Vec2::new(dx, dy)
    }
}

/// Union of boxes.
fn union(boxes: impl Iterator<Item = (Vec2, Vec2)>) -> Option<(Vec2, Vec2)> {
    boxes.fold(None, |acc, (lo, hi)| Some(acc.map_or((lo, hi), |(a, b): (Vec2, Vec2)| (a.min(lo), b.max(hi)))))
}

/// Why the auto-rig failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RigError {
    /// No layer has any paint.
    #[error("no layer has any paint")]
    Empty,
    /// The puppet it built failed a check (a bug in the rig).
    #[error(transparent)]
    Puppet(#[from] PuppetError),
}

fn entry(param: &str, axis: u8, channel: &str, minus: Option<&str>, input: [f32; 2], output: [f32; 2]) -> MappingEntry {
    MappingEntry {
        param: param.into(),
        axis,
        source: ChannelSource { channel: channel.into(), minus: minus.map(Into::into) },
        input,
        output,
    }
}

/// Rigs `art`.
///
/// # Errors
/// [`RigError::Empty`] when nothing is painted; [`RigError::Puppet`] if the result fails its own checks.
#[allow(clippy::too_many_lines, reason = "one pass over the standard rig, in order")]
#[allow(clippy::cast_precision_loss, reason = "canvas sides far below 2^24")]
pub fn autorig(art: &LayeredArt) -> Result<Rigged, RigError> {
    let canvas = art.manifest.canvas;
    let half = Vec2::new(canvas[0] as f32, canvas[1] as f32) * 0.5;
    let mut notes = Vec::new();
    let mut cuts = Vec::new();
    for (index, layer) in art.layers.iter().enumerate() {
        let Some((rect, rgba)) = cut(layer, canvas) else {
            notes.push(format!("{}: empty, left out", layer.def.name));
            continue;
        };
        cuts.push(Cut {
            index,
            role: layer.role,
            side: Side::Left,
            depth: layer.def.depth.unwrap_or_else(|| layer.role.depth()),
            rect,
            rgba,
        });
    }
    if cuts.is_empty() {
        return Err(RigError::Empty);
    }
    // The face: the face layer's box, or the head layers', or the top third of everything.
    let face_box = union(cuts.iter().filter(|c| c.role == Role::Face).map(|c| (c.lo(half), c.hi(half))))
        .or_else(|| union(cuts.iter().filter(|c| c.role.on_head()).map(|c| (c.lo(half), c.hi(half)))))
        .or_else(|| {
            union(cuts.iter().map(|c| (c.lo(half), c.hi(half))))
                .map(|(lo, hi)| (lo, Vec2::new(hi.x, lo.y + (hi.y - lo.y) / 3.0)))
        })
        .ok_or(RigError::Empty)?;
    let head =
        HeadShape { centre: (face_box.0 + face_box.1) * 0.5, radii: ((face_box.1 - face_box.0) * 0.5).max(Vec2::ONE) };
    let all = union(cuts.iter().map(|c| (c.lo(half), c.hi(half)))).ok_or(RigError::Empty)?;
    let body_box = union(cuts.iter().filter(|c| !c.role.on_head()).map(|c| (c.lo(half), c.hi(half)))).unwrap_or(all);
    // Sides: by name or manifest, else by which side of the face's centre the layer sits.
    for c in &mut cuts {
        let def = &art.layers[c.index].def;
        let lower = def.name.to_ascii_lowercase();
        c.side = def.side.unwrap_or_else(|| {
            if lower.ends_with(" l") || lower.contains("left") || lower.ends_with("_l") {
                Side::Left
            } else if lower.ends_with(" r") || lower.contains("right") || lower.ends_with("_r") {
                Side::Right
            } else if c.centre(half).x < head.centre.x {
                Side::Left
            } else {
                Side::Right
            }
        });
    }

    let mut b = PuppetBuilder::new(&art.manifest.name, [canvas[0] as f32, canvas[1] as f32]);
    b.meta_mut().artist.clone_from(&art.manifest.artist);
    b.meta_mut().source = "layers".into();
    let root = b.root();
    let body_pivot_at = Vec2::new(head.centre.x, body_box.1.y);
    let neck_at = Vec2::new(head.centre.x, face_box.1.y - head.radii.y * 0.15);
    let body = b.group("body pivot", root, body_pivot_at.to_array());
    let head_pivot = b.group("head pivot", body, (neck_at - body_pivot_at).to_array());

    let angle_x = b.param("ParamAngleX", -30.0, 30.0, 0.0, &ANGLE_KEYS);
    let angle_y = b.param("ParamAngleY", -30.0, 30.0, 0.0, &ANGLE_KEYS);
    let angle_z = b.param("ParamAngleZ", -30.0, 30.0, 0.0, &[-30.0, 30.0]);
    b.bind(angle_z, head_pivot, BindTarget::Rotate, Interp::Linear, vec![-30f32.to_radians(), 30f32.to_radians()]);
    let body_x = b.param("ParamBodyAngleX", -10.0, 10.0, 0.0, &[-10.0, 0.0, 10.0]);
    let body_z = b.param("ParamBodyAngleZ", -10.0, 10.0, 0.0, &[-10.0, 10.0]);
    b.bind(body_z, body, BindTarget::Rotate, Interp::Linear, vec![-10f32.to_radians(), 10f32.to_radians()]);
    let body_w = (body_box.1.x - body_box.0.x).max(1.0);
    b.bind(body_x, head_pivot, BindTarget::TranslateX, Interp::Linear, vec![-0.04 * body_w, 0.0, 0.04 * body_w]);
    let breath = b.param("ParamBreath", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    b.bind(
        breath,
        head_pivot,
        BindTarget::TranslateY,
        Interp::Linear,
        vec![0.0, -0.012 * (body_box.1.y - body_box.0.y)],
    );
    let mouth_open = b.param("ParamMouthOpenY", 0.0, 1.0, 0.0, &[0.0, 0.15, 1.0]);
    let mouth_form = b.param("ParamMouthForm", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let hair_front = b.param("ParamHairFront", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let hair_side = b.param("ParamHairSide", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let hair_back = b.param("ParamHairBack", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let mut per_side: BTreeMap<(Side, &str), ParamIx> = BTreeMap::new();
    let mut side_param = |b: &mut PuppetBuilder, side: Side, what: &'static str, min: f32, keys: &[f32]| -> ParamIx {
        *per_side.entry((side, what)).or_insert_with(|| {
            let name = match (what, side) {
                ("eye", Side::Left) => "ParamEyeROpen",
                ("eye", Side::Right) => "ParamEyeLOpen",
                ("brow", Side::Left) => "ParamBrowRY",
                ("brow", Side::Right) => "ParamBrowLY",
                ("angle", Side::Left) => "ParamBrowRAngle",
                _ => "ParamBrowLAngle",
            };
            let default = if what == "eye" { 1.0 } else { 0.0 };
            b.param(name, min, 1.0, default, keys)
        })
    };
    let eye_x = b.param("ParamEyeBallX", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let eye_y = b.param("ParamEyeBallY", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    let mut expressions: BTreeMap<String, ParamIx> = BTreeMap::new();

    // The eye regions per side: the whites' box, else the eye lines'.
    let eye_box = |side: Side| {
        union(cuts.iter().filter(|c| c.side == side && c.role == Role::EyeWhite).map(|c| (c.lo(half), c.hi(half))))
            .or_else(|| {
                union(
                    cuts.iter()
                        .filter(|c| c.side == side && matches!(c.role, Role::EyeLine | Role::Iris))
                        .map(|c| (c.lo(half), c.hi(half))),
                )
            })
    };
    let eye_boxes = [(Side::Left, eye_box(Side::Left)), (Side::Right, eye_box(Side::Right))];
    let mouth_box = union(cuts.iter().filter(|c| matches!(c.role, Role::Mouth)).map(|c| (c.lo(half), c.hi(half))))
        .or_else(|| union(cuts.iter().filter(|c| c.role == Role::MouthOpen).map(|c| (c.lo(half), c.hi(half)))));
    let has_open_mouth = cuts.iter().any(|c| c.role == Role::MouthOpen);
    let has_closed_eye = |side: Side| cuts.iter().any(|c| c.side == side && c.role == Role::EyeClosed);
    let mut whites: BTreeMap<Side, Vec<NodeIx>> = BTreeMap::new();
    let mut irises: Vec<(Side, NodeIx)> = Vec::new();
    let mut textures: Vec<([u32; 2], Vec<u8>)> = Vec::new();
    let count = cuts.len();
    let mut parts: Vec<(Role, NodeIx)> = Vec::with_capacity(count);
    let mut replacing: Vec<(ParamIx, Vec<String>)> = Vec::new();

    for (order, c) in cuts.iter().enumerate() {
        let layer = &art.layers[c.index];
        let on_head = c.role.on_head();
        let parent = if on_head { head_pivot } else { body };
        let origin = if on_head { neck_at } else { body_pivot_at };
        let tex: TexIx = b.texture(&format!("textures/{}.png", textures.len()), [c.rect[2], c.rect[3]]);
        textures.push(([c.rect[2], c.rect[3]], c.rgba.clone()));
        let mesh = cut_mesh(c, half, origin);
        let model: Vec<Vec2> = mesh.vertices.iter().map(|v| Vec2::from(*v) + origin).collect();
        let part = b.part(&layer.def.name, parent, tex, mesh);
        parts.push((c.role, part));
        if let Some(n) = b.node_mut(part) {
            // Back to front: the first layer has the largest zsort.
            #[allow(clippy::cast_precision_loss, reason = "at most 1024 layers")]
            let z = (count - order) as f32 * 0.01;
            n.zsort = z;
        }
        if let Some(d) = b.drawable_mut(part) {
            d.blend = layer.def.blend;
            d.opacity = layer.def.opacity;
        }
        let offsets = |f: &dyn Fn(Vec2) -> Vec2| -> Vec<[f32; 2]> { model.iter().map(|p| f(*p).to_array()).collect() };
        let zero = vec![[0.0, 0.0]; model.len()];
        if on_head {
            // The head turn, at each key angle.
            let turn = |yaw: f32, pitch: f32| offsets(&|p| head.turn(p, c.depth, yaw, pitch));
            b.bind_deform(angle_x, part, Interp::Cubic, ANGLE_KEYS.iter().map(|a| turn(a.to_radians(), 0.0)).collect());
            b.bind_deform(angle_y, part, Interp::Cubic, ANGLE_KEYS.iter().map(|a| turn(0.0, a.to_radians())).collect());
        } else {
            // The body turn: the same model on the body's own, flatter shape.
            let torso = HeadShape {
                centre: (body_box.0 + body_box.1) * 0.5,
                radii: ((body_box.1 - body_box.0) * 0.5).max(Vec2::ONE),
            };
            let turn = |yaw: f32| offsets(&|p| torso.turn(p, c.depth * 0.5, yaw, 0.0) * 0.5);
            b.bind_deform(
                body_x,
                part,
                Interp::Linear,
                vec![turn(-10f32.to_radians()), zero.clone(), turn(10f32.to_radians())],
            );
            let base = body_box.1.y;
            let height = (body_box.1.y - body_box.0.y).max(1.0);
            let lift = offsets(&|p| Vec2::new(0.0, -0.012 * height * ((base - p.y) / height).clamp(0.0, 1.0)));
            b.bind_deform(breath, part, Interp::Linear, vec![zero.clone(), lift]);
        }
        let (lo, hi) = (c.lo(half), c.hi(half));
        let size = hi - lo;
        match c.role {
            Role::EyeWhite | Role::Iris | Role::EyeLine | Role::EyeLower | Role::EyeClosed => {
                let eye = eye_boxes.iter().find(|(s, _)| *s == c.side).and_then(|(_, b)| *b).unwrap_or((lo, hi));
                let close_y = eye.0.y + (eye.1.y - eye.0.y) * 0.62;
                let open = side_param(&mut b, c.side, "eye", 0.0, &[0.0, 0.25, 1.0]);
                let shut = match c.role {
                    Role::EyeLine => {
                        let drop = (close_y - hi.y + size.y * 0.35).max(0.0);
                        offsets(&|p| Vec2::new(0.0, drop + (hi.y - p.y) * 0.25))
                    }
                    Role::EyeClosed => zero.clone(),
                    _ => offsets(&|p| Vec2::new(0.0, (close_y - p.y) * 0.95)),
                };
                let mid: Vec<[f32; 2]> = shut.iter().map(|o| [o[0] * 0.75, o[1] * 0.75]).collect();
                b.bind_deform(open, part, Interp::Linear, vec![shut, mid, zero.clone()]);
                if c.role == Role::EyeClosed {
                    b.bind(open, part, BindTarget::Opacity, Interp::Linear, vec![1.0, 0.0, 0.0]);
                } else if has_closed_eye(c.side) {
                    b.bind(open, part, BindTarget::Opacity, Interp::Linear, vec![0.0, 1.0, 1.0]);
                }
                match c.role {
                    Role::EyeWhite => whites.entry(c.side).or_default().push(part),
                    Role::Iris => {
                        irises.push((c.side, part));
                        let reach = eye.1 - eye.0;
                        b.bind(
                            eye_x,
                            part,
                            BindTarget::TranslateX,
                            Interp::Linear,
                            vec![-0.22 * reach.x, 0.0, 0.22 * reach.x],
                        );
                        b.bind(
                            eye_y,
                            part,
                            BindTarget::TranslateY,
                            Interp::Linear,
                            vec![0.15 * reach.y, 0.0, -0.15 * reach.y],
                        );
                    }
                    _ => {}
                }
            }
            Role::Brow => {
                let raise = side_param(&mut b, c.side, "brow", -1.0, &[-1.0, 0.0, 1.0]);
                let lift = 0.06 * head.radii.y;
                b.bind(raise, part, BindTarget::TranslateY, Interp::Linear, vec![lift, 0.0, -lift]);
                let angle = side_param(&mut b, c.side, "angle", -1.0, &[-1.0, 0.0, 1.0]);
                let centre = (lo + hi) * 0.5;
                // Angry (+1): the inner end (toward the face's centre) drops.
                let inward = if centre.x < head.centre.x { 1.0 } else { -1.0 };
                let tilt = |k: f32| {
                    offsets(&|p| {
                        Vec2::new(0.0, k * inward * (p.x - centre.x) / size.x.max(1.0) * size.y.max(4.0) * 1.2)
                    })
                };
                b.bind_deform(angle, part, Interp::Linear, vec![tilt(-1.0), zero.clone(), tilt(1.0)]);
            }
            Role::Mouth | Role::MouthOpen => {
                let (mlo, mhi) = mouth_box.unwrap_or((lo, hi));
                let mc = (mlo + mhi) * 0.5;
                let mw = (mhi.x - mlo.x).max(1.0) * 0.5;
                let form = |k: f32| {
                    offsets(&|p| {
                        let u = ((p.x - mc.x) / mw).clamp(-1.0, 1.0);
                        Vec2::new(0.0, -k * 0.35 * mw * 0.3 * (u * u - 0.35))
                    })
                };
                b.bind_deform(mouth_form, part, Interp::Linear, vec![form(-1.0), zero.clone(), form(1.0)]);
                if c.role == Role::MouthOpen {
                    let grow = |s: f32| offsets(&|p| Vec2::new(0.0, (p.y - lo.y) * (s - 1.0)));
                    b.bind_deform(mouth_open, part, Interp::Linear, vec![grow(0.2), grow(0.45), zero.clone()]);
                    b.bind(mouth_open, part, BindTarget::Opacity, Interp::Linear, vec![0.0, 1.0, 1.0]);
                } else if has_open_mouth {
                    b.bind(mouth_open, part, BindTarget::Opacity, Interp::Linear, vec![1.0, 0.0, 0.0]);
                } else {
                    let stretch = offsets(&|p| Vec2::new(0.0, (p.y - mc.y) * 1.4 + 0.5 * size.y));
                    let small: Vec<[f32; 2]> = stretch.iter().map(|o| [o[0] * 0.2, o[1] * 0.2]).collect();
                    b.bind_deform(mouth_open, part, Interp::Linear, vec![zero.clone(), small, stretch]);
                }
            }
            Role::HairFront | Role::HairSide | Role::HairBack => {
                let param = match c.role {
                    Role::HairFront => hair_front,
                    Role::HairSide => hair_side,
                    _ => hair_back,
                };
                let root_y = lo.y.max(head.centre.y - head.radii.y * 1.2);
                let length = (hi.y - root_y).max(1.0);
                let amp = 0.06 * length.max(head.radii.y);
                let sway = |k: f32| {
                    offsets(&|p| {
                        let t = ((p.y - root_y) / length).clamp(0.0, 1.0).powf(1.6);
                        Vec2::new(k * amp * t, -(k * amp * t).abs() * 0.15)
                    })
                };
                b.bind_deform(param, part, Interp::Cubic, vec![sway(-1.0), zero.clone(), sway(1.0)]);
            }
            _ => {}
        }
        for (channel, show) in [(&layer.def.show_when, true), (&layer.def.hide_when, false)] {
            let Some(channel) = channel else { continue };
            let param = *expressions
                .entry(channel.clone())
                .or_insert_with(|| b.param(&format!("Expr {channel}"), 0.0, 1.0, 0.0, &[0.0, 0.5, 1.0]));
            let values = if show { vec![0.0, 1.0, 1.0] } else { vec![1.0, 0.0, 0.0] };
            b.bind(param, part, BindTarget::Opacity, Interp::Linear, values);
            let replaced = layer.def.replaced();
            if show && !replaced.is_empty() {
                replacing.push((param, replaced));
            }
        }
    }
    // An expression that stands in for a feature hides that feature while it shows.
    for (param, groups) in &replacing {
        for (role, part) in &parts {
            let group = match role {
                Role::EyeWhite | Role::Iris | Role::EyeLine | Role::EyeLower | Role::EyeClosed => "eyes",
                Role::Mouth | Role::MouthOpen => "mouth",
                Role::Brow => "brows",
                _ => continue,
            };
            if groups.iter().any(|g| g == group) {
                b.bind(*param, *part, BindTarget::Opacity, Interp::Linear, vec![1.0, 0.0, 0.0]);
            }
        }
    }
    // Irises clip to their side's whites.
    for (side, iris) in &irises {
        let masks: Vec<MaskRef> = whites
            .get(side)
            .map(|w| w.iter().map(|&source| MaskRef { source, mode: MaskMode::Mask }).collect())
            .unwrap_or_default();
        if let Some(d) = b.drawable_mut(*iris) {
            d.masks = masks;
        }
    }
    // Hair pendulums hang from the head pivot.
    for (name, param, length) in
        [("hair front", hair_front, 0.9), ("hair side", hair_side, 1.3), ("hair back", hair_back, 1.7)]
    {
        b.pendulum(Pendulum {
            name: name.into(),
            anchor: head_pivot,
            param,
            model: PendulumModel::Rigid,
            map: MapMode::AngleLength,
            gravity: 1.0,
            length: head.radii.y * length,
            frequency: 1.0,
            angle_damping: 0.35,
            length_damping: 0.5,
            output_scale: [-6.0, 1.0],
            local_only: false,
        });
    }
    let def = b.finish()?;
    let mapping = default_mapping(&def);
    notes.extend(cuts.iter().map(|c| format!("{}: {:?}, {:?} side", art.layers[c.index].def.name, c.role, c.side)));
    Ok(Rigged { def, mapping, images: textures, notes })
}

/// The standard mapping for the parameters the auto-rig makes (and that most 2D `VTuber` models share): the channel bus
/// onto `Param*` names, plus `Expr <channel>` parameters onto their channels. Entries for parameters `def` lacks are
/// left out.
#[must_use]
pub fn default_mapping(def: &PuppetDef) -> ParamMapping {
    let deg30 = 30f32.to_radians();
    let deg10 = 10f32.to_radians();
    let mut entries = vec![
        entry("ParamAngleX", 0, "headYaw", None, [-deg30, deg30], [-30.0, 30.0]),
        entry("ParamAngleY", 0, "headPitch", None, [-deg30, deg30], [30.0, -30.0]),
        entry("ParamAngleZ", 0, "headRoll", None, [-deg30, deg30], [-30.0, 30.0]),
        entry("ParamBodyAngleX", 0, "bodyTurn", None, [-deg10, deg10], [-10.0, 10.0]),
        entry("ParamBodyAngleZ", 0, "lean", None, [-deg10, deg10], [-10.0, 10.0]),
        entry("ParamBreath", 0, "breath", None, [0.0, 1.0], [0.0, 1.0]),
        entry("ParamEyeLOpen", 0, "eyeBlinkLeft", None, [0.0, 1.0], [1.0, 0.0]),
        entry("ParamEyeROpen", 0, "eyeBlinkRight", None, [0.0, 1.0], [1.0, 0.0]),
        entry("ParamEyeBallX", 0, "eyeLookOutLeft", Some("eyeLookInLeft"), [-1.0, 1.0], [-1.0, 1.0]),
        entry("ParamEyeBallY", 0, "eyeLookUpLeft", Some("eyeLookDownLeft"), [-1.0, 1.0], [-1.0, 1.0]),
        entry("ParamBrowLY", 0, "browInnerUp", None, [0.0, 1.0], [0.0, 0.8]),
        entry("ParamBrowRY", 0, "browInnerUp", None, [0.0, 1.0], [0.0, 0.8]),
        entry("ParamBrowLY", 0, "browOuterUpLeft", None, [0.0, 1.0], [0.0, 0.4]),
        entry("ParamBrowRY", 0, "browOuterUpRight", None, [0.0, 1.0], [0.0, 0.4]),
        entry("ParamBrowLAngle", 0, "browDownLeft", None, [0.0, 1.0], [0.0, 1.0]),
        entry("ParamBrowRAngle", 0, "browDownRight", None, [0.0, 1.0], [0.0, 1.0]),
        entry("ParamMouthOpenY", 0, "jawOpen", None, [0.0, 0.6], [0.0, 1.0]),
        entry("ParamMouthForm", 0, "mouthSmileLeft", Some("mouthFrownLeft"), [-1.0, 1.0], [-1.0, 1.0]),
    ];
    for p in &def.params {
        if let Some(channel) = p.name.strip_prefix("Expr ") {
            entries.push(entry(&p.name, 0, channel, None, [0.0, 1.0], [0.0, 1.0]));
        }
    }
    entries.retain(|e| def.param(&e.param).is_some());
    ParamMapping { entries }
}
