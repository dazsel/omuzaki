//! The room itself: `room` (a planked floor and three walls, open toward the camera, with skirting, an optional
//! wainscot and a cornice) and `rug`.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, PropPart, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, mix, painted, painted_plain};
use super::{Seeded, between, output};

/// A room.
pub struct Room;

/// The room's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RoomParams {
    /// Metres along x, 2 to 12.
    pub width: f32,
    /// Metres along z, from the back wall to the open side, 2 to 12.
    pub depth: f32,
    /// Metres, 2.2 to 5.
    pub height: f32,
    /// The walls' paint.
    pub walls: Colour,
    /// The floor's wood (or its one colour, without planks).
    pub floor: Colour,
    /// Lay the floor in planks.
    pub planks: bool,
    /// Panelling on the lower walls, to 0.9 m; none when absent.
    pub wainscot: Option<Colour>,
    /// The skirting, the chair rail and the cornice.
    pub trim: Colour,
    /// Which walls stand: the back (-Z), the left (-X) and the right (+X).
    pub walls_up: [bool; 3],
    /// Varies the planks.
    pub seed: u32,
}

impl Default for RoomParams {
    fn default() -> Self {
        Self {
            width: 5.0,
            depth: 4.0,
            height: 2.7,
            walls: Colour::hex(0x00e9_dfcf),
            floor: Colour::hex(0x008a_5a3c),
            planks: true,
            wainscot: Some(Colour::hex(0x007a_5238)),
            trim: Colour::hex(0x00f3_ede2),
            walls_up: [true, true, true],
            seed: 7,
        }
    }
}

impl PropKind for Room {
    type Params = RoomParams;
    const ID: &'static str = "room";
    const SUMMARY: &'static str = "An interior: a floor centred on the origin and up to three walls (back, left, \
                                   right), open toward +Z, with skirting, a cornice and panelling if asked. Size, \
                                   paint, wood, planks.";

    fn validate(p: &RoomParams) -> Result<(), String> {
        between("width", p.width, 2.0, 12.0)?;
        between("depth", p.depth, 2.0, 12.0)?;
        between("height", p.height, 2.2, 5.0)
    }

    fn build(p: &RoomParams) -> Result<PropOutput, String> {
        let (w, d, h) = (p.width * 0.5, p.depth * 0.5, p.height);
        let wall = 0.1;
        let mut floor = Shape::new();
        if p.planks {
            let mut rng = Seeded::new(p.seed);
            let (plank_w, gap) = (0.14, 0.003);
            let rows = (p.width / plank_w).ceil();
            let mut x = -w;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few dozen rows
            for _ in 0..rows as u32 {
                let x1 = (x + plank_w).min(w);
                let mut z = -d - rng.range(0.0, 1.2);
                while z < d {
                    let len = rng.range(0.9, 1.8);
                    let (z0, z1) = (z.max(-d), (z + len).min(d));
                    if z1 - z0 > 0.02 {
                        let tone = mix(p.floor, darker(p.floor, 0.3), rng.next() * 0.6);
                        floor.block(Vec3::new(x + gap, -0.02, z0 + gap), Vec3::new(x1 - gap, 0.0, z1 - gap), tone);
                    }
                    z += len;
                }
                x = x1;
            }
            // The gaps read as dark lines: a dark board under the planks.
            floor.block(Vec3::new(-w, -0.03, -d), Vec3::new(w, -0.019, d), darker(p.floor, 0.65));
        } else {
            floor.block(Vec3::new(-w, -0.03, -d), Vec3::new(w, 0.0, d), p.floor);
        }
        let mut walls = Shape::new();
        let mut trim = Shape::new();
        let [back, left, right] = p.walls_up;
        // Each standing wall: (its inner face's outward normal, its span) as a box outside the floor.
        if back {
            walls.block(Vec3::new(-w - wall, -0.03, -d - wall), Vec3::new(w + wall, h, -d), p.walls);
        }
        for (up, side) in [(left, -1.0_f32), (right, 1.0)] {
            if up {
                let (x0, x1) = if side < 0.0 { (-w - wall, -w) } else { (w, w + wall) };
                walls.block(Vec3::new(x0, -0.03, -d - wall), Vec3::new(x1, h, d), p.walls);
            }
        }
        // Trim along each standing wall's inner face: skirting, a chair rail over the panelling, a cornice.
        let mut runs: Vec<(Vec3, Vec3, Vec3)> = Vec::new();
        if back {
            runs.push((Vec3::new(-w, 0.0, -d), Vec3::new(w, 0.0, -d), Vec3::Z));
        }
        if left {
            runs.push((Vec3::new(-w, 0.0, -d), Vec3::new(-w, 0.0, d), Vec3::X));
        }
        if right {
            runs.push((Vec3::new(w, 0.0, -d), Vec3::new(w, 0.0, d), Vec3::NEG_X));
        }
        for (a, b, inward) in runs {
            let band = |y0: f32, y1: f32, out: f32| {
                let lo = a.min(b) + Vec3::new(0.0, y0, 0.0);
                let hi = a.max(b) + Vec3::new(0.0, y1, 0.0);
                let (lo, hi) = (lo.min(lo + inward * out), hi.max(hi + inward * out));
                (lo, hi)
            };
            if let Some(panel) = p.wainscot {
                let (lo, hi) = band(0.0, 0.9, 0.012);
                walls.block(lo, hi, panel);
                let (lo, hi) = band(0.88, 0.93, 0.03);
                trim.block(lo, hi, p.trim);
            }
            let (lo, hi) = band(0.0, 0.12, 0.022);
            trim.block(lo, hi, p.trim);
            let (lo, hi) = band(h - 0.09, h, 0.05);
            trim.block(lo, hi, p.trim);
        }
        let mut parts = vec![(floor, painted_plain()), (walls, painted_plain())];
        if !trim.is_empty() {
            parts.push((trim, painted(darker(p.trim, 0.45))));
        }
        Ok(unshadowed(output(parts)))
    }
}

register_prop_kind!(Room);

/// A rug.
pub struct Rug;

/// The rug's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RugParams {
    /// Metres along x, 0.6 to 5.
    pub width: f32,
    /// Metres along z, 0.4 to 5.
    pub length: f32,
    /// The field.
    pub colour: Colour,
    /// The border band; the field's colour, darker, when absent.
    pub border: Option<Colour>,
    /// What is woven into the field.
    pub pattern: RugPattern,
    /// The pattern's colour; between the field and the border when absent.
    pub motif: Option<Colour>,
    /// Varies a scattered pattern.
    pub seed: u32,
}

/// What a rug's field carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RugPattern {
    /// An inner panel a shade toward the border, and nothing else.
    #[default]
    Plain,
    /// A central medallion ringed with leaves, and a quarter medallion in each corner.
    Medallion,
    /// Sprigs of leaves scattered over the field and a vine around its edge.
    Foliage,
    /// Diamonds on a grid.
    Lattice,
}

impl Default for RugParams {
    fn default() -> Self {
        Self {
            width: 2.0,
            length: 1.4,
            colour: Colour::hex(0x00a2_4a3e),
            border: Some(Colour::hex(0x00e0_c79a)),
            pattern: RugPattern::Plain,
            motif: None,
            seed: 3,
        }
    }
}

impl PropKind for Rug {
    type Params = RugParams;
    const ID: &'static str = "rug";
    const SUMMARY: &'static str = "A flat rug lying on y = 0 with a border band and a woven pattern (plain, \
                                   medallion, foliage or lattice). Size, colours and the pattern's colour.";

    fn validate(p: &RugParams) -> Result<(), String> {
        between("width", p.width, 0.6, 5.0)?;
        between("length", p.length, 0.4, 5.0)
    }

    fn build(p: &RugParams) -> Result<PropOutput, String> {
        let border = p.border.unwrap_or_else(|| darker(p.colour, 0.35));
        let (w, l) = (p.width * 0.5, p.length * 0.5);
        let band = (p.width.min(p.length) * 0.08).min(0.16);
        let mut rug = Shape::new();
        rug.bevelled(At::xyz(0.0, 0.005, 0.0), Vec3::new(w, 0.005, l), 0.004, border);
        rug.bevelled(At::xyz(0.0, 0.0075, 0.0), Vec3::new(w - band, 0.004, l - band), 0.003, p.colour);
        rug.bevelled(
            At::xyz(0.0, 0.0085, 0.0),
            Vec3::new(w - band * 2.0, 0.004, l - band * 2.0),
            0.003,
            mix(p.colour, border, 0.25),
        );
        let motif = p.motif.unwrap_or_else(|| mix(p.colour, border, 0.6));
        weave(&mut rug, p.pattern, motif, (w - band * 2.0, l - band * 2.0), p.seed);
        Ok(unshadowed(output(vec![(rug, painted_plain())])))
    }
}

register_prop_kind!(Rug);

/// The height a pattern lies at: just over the rug's top layer.
const WEAVE: f32 = 0.0128;

/// A leaf lying flat on the rug at `(x, z)`, its tip pointing `heading` degrees from +Z toward +X.
fn flat_leaf(rug: &mut Shape, x: f32, z: f32, heading: f32, length: f32, colour: Colour) {
    rug.leaf(At::xyz(x, WEAVE, z).yaw(heading).pitch(90.0), length, length * 0.42, 0.0, colour);
}

/// Weaves `pattern` in `motif` into a field of half-extents `field` (x, z) centred on the rug.
fn weave(rug: &mut Shape, pattern: RugPattern, motif: Colour, field: (f32, f32), seed: u32) {
    let (fw, fl) = field;
    if fw <= 0.05 || fl <= 0.05 {
        return;
    }
    let small = fw.min(fl);
    match pattern {
        RugPattern::Plain => {}
        RugPattern::Medallion => {
            // A medallion of two rings, leaves around it, and a quarter of one in each corner.
            let (rx, rz) = (fw * 0.32, fl * 0.32);
            rug.ellipsoid(At::xyz(0.0, WEAVE - 0.0005, 0.0), Vec3::new(rx, 0.001, rz), 2, 40, motif);
            rug.ellipsoid(At::xyz(0.0, WEAVE + 0.0003, 0.0), Vec3::new(rx * 0.6, 0.001, rz * 0.6), 2, 40, mix(motif, Colour::WHITE, 0.25));
            let leaves = 16_u8;
            for i in 0..leaves {
                let a = 360.0 * f32::from(i) / f32::from(leaves);
                let (sin, cos) = a.to_radians().sin_cos();
                flat_leaf(rug, sin * rx * 1.08, cos * rz * 1.08, a, small * 0.18, motif);
            }
            for (sx, sz) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                for k in 0..3_u8 {
                    let a: f32 = if sz > 0.0 { 0.0 } else { 180.0 };
                    let turn = a + sx * sz * (20.0 + 25.0 * f32::from(k));
                    flat_leaf(rug, sx * fw * 0.9, sz * fl * 0.9, turn + 180.0, small * 0.2, motif);
                }
            }
        }
        RugPattern::Foliage => {
            let mut rng = Seeded::new(seed);
            let pitch = (small * 0.45).max(0.25);
            let (nx, nz) = ((fw * 2.0 / pitch).floor().max(1.0), (fl * 2.0 / pitch).floor().max(1.0));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few dozen sprigs
            for (i, j) in (0..nx as u32).flat_map(|i| (0..nz as u32).map(move |j| (i, j))) {
                #[allow(clippy::cast_precision_loss)] // small counts
                let (cx, cz) = (-fw + pitch * (i as f32 + 0.5), -fl + pitch * (j as f32 + 0.5));
                let (cx, cz) = (cx + rng.range(-0.15, 0.15) * pitch, cz + rng.range(-0.15, 0.15) * pitch);
                let heading = rng.range(0.0, 360.0);
                for k in 0..3_u8 {
                    flat_leaf(rug, cx, cz, heading + 55.0 * (f32::from(k) - 1.0), pitch * 0.3, motif);
                }
            }
            // The vine: leaves pointing along each side, alternating in and out.
            let step = pitch * 0.35;
            for (along, half, fixed, axis_x) in [(fw, fw, fl, true), (fw, fw, -fl, true), (fl, fl, fw, false), (fl, fl, -fw, false)] {
                let mut t = -along + step * 0.5;
                let mut flip = 1.0;
                while t < half - step * 0.25 {
                    let inset = fixed - fixed.signum() * pitch * 0.12;
                    let (x, z, heading) = if axis_x { (t, inset, 90.0 + 40.0 * flip) } else { (inset, t, 40.0 * flip) };
                    flat_leaf(rug, x, z, heading, step * 0.9, motif);
                    t += step;
                    flip = -flip;
                }
            }
        }
        RugPattern::Lattice => {
            let pitch = (small * 0.3).max(0.18);
            let d = pitch * 0.22;
            let (nx, nz) = ((fw * 2.0 / pitch).floor().max(1.0), (fl * 2.0 / pitch).floor().max(1.0));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
            for i in 0..nx as u32 {
                for j in 0..nz as u32 {
                    let x = -fw + (fw * 2.0 - nx * pitch) * 0.5 + pitch * (i as f32 + 0.5);
                    let z = -fl + (fl * 2.0 - nz * pitch) * 0.5 + pitch * (j as f32 + 0.5);
                    rug.cuboid(At::xyz(x, WEAVE, z).yaw(45.0), Vec3::new(d, 0.0008, d), motif);
                }
            }
        }
    }
}

/// `out` with no part in the shadow maps: a floor and walls only receive (a directional key's map is fitted to its
/// casters, and a room-sized caster would spread the cast's shadow over it), and a rug lies flat.
fn unshadowed(mut out: PropOutput) -> PropOutput {
    out.parts = out.parts.into_iter().map(PropPart::without_shadow).collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_lies_flat_inside_the_rug_and_adds_to_the_plain_one() {
        let plain = Rug::build(&RugParams::default()).expect("builds");
        let count = plain.parts[0].mesh.positions.len();
        for pattern in [RugPattern::Medallion, RugPattern::Foliage, RugPattern::Lattice] {
            let p = RugParams { width: 3.0, length: 2.2, pattern, ..RugParams::default() };
            let out = Rug::build(&p).expect("builds");
            let positions = &out.parts[0].mesh.positions;
            assert!(positions.len() > count + 40, "{pattern:?} adds nothing");
            for q in positions {
                assert!(q.x.abs() <= p.width * 0.5 + 1e-4 && q.z.abs() <= p.length * 0.5 + 1e-4, "{pattern:?} spills: {q}");
                assert!(q.y >= -1e-4 && q.y < 0.016, "{pattern:?} stands up off the rug: {q}");
            }
        }
    }

    #[test]
    fn a_pattern_by_name_reads_from_its_params() {
        let p: RugParams = serde_json::from_value(serde_json::json!({ "pattern": "foliage", "motif": "#6a5a48" })).expect("parses");
        assert_eq!(p.pattern, RugPattern::Foliage);
    }
}
