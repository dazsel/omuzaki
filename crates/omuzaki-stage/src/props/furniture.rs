//! Furniture kinds: `desk`, `chair`, `cafe-table` and `bed`. Sizes are a person's: a desk top at 74 cm, a seat at
//! 45 cm, a bed's mattress top at about 55 cm (the sizes furniture makers publish), so a character of real stature
//! sits and leans on them.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, mix, painted};
use super::{between, output};

/// Wood a shade lighter than walnut: the default for furniture.
pub(crate) const WOOD: Colour = Colour::hex(0x009a_6b4a);

/// A desk.
pub struct Desk;

/// The desk's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DeskParams {
    /// Metres, 0.6 to 2.4.
    pub width: f32,
    /// Metres, 0.4 to 1.0.
    pub depth: f32,
    /// The top's height in metres, 0.6 to 1.1.
    pub height: f32,
    /// The wood.
    pub colour: Colour,
    /// A bank of drawers under the right end.
    pub drawers: bool,
}

impl Default for DeskParams {
    fn default() -> Self {
        Self { width: 1.2, depth: 0.6, height: 0.74, colour: WOOD, drawers: true }
    }
}

impl PropKind for Desk {
    type Params = DeskParams;
    const ID: &'static str = "desk";
    const SUMMARY: &'static str =
        "A desk: a top on four legs with a back rail, and a bank of drawers if asked. Width, depth, height and wood.";

    fn validate(p: &DeskParams) -> Result<(), String> {
        between("width", p.width, 0.6, 2.4)?;
        between("depth", p.depth, 0.4, 1.0)?;
        between("height", p.height, 0.6, 1.1)
    }

    fn build(p: &DeskParams) -> Result<PropOutput, String> {
        let (w, d, h) = (p.width * 0.5, p.depth * 0.5, p.height);
        let (top, leg, inset) = (0.03, 0.045, 0.03);
        let wood = p.colour;
        let side = darker(wood, 0.12);
        let mut s = Shape::new();
        s.bevelled(At::xyz(0.0, h - top * 0.5, 0.0), Vec3::new(w, top * 0.5, d), 0.006, wood);
        for x in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let at = Vec3::new(x * (w - inset - leg * 0.5), 0.0, z * (d - inset - leg * 0.5));
                s.block(at - Vec3::new(leg * 0.5, 0.0, leg * 0.5), at + Vec3::new(leg * 0.5, h - top, leg * 0.5), side);
            }
        }
        // A rail under the top at the back and the sides, so the frame reads as one piece.
        let rail = 0.08;
        s.block(
            Vec3::new(-w + inset, h - top - rail, -d + inset),
            Vec3::new(w - inset, h - top, -d + inset + 0.02),
            side,
        );
        if p.drawers {
            let bank = (p.width * 0.32).min(0.42);
            let x0 = w - inset - leg - bank;
            let body = Vec3::new(x0, h - top - 0.42, -d + inset + 0.02);
            s.block(body, Vec3::new(w - inset - leg, h - top, d - inset - 0.01), side);
            let handle = darker(wood, 0.55);
            for i in 0..3 {
                let y = h - top - 0.07 - f32::from(u8::try_from(i).unwrap_or(0)) * 0.13;
                let face = Vec3::new(x0 + 0.012, y - 0.055, d - inset - 0.01);
                s.block(face, Vec3::new(w - inset - leg - 0.012, y + 0.055, d - inset + 0.004), mix(wood, side, 0.4));
                s.cylinder(
                    At::xyz(x0 + bank * 0.5, y, d - inset + 0.004).pitch(90.0),
                    0.008,
                    0.018,
                    10,
                    handle,
                );
            }
        }
        Ok(output(vec![(s, painted(darker(wood, 0.6)))]))
    }
}

register_prop_kind!(Desk);

/// A chair.
pub struct Chair;

/// The chair's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ChairParams {
    /// The seat's height in metres, 0.35 to 0.6.
    pub seat_height: f32,
    /// The seat's width in metres, 0.35 to 0.6.
    pub width: f32,
    /// The back's height above the seat in metres, 0 (a stool) to 0.7.
    pub back: f32,
    /// The wood.
    pub colour: Colour,
    /// The seat cushion's cloth; none when absent.
    pub cushion: Option<Colour>,
}

impl Default for ChairParams {
    fn default() -> Self {
        Self { seat_height: 0.45, width: 0.44, back: 0.42, colour: WOOD, cushion: Some(Colour::hex(0x00b8_4a4a)) }
    }
}

impl PropKind for Chair {
    type Params = ChairParams;
    const ID: &'static str = "chair";
    const SUMMARY: &'static str =
        "A wooden chair facing +Z: four legs, a seat, a slatted back (0 for a stool) and a cushion if a colour is given.";

    fn validate(p: &ChairParams) -> Result<(), String> {
        between("seat_height", p.seat_height, 0.35, 0.6)?;
        between("width", p.width, 0.35, 0.6)?;
        between("back", p.back, 0.0, 0.7)
    }

    fn build(p: &ChairParams) -> Result<PropOutput, String> {
        let (w, sh) = (p.width * 0.5, p.seat_height);
        let d = w * 0.95;
        let (seat, leg) = (0.035, 0.032);
        let wood = p.colour;
        let frame = darker(wood, 0.1);
        let mut s = Shape::new();
        s.bevelled(At::xyz(0.0, sh - seat * 0.5, 0.0), Vec3::new(w, seat * 0.5, d), 0.008, wood);
        for x in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let at = Vec3::new(x * (w - leg * 0.6), 0.0, z * (d - leg * 0.6));
                let top = if z < 0.0 { sh + p.back } else { sh - seat };
                s.block(at - Vec3::new(leg * 0.5, 0.0, leg * 0.5), at + Vec3::new(leg * 0.5, top, leg * 0.5), frame);
            }
            // A stretcher between each side's legs, low down.
            s.block(
                Vec3::new(x * (w - leg * 0.6) - 0.01, 0.14, -d + leg),
                Vec3::new(x * (w - leg * 0.6) + 0.01, 0.17, d - leg),
                frame,
            );
        }
        if p.back > 0.05 {
            let z = -d + leg * 0.6;
            let slats = 3;
            for i in 0..slats {
                let t = (f32::from(u8::try_from(i).unwrap_or(0)) + 1.0) / f32::from(u8::try_from(slats + 1).unwrap_or(4));
                let y = sh + p.back * (0.25 + 0.7 * t);
                s.block(Vec3::new(-w + leg, y - 0.025, z - 0.01), Vec3::new(w - leg, y + 0.025, z + 0.01), wood);
            }
        }
        let mut parts = vec![(s, painted(darker(wood, 0.6)))];
        if let Some(cloth) = p.cushion {
            let mut c = Shape::new();
            c.bevelled(At::xyz(0.0, sh + 0.022, 0.01), Vec3::new(w * 0.9, 0.022, d * 0.88), 0.016, cloth);
            parts.push((c, painted(darker(cloth, 0.55))));
        }
        Ok(output(parts))
    }
}

register_prop_kind!(Chair);

/// A round café table.
pub struct CafeTable;

/// The café table's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CafeTableParams {
    /// The top's radius in metres, 0.25 to 0.6.
    pub radius: f32,
    /// The top's height in metres, 0.6 to 1.1.
    pub height: f32,
    /// The top.
    pub top: Colour,
    /// The pedestal and its foot.
    pub metal: Colour,
}

impl Default for CafeTableParams {
    fn default() -> Self {
        Self { radius: 0.35, height: 0.74, top: Colour::hex(0x00e8_dccb), metal: Colour::hex(0x0032_3238) }
    }
}

impl PropKind for CafeTable {
    type Params = CafeTableParams;
    const ID: &'static str = "cafe-table";
    const SUMMARY: &'static str = "A round café table on one pedestal and a round foot. Radius, height and colours.";

    fn validate(p: &CafeTableParams) -> Result<(), String> {
        between("radius", p.radius, 0.25, 0.6)?;
        between("height", p.height, 0.6, 1.1)
    }

    fn build(p: &CafeTableParams) -> Result<PropOutput, String> {
        let mut top = Shape::new();
        top.cylinder(At::xyz(0.0, p.height - 0.03, 0.0), p.radius, 0.03, 40, p.top);
        let mut metal = Shape::new();
        metal.frustum(At::xyz(0.0, 0.0, 0.0), p.radius * 0.62, p.radius * 0.5, 0.025, 32, (true, true), p.metal);
        metal.cylinder(At::xyz(0.0, 0.02, 0.0), 0.028, p.height - 0.05, 16, p.metal);
        metal.frustum(At::xyz(0.0, p.height - 0.07, 0.0), 0.03, 0.12, 0.04, 24, (true, true), p.metal);
        Ok(output(vec![(top, painted(darker(p.top, 0.6))), (metal, painted(darker(p.metal, 0.4)))]))
    }
}

register_prop_kind!(CafeTable);

/// A made bed.
pub struct Bed;

/// The bed's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BedParams {
    /// Metres, 0.9 (single) to 2.0.
    pub width: f32,
    /// Metres, 1.8 to 2.3.
    pub length: f32,
    /// The frame and headboard.
    pub frame: Colour,
    /// The sheet and pillows.
    pub sheet: Colour,
    /// The blanket over the lower two thirds.
    pub blanket: Colour,
    /// Pillows, 0 to 4.
    pub pillows: u8,
}

impl Default for BedParams {
    fn default() -> Self {
        Self {
            width: 1.4,
            length: 2.0,
            frame: WOOD,
            sheet: Colour::hex(0x00f2_efe8),
            blanket: Colour::hex(0x007c_8fb8),
            pillows: 2,
        }
    }
}

impl PropKind for Bed {
    type Params = BedParams;
    const ID: &'static str = "bed";
    const SUMMARY: &'static str =
        "A made bed, headboard toward -Z: a frame, a mattress, a blanket and pillows. Size, colours and pillow count.";

    fn validate(p: &BedParams) -> Result<(), String> {
        between("width", p.width, 0.9, 2.0)?;
        between("length", p.length, 1.8, 2.3)?;
        if p.pillows > 4 {
            return Err(format!("pillows is 0 to 4, got {}", p.pillows));
        }
        Ok(())
    }

    fn build(p: &BedParams) -> Result<PropOutput, String> {
        let (w, l) = (p.width * 0.5, p.length * 0.5);
        let frame_top = 0.32;
        let mut frame = Shape::new();
        frame.block(Vec3::new(-w - 0.03, 0.08, -l - 0.03), Vec3::new(w + 0.03, frame_top, l + 0.03), p.frame);
        for x in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let at = Vec3::new(x * (w - 0.02), 0.0, z * (l - 0.02));
                frame.block(at - Vec3::new(0.04, 0.0, 0.04), at + Vec3::new(0.04, 0.1, 0.04), darker(p.frame, 0.15));
            }
        }
        frame.bevelled(At::xyz(0.0, 0.5, -l - 0.05), Vec3::new(w + 0.05, 0.5, 0.035), 0.012, p.frame);
        let mut cloth = Shape::new();
        let mattress = 0.22;
        cloth.bevelled(
            At::xyz(0.0, frame_top + mattress * 0.5, 0.0),
            Vec3::new(w - 0.01, mattress * 0.5, l - 0.01),
            0.05,
            p.sheet,
        );
        let across = p.pillows.clamp(1, 2);
        let pillow_w = (p.width - 0.12) / f32::from(across);
        for i in 0..p.pillows {
            let (row, col) = (f32::from(i / 2), f32::from(i % 2));
            let x = if across == 1 { 0.0 } else { (col - 0.5) * pillow_w };
            let at = At::xyz(x, frame_top + mattress + 0.06 + row * 0.07, -l + 0.28 + row * 0.12).pitch(-12.0);
            cloth.bevelled(at, Vec3::new(pillow_w * 0.46, 0.06, 0.2), 0.05, mix(p.sheet, Colour::WHITE, 0.3));
        }
        let mut blanket = Shape::new();
        let reach = p.length * 0.66;
        blanket.bevelled(
            At::xyz(0.0, frame_top + mattress * 0.55, l - reach * 0.5 + 0.02),
            Vec3::new(w + 0.04, mattress * 0.55 + 0.012, reach * 0.5),
            0.04,
            p.blanket,
        );
        Ok(output(vec![
            (frame, painted(darker(p.frame, 0.6))),
            (cloth, painted(darker(p.sheet, 0.45))),
            (blanket, painted(darker(p.blanket, 0.5))),
        ]))
    }
}

register_prop_kind!(Bed);
