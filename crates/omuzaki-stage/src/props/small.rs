//! Small things: `cup`, the thing in a hand (ST-03's mug), and `plant`, a pot of seeded leaves.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, mix, painted};
use super::{Seeded, between, output};

/// A cup.
pub struct Cup;

/// The cup's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CupParams {
    /// Metres, 0.05 to 0.16.
    pub height: f32,
    /// The rim's radius in metres, 0.025 to 0.07.
    pub radius: f32,
    /// The glaze.
    pub colour: Colour,
    /// A handle on the +X side.
    pub handle: bool,
    /// A saucer under it.
    pub saucer: bool,
    /// The drink's colour, a little below the rim; none for an empty cup.
    pub drink: Option<Colour>,
}

impl Default for CupParams {
    fn default() -> Self {
        Self {
            height: 0.09,
            radius: 0.042,
            colour: Colour::hex(0x00d9_c7f0),
            handle: true,
            saucer: false,
            drink: Some(Colour::hex(0x005a_3826)),
        }
    }
}

impl PropKind for Cup {
    type Params = CupParams;
    const ID: &'static str = "cup";
    const SUMMARY: &'static str =
        "A cup or mug standing on y = 0: height, rim radius, glaze, a handle, a saucer and the drink in it.";

    fn validate(p: &CupParams) -> Result<(), String> {
        between("height", p.height, 0.05, 0.16)?;
        between("radius", p.radius, 0.025, 0.07)
    }

    fn build(p: &CupParams) -> Result<PropOutput, String> {
        let (h, r) = (p.height, p.radius);
        let lift = if p.saucer { 0.012 } else { 0.0 };
        let mut cup = Shape::new();
        cup.vessel(At::xyz(0.0, lift, 0.0), r * 0.84, r, h, r * 0.09, true, 36, p.colour);
        if p.handle {
            let points: Vec<Vec3> = (0..=10)
                .map(|i| {
                    let a = std::f32::consts::PI * f32::from(u8::try_from(i).unwrap_or(0)) / 10.0;
                    Vec3::new(r * 0.92 + a.sin() * h * 0.3, lift + h * 0.5 + a.cos() * h * 0.3, 0.0)
                })
                .collect();
            cup.tube(&points, h * 0.055, 10, p.colour);
        }
        if p.saucer {
            cup.frustum(At::xyz(0.0, 0.0, 0.0), r * 1.2, r * 1.75, lift, 40, (true, true), p.colour);
        }
        let mut parts = vec![(cup, painted(darker(p.colour, 0.5)))];
        if let Some(drink) = p.drink {
            let mut surface = Shape::new();
            let wall = r * 0.09;
            let level = h * 0.82;
            let at_level = r * 0.84 + (r - r * 0.84) * (level / h) - wall - 0.0005;
            surface.cylinder(At::xyz(0.0, lift + level - 0.004, 0.0), at_level, 0.004, 32, drink);
            parts.push((surface, painted(darker(drink, 0.5))));
        }
        Ok(output(parts))
    }
}

register_prop_kind!(Cup);

/// A potted plant.
pub struct Plant;

/// The plant's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlantParams {
    /// Metres to the top of the leaves, 0.2 to 2.0.
    pub height: f32,
    /// Leaves, 4 to 60.
    pub leaves: u8,
    /// The leaves' green.
    pub leaf: Colour,
    /// The pot.
    pub pot: Colour,
    /// Arranges the leaves.
    pub seed: u32,
}

impl Default for PlantParams {
    fn default() -> Self {
        Self { height: 0.7, leaves: 22, leaf: Colour::hex(0x004f_8a4a), pot: Colour::hex(0x00c2_7550), seed: 3 }
    }
}

impl PropKind for Plant {
    type Params = PlantParams;
    const ID: &'static str = "plant";
    const SUMMARY: &'static str = "A potted plant: a clay pot and seeded leaves arching out from its middle. Height, \
                                   leaf count, colours and seed.";

    fn validate(p: &PlantParams) -> Result<(), String> {
        between("height", p.height, 0.2, 2.0)?;
        if !(4..=60).contains(&p.leaves) {
            return Err(format!("leaves is 4 to 60, got {}", p.leaves));
        }
        Ok(())
    }

    fn build(p: &PlantParams) -> Result<PropOutput, String> {
        let pot_h = p.height * 0.3;
        let pot_r = pot_h * 0.62;
        let mut pot = Shape::new();
        pot.vessel(At::xyz(0.0, 0.0, 0.0), pot_r * 0.75, pot_r, pot_h, pot_r * 0.08, true, 32, p.pot);
        pot.frustum(At::xyz(0.0, pot_h - pot_h * 0.14, 0.0), pot_r * 1.04, pot_r * 1.06, pot_h * 0.14, 32, (true, true), p.pot);
        let soil_r = pot_r * 0.9;
        pot.cylinder(At::xyz(0.0, pot_h * 0.78, 0.0), soil_r, 0.01, 24, Colour::hex(0x003a_2a20));
        let mut leaves = Shape::new();
        let mut rng = Seeded::new(p.seed);
        let reach = p.height - pot_h * 0.8;
        for i in 0..p.leaves {
            let t = f32::from(i) / f32::from(p.leaves);
            let yaw = t * 360.0 * 2.618 + rng.range(-15.0, 15.0);
            // Inner leaves stand up, outer ones arch out.
            let tilt = rng.range(8.0, 62.0);
            let length = reach * rng.range(0.55, 1.0) / (tilt.to_radians().cos() * 0.6 + 0.4);
            let shade = mix(p.leaf, darker(p.leaf, 0.35), rng.next());
            leaves.leaf(
                At::xyz(rng.range(-0.2, 0.2) * pot_r, pot_h * 0.8, rng.range(-0.2, 0.2) * pot_r).yaw(yaw).pitch(tilt),
                length.min(reach * 1.3),
                length * rng.range(0.16, 0.24),
                length * 0.25,
                shade,
            );
        }
        // Each leaf has a face on both sides of its own, so it reads from behind with culling on.
        Ok(output(vec![(pot, painted(darker(p.pot, 0.5))), (leaves, painted(darker(p.leaf, 0.55)))]))
    }
}

register_prop_kind!(Plant);
