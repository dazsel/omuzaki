//! Things that dress a room: `gramophone` (a cabinet, a platter and a flared horn), `picture` (a framed picture to hang
//! on a wall), `urn` (a lidded vase with handles) and `chest` (a lidded chest with metal straps). Procedural, in metres, toon with vertex colours, like the other
//! interior kinds.

use std::f32::consts::{PI, TAU};

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, mix, painted, painted_plain};
use super::{between, output};

/// Brass, the default metal of the decor kinds.
pub(crate) const BRASS: Colour = Colour::hex(0x00c9_9a3e);
/// A dark lacquered wood.
const WALNUT: Colour = Colour::hex(0x004a_2e22);

/// `i` of `n` as a fraction, for the few dozen steps a kind loops over.
fn frac(i: u32, n: u32) -> f32 {
    // Counts of a few dozen: exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let t = i as f32 / n.max(1) as f32;
    t
}

/// A gramophone.
pub struct Gramophone;

/// The gramophone's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GramophoneParams {
    /// The cabinet's width and depth in metres, 0.2 to 0.6.
    pub size: f32,
    /// The horn's mouth radius in metres, 0.1 to 0.45.
    pub horn: f32,
    /// The horn's flutes (petals) around its mouth, 0 for a smooth bell, up to 16.
    pub petals: u8,
    /// The cabinet.
    pub cabinet: Colour,
    /// The horn and the fittings.
    pub metal: Colour,
}

impl Default for GramophoneParams {
    fn default() -> Self {
        Self { size: 0.34, horn: 0.26, petals: 8, cabinet: WALNUT, metal: BRASS }
    }
}

impl PropKind for Gramophone {
    type Params = GramophoneParams;
    const ID: &'static str = "gramophone";
    const SUMMARY: &'static str = "A gramophone standing on y = 0: a wooden cabinet with a platter, and a flared horn \
                                   rising from its back and opening toward +Z. Size, the horn's mouth and flutes, wood \
                                   and metal.";

    fn validate(p: &GramophoneParams) -> Result<(), String> {
        between("size", p.size, 0.2, 0.6)?;
        between("horn", p.horn, 0.1, 0.45)?;
        if p.petals > 16 {
            return Err(format!("petals is 0 to 16, got {}", p.petals));
        }
        Ok(())
    }

    fn build(p: &GramophoneParams) -> Result<PropOutput, String> {
        let s = p.size * 0.5;
        let box_h = p.size * 0.42;
        let mut cabinet = Shape::new();
        cabinet.bevelled(At::xyz(0.0, box_h * 0.5, 0.0), Vec3::new(s, box_h * 0.5, s), 0.006, p.cabinet);
        // A plinth and a lid moulding a little wider than the box.
        cabinet.block(Vec3::new(-s - 0.01, 0.0, -s - 0.01), Vec3::new(s + 0.01, 0.02, s + 0.01), darker(p.cabinet, 0.2));
        cabinet.block(Vec3::new(-s - 0.008, box_h - 0.012, -s - 0.008), Vec3::new(s + 0.008, box_h, s + 0.008), darker(p.cabinet, 0.15));
        // A crank on the +X side.
        let mut metal = Shape::new();
        metal.tube(&[Vec3::new(s, box_h * 0.55, 0.0), Vec3::new(s + 0.04, box_h * 0.55, 0.0), Vec3::new(s + 0.04, box_h * 0.3, 0.02)], 0.005, 8, p.metal);
        // The platter and its record.
        let platter_r = s * 0.82;
        metal.cylinder(At::xyz(0.0, box_h, 0.0), platter_r, 0.01, 40, p.metal);
        cabinet.cylinder(At::xyz(0.0, box_h + 0.01, 0.0), platter_r * 0.96, 0.004, 40, Colour::hex(0x0016_1616));
        // The tone arm: up from the back of the lid, over the record.
        let arm_base = Vec3::new(-s * 0.55, box_h, -s * 0.6);
        metal.cylinder(At::pos(arm_base), 0.02, 0.05, 16, p.metal);
        let neck_top = arm_base + Vec3::new(0.0, p.horn * 0.9 + 0.08, 0.0);
        // The neck rises from the arm's base and bends forward into the horn's throat.
        let throat = neck_top + Vec3::new(0.0, p.horn * 0.25, p.horn * 0.35);
        let neck: Vec<Vec3> = (0..=12)
            .map(|i| {
                let t = frac(i, 12);
                let up = arm_base + (neck_top - arm_base) * t.min(1.0);
                // A quarter bend over the last third.
                let bend = ((t - 0.66) / 0.34).clamp(0.0, 1.0);
                up + (throat - neck_top) * (bend * PI * 0.5).sin()
            })
            .collect();
        metal.tube(&neck, 0.018, 12, p.metal);
        metal.tube(&[arm_base + Vec3::new(0.0, 0.05, 0.0), Vec3::new(platter_r * 0.2, box_h + 0.05, platter_r * 0.1), Vec3::new(platter_r * 0.35, box_h + 0.02, platter_r * 0.25)], 0.006, 8, p.metal);

        // The horn: rings flaring out from the throat along +Z (tilted up a little), each a short open vessel, so the
        // inside reads from the front. The mouth's rim is scalloped into `petals` flutes.
        let mut horn = Shape::new();
        let inside = darker(p.metal, 0.35);
        let rings = 10;
        let length = p.horn * 1.5;
        let radius = |t: f32| 0.02 + (p.horn - 0.02) * t.powf(2.4);
        for i in 0..rings {
            let (t0, t1) = (frac(i, rings), frac(i + 1, rings));
            let at = At::pos(throat + Vec3::new(0.0, length * t0 * 0.3, length * t0)).pitch(73.0);
            let shade = mix(p.metal, inside, 0.25 * (1.0 - t0));
            horn.vessel(at, radius(t0), radius(t1), length * (t1 - t0) * 1.04, 0.004, false, 40, shade);
        }
        if p.petals > 0 {
            // A rib along each seam between two flutes, from halfway out to the mouth.
            let mouth = throat + Vec3::new(0.0, length * 0.3, length);
            let axis = (mouth - throat).normalize();
            let side = axis.cross(Vec3::Y).normalize();
            let up = side.cross(axis).normalize();
            for k in 0..u32::from(p.petals) {
                let a = frac(k, u32::from(p.petals)) * TAU;
                let dir = side * a.cos() + up * a.sin();
                let rib: Vec<Vec3> =
                    (5..=10).map(|i| throat + (mouth - throat) * frac(i, 10) + dir * (radius(frac(i, 10)) + 0.003)).collect();
                horn.tube(&rib, 0.004, 6, darker(p.metal, 0.2));
            }
        }
        Ok(output(vec![
            (cabinet, painted(darker(p.cabinet, 0.6))),
            (metal, painted(darker(p.metal, 0.55))),
            (horn, painted(darker(p.metal, 0.55))),
        ]))
    }
}

register_prop_kind!(Gramophone);

/// A framed picture.
pub struct Picture;

/// The picture's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PictureParams {
    /// Metres, the frame's outside, 0.15 to 3.
    pub width: f32,
    /// Metres, the frame's outside, 0.15 to 3.
    pub height: f32,
    /// The moulding's width in metres, 0.01 to 0.2.
    pub frame_width: f32,
    /// The frame.
    pub frame: Colour,
    /// A mat inside the frame, this wide in metres (0 for none), 0 to 0.2.
    pub mat_width: f32,
    /// The mat.
    pub mat: Colour,
    /// The canvas: its ground colour, darkening a little toward the edges.
    pub canvas: Colour,
    /// An oval medallion on the canvas (a portrait's ground, an emblem), none when absent.
    pub medallion: Option<Colour>,
}

impl Default for PictureParams {
    fn default() -> Self {
        Self {
            width: 0.6,
            height: 0.8,
            frame_width: 0.05,
            frame: BRASS,
            mat_width: 0.0,
            mat: Colour::hex(0x00e8_dcc4),
            canvas: Colour::hex(0x0041_4a5a),
            medallion: None,
        }
    }
}

impl PropKind for Picture {
    type Params = PictureParams;
    const ID: &'static str = "picture";
    const SUMMARY: &'static str = "A framed picture to hang on a wall: its bottom edge at y = 0, its face toward +Z, \
                                   2 cm deep. Size, the moulding, a mat, the canvas and an oval medallion on it.";

    fn validate(p: &PictureParams) -> Result<(), String> {
        between("width", p.width, 0.15, 3.0)?;
        between("height", p.height, 0.15, 3.0)?;
        between("frame_width", p.frame_width, 0.01, 0.2)?;
        between("mat_width", p.mat_width, 0.0, 0.2)?;
        if 2.0 * (p.frame_width + p.mat_width) >= p.width.min(p.height) {
            return Err("the frame and the mat leave no canvas: make them narrower or the picture larger".to_owned());
        }
        Ok(())
    }

    fn build(p: &PictureParams) -> Result<PropOutput, String> {
        let (w, h, f) = (p.width * 0.5, p.height, p.frame_width);
        let depth = 0.02;
        let mut frame = Shape::new();
        // Four bevelled bars, the outer edge standing a little proud.
        frame.bevelled(At::xyz(0.0, f * 0.5, depth * 0.5), Vec3::new(w, f * 0.5, depth * 0.5), f * 0.2, p.frame);
        frame.bevelled(At::xyz(0.0, h - f * 0.5, depth * 0.5), Vec3::new(w, f * 0.5, depth * 0.5), f * 0.2, p.frame);
        for x in [-1.0, 1.0] {
            frame.bevelled(At::xyz(x * (w - f * 0.5), h * 0.5, depth * 0.5), Vec3::new(f * 0.5, h * 0.5 - f, depth * 0.5), f * 0.2, p.frame);
        }
        // A thin inner lip, darker, where the frame meets the picture.
        let lip = darker(p.frame, 0.35);
        let (iw, ih0, ih1) = (w - f, f, h - f);
        frame.block(Vec3::new(-iw, ih0, depth * 0.3), Vec3::new(iw, ih0 + 0.006, depth * 0.75), lip);
        frame.block(Vec3::new(-iw, ih1 - 0.006, depth * 0.3), Vec3::new(iw, ih1, depth * 0.75), lip);
        for x in [-1.0, 1.0] {
            let (x0, x1) = if x < 0.0 { (-iw, -iw + 0.006) } else { (iw - 0.006, iw) };
            frame.block(Vec3::new(x0, ih0, depth * 0.3), Vec3::new(x1, ih1, depth * 0.75), lip);
        }
        let mut face = Shape::new();
        let m = p.mat_width;
        if m > 0.0 {
            face.block(Vec3::new(-iw, ih0, 0.0), Vec3::new(iw, ih1, depth * 0.45), p.mat);
        }
        // The canvas in a few nested panels, each a shade lighter toward the middle: a painted ground, not a flat card.
        let (cw, c0, c1) = (iw - m, ih0 + m, ih1 - m);
        let steps = 4;
        for i in 0..steps {
            let t = frac(i, steps);
            let inset = t * 0.18 * cw.min((c1 - c0) * 0.5);
            let shade = mix(darker(p.canvas, 0.25), p.canvas, t * 1.33);
            face.block(Vec3::new(-cw + inset, c0 + inset, 0.0), Vec3::new(cw - inset, c1 - inset, depth * 0.5 + 0.0004 * t), shade);
        }
        if let Some(medallion) = p.medallion {
            let (rx, ry) = (cw * 0.62, (c1 - c0) * 0.3);
            face.ellipsoid(At::xyz(0.0, f32::midpoint(c0, c1) + (c1 - c0) * 0.08, depth * 0.5), Vec3::new(rx, ry, 0.002), 6, 48, medallion);
        }
        Ok(output(vec![(frame, painted(darker(p.frame, 0.6))), (face, painted_plain())]))
    }
}

register_prop_kind!(Picture);

/// A lidded vase with handles.
pub struct Urn;

/// The urn's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UrnParams {
    /// Metres from the foot to the lid's knob, 0.15 to 1.5.
    pub height: f32,
    /// The body.
    pub colour: Colour,
    /// The foot, the bands, the handles and the knob.
    pub trim: Colour,
    /// Two handles on the shoulders.
    pub handles: bool,
    /// A lid with a knob; open-mouthed without.
    pub lid: bool,
}

impl Default for UrnParams {
    fn default() -> Self {
        Self { height: 0.5, colour: Colour::hex(0x0036_4a78), trim: BRASS, handles: true, lid: true }
    }
}

impl PropKind for Urn {
    type Params = UrnParams;
    const ID: &'static str = "urn";
    const SUMMARY: &'static str = "A lidded vase standing on y = 0: a stepped foot, a round body with bands, a neck, two \
                                   handles and a lid with a knob. Height, body and trim colours, handles and lid.";

    fn validate(p: &UrnParams) -> Result<(), String> {
        between("height", p.height, 0.15, 1.5)
    }

    fn build(p: &UrnParams) -> Result<PropOutput, String> {
        let h = p.height;
        let r = h * 0.24;
        let mut body = Shape::new();
        let mut trim = Shape::new();
        // Foot: a plinth, then a stem.
        trim.frustum(At::xyz(0.0, 0.0, 0.0), r * 0.62, r * 0.55, h * 0.04, 32, (true, true), p.trim);
        trim.frustum(At::xyz(0.0, h * 0.04, 0.0), r * 0.4, r * 0.3, h * 0.08, 32, (true, true), darker(p.trim, 0.1));
        // The body: an egg wider at the shoulder.
        let centre = h * 0.12 + r * 1.15;
        body.ellipsoid(At::xyz(0.0, centre, 0.0), Vec3::new(r, r * 1.2, r), 16, 40, p.colour);
        // Two bands: one at the waist, one at the shoulder.
        for (y, rad) in [(centre - r * 0.55, r * 0.84), (centre + r * 0.5, r * 0.87)] {
            trim.cylinder(At::xyz(0.0, y, 0.0), rad, r * 0.1, 40, p.trim);
        }
        // The neck and its lip.
        let neck = centre + r * 1.1;
        body.frustum(At::xyz(0.0, neck - r * 0.2, 0.0), r * 0.45, r * 0.38, r * 0.45, 32, (false, true), darker(p.colour, 0.1));
        trim.frustum(At::xyz(0.0, neck + r * 0.22, 0.0), r * 0.42, r * 0.48, r * 0.1, 32, (true, true), p.trim);
        let top = neck + r * 0.32;
        if p.lid {
            body.ellipsoid(At::xyz(0.0, top, 0.0), Vec3::new(r * 0.46, r * 0.22, r * 0.46), 8, 32, p.colour);
            trim.frustum(At::xyz(0.0, top + r * 0.15, 0.0), r * 0.1, 0.0, (h - top - r * 0.15).max(r * 0.2), 16, (true, false), p.trim);
        }
        if p.handles {
            for side in [-1.0_f32, 1.0] {
                let points: Vec<Vec3> = (0..=10)
                    .map(|i| {
                        let a = PI * frac(i, 10);
                        Vec3::new(side * (r * 0.85 + a.sin() * r * 0.45), centre + r * 0.55 + a.cos() * r * 0.35, 0.0)
                    })
                    .collect();
                trim.tube(&points, r * 0.06, 10, p.trim);
            }
        }
        Ok(output(vec![(body, painted(darker(p.colour, 0.6))), (trim, painted(darker(p.trim, 0.55)))]))
    }
}

register_prop_kind!(Urn);

/// A chest.
pub struct Chest;

/// The chest's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChestParams {
    /// Metres along x, 0.2 to 1.5.
    pub width: f32,
    /// Metres along z, 0.15 to 1.0.
    pub depth: f32,
    /// Metres to the top of the lid, 0.15 to 1.0.
    pub height: f32,
    /// A rounded lid; flat without.
    pub domed: bool,
    /// The wood or leather.
    pub colour: Colour,
    /// The straps, corners and lock.
    pub metal: Colour,
}

impl Default for ChestParams {
    fn default() -> Self {
        Self { width: 0.6, depth: 0.38, height: 0.42, domed: true, colour: Colour::hex(0x008a_6a4a), metal: BRASS }
    }
}

impl PropKind for Chest {
    type Params = ChestParams;
    const ID: &'static str = "chest";
    const SUMMARY: &'static str = "A chest standing on y = 0, its front toward +Z: a box and a flat or rounded lid, \
                                   bound by metal straps with corner caps and a lock plate. Size, lid, wood and metal.";

    fn validate(p: &ChestParams) -> Result<(), String> {
        between("width", p.width, 0.2, 1.5)?;
        between("depth", p.depth, 0.15, 1.0)?;
        between("height", p.height, 0.15, 1.0)
    }

    fn build(p: &ChestParams) -> Result<PropOutput, String> {
        let (w, d) = (p.width * 0.5, p.depth * 0.5);
        let lid_h = if p.domed { d.min(p.height * 0.4) } else { p.height * 0.18 };
        let body_h = p.height - lid_h;
        let mut wood = Shape::new();
        let mut metal = Shape::new();
        wood.bevelled(At::xyz(0.0, body_h * 0.5, 0.0), Vec3::new(w, body_h * 0.5, d), 0.006, p.colour);
        if p.domed {
            // A rounded lid: the upper half of an ellipsoid over the box (its lower half is inside it).
            wood.ellipsoid(At::xyz(0.0, body_h, 0.0), Vec3::new(w, lid_h, d), 12, 40, mix(p.colour, Colour::WHITE, 0.05));
        } else {
            wood.bevelled(At::xyz(0.0, body_h + lid_h * 0.5, 0.0), Vec3::new(w + 0.008, lid_h * 0.5, d + 0.008), 0.006, mix(p.colour, Colour::WHITE, 0.05));
        }
        // Straps over the front, a third in from each end, and caps on the front corners.
        let strap = (p.width * 0.04).max(0.015);
        for x in [-w * 0.55, w * 0.55] {
            metal.block(Vec3::new(x - strap * 0.5, 0.0, d), Vec3::new(x + strap * 0.5, body_h, d + 0.006), p.metal);
        }
        for x in [-1.0, 1.0] {
            metal.block(Vec3::new(x * w - strap * 0.5 * (1.0 + x), 0.0, d - 0.004), Vec3::new(x * w + strap * 0.5 * (1.0 - x), strap * 2.0, d + 0.008), p.metal);
            metal.block(Vec3::new(x * w - strap * 0.5 * (1.0 + x), body_h - strap * 2.0, d - 0.004), Vec3::new(x * w + strap * 0.5 * (1.0 - x), body_h, d + 0.008), p.metal);
        }
        // A band along the box's rim, and the lock plate.
        metal.block(Vec3::new(-w - 0.003, body_h - strap * 0.6, -d - 0.003), Vec3::new(w + 0.003, body_h, d + 0.007), darker(p.metal, 0.1));
        metal.bevelled(At::xyz(0.0, body_h - strap * 1.6, d + 0.006), Vec3::new(strap * 1.2, strap * 1.4, 0.004), 0.002, p.metal);
        Ok(output(vec![(wood, painted(darker(p.colour, 0.6))), (metal, painted(darker(p.metal, 0.55)))]))
    }
}

register_prop_kind!(Chest);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::shape::tests::assert_solid;

    #[test]
    fn each_kind_builds_with_its_defaults_and_stands_on_the_floor() {
        let builds = [
            Gramophone::build(&GramophoneParams::default()),
            Picture::build(&PictureParams { medallion: Some(Colour::hex(0x00d8_c8a0)), mat_width: 0.04, ..PictureParams::default() }),
            Urn::build(&UrnParams::default()),
            Chest::build(&ChestParams::default()),
            Chest::build(&ChestParams { domed: false, ..ChestParams::default() }),
        ];
        for build in builds {
            let out = build.expect("builds");
            assert!(!out.parts.is_empty());
            let lowest = out
                .parts
                .iter()
                .flat_map(|part| part.mesh.positions.iter().map(|p| p.y))
                .fold(f32::INFINITY, f32::min);
            assert!(lowest.abs() < 0.01, "stands on y = 0: lowest point {lowest}");
        }
    }

    #[test]
    fn the_picture_refuses_a_frame_that_leaves_no_canvas() {
        let p = PictureParams { width: 0.2, frame_width: 0.06, mat_width: 0.05, ..PictureParams::default() };
        assert!(Picture::validate(&p).is_err());
        assert!(Picture::validate(&PictureParams::default()).is_ok());
    }

    #[test]
    fn the_urn_and_the_gramophone_cabinet_are_closed_solids() {
        let urn = Urn::build(&UrnParams { handles: false, lid: false, ..UrnParams::default() }).expect("builds");
        // The foot and the bands are capped frustums and cylinders: closed.
        assert_solid(&urn.parts[1].mesh, true);
    }
}
