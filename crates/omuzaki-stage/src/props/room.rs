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
}

impl Default for RugParams {
    fn default() -> Self {
        Self { width: 2.0, length: 1.4, colour: Colour::hex(0x00a2_4a3e), border: Some(Colour::hex(0x00e0_c79a)) }
    }
}

impl PropKind for Rug {
    type Params = RugParams;
    const ID: &'static str = "rug";
    const SUMMARY: &'static str = "A flat rug lying on y = 0 with a border band. Size and colours.";

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
        Ok(unshadowed(output(vec![(rug, painted_plain())])))
    }
}

register_prop_kind!(Rug);

/// `out` with no part in the shadow maps: a floor and walls only receive (a directional key's map is fitted to its
/// casters, and a room-sized caster would spread the cast's shadow over it), and a rug lies flat.
fn unshadowed(mut out: PropOutput) -> PropOutput {
    out.parts = out.parts.into_iter().map(PropPart::without_shadow).collect();
    out
}
