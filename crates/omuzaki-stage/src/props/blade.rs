//! `blade`: a sword or a dagger, to stand in a scene or rest in a hand (#2715). Built along +Y from the middle of its grip
//! at the origin, so a hand holding it holds the origin: the blade above, the guard across it, the grip and pommel
//! below. Sheathed, a scabbard in its own colour covers the blade.

use omuzaki_render_api::{Colour, Material, ParamValue, ProgramParams, Shading};
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::{At, Shape, darker, painted};
use super::{between, output};

/// A blade.
pub struct Blade;

/// The blade's parameters. A sword by default; a dagger is short (`length` 0.2), narrow and has a small guard.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BladeParams {
    /// The blade's length in metres, from the guard to the tip, 0.1 to 1.2.
    pub length: f32,
    /// Its width at the base, metres, 0.012 to 0.1.
    pub width: f32,
    /// Its thickness at the spine, metres, 0.002 to 0.015.
    pub thickness: f32,
    /// How much of its length narrows to the point, metres, 0 (a blunt end) up to the length.
    pub point: f32,
    /// How far the tip curves forward (+X, a sabre or a katana), metres, 0 to 0.2.
    pub curve: f32,
    /// The cross-guard's span, metres, 0 (none) to 0.35.
    pub guard: f32,
    /// The grip's length, metres, 0.05 to 0.4.
    pub grip: f32,
    /// A round pommel at the end of the grip.
    pub pommel: bool,
    /// A scabbard over the blade.
    pub sheathed: bool,
    /// The blade's steel.
    pub steel: Colour,
    /// The guard and pommel.
    pub fittings: Colour,
    /// The grip's wrap.
    pub wrap: Colour,
    /// The scabbard.
    pub scabbard: Colour,
}

impl Default for BladeParams {
    fn default() -> Self {
        Self {
            length: 0.75,
            width: 0.045,
            thickness: 0.006,
            point: 0.12,
            curve: 0.0,
            guard: 0.18,
            grip: 0.18,
            pommel: true,
            sheathed: false,
            steel: Colour::hex(0x00c8_ced8),
            fittings: Colour::hex(0x00b0_8a40),
            wrap: Colour::hex(0x004a_2e22),
            scabbard: Colour::hex(0x003a_2a24),
        }
    }
}

/// Steel and brass: the `metal` program with its bands tight and bright, so a blade flashes as it turns.
fn metal(line: Colour, shine: Colour) -> Material {
    let mut material = painted(line);
    let mut params = ProgramParams::new();
    if let (Ok(shine_key), Ok(gloss)) = (omuzaki_core::id::Slug::new("shine"), omuzaki_core::id::Slug::new("gloss")) {
        params.insert(shine_key, ParamValue::Colour(shine));
        params.insert(gloss, ParamValue::Float(0.8));
    }
    if let Ok(name) = omuzaki_core::id::Slug::new("metal") {
        material.shading = Shading::Program { name, params };
    }
    material
}

impl PropKind for Blade {
    type Params = BladeParams;
    const ID: &'static str = "blade";
    const SUMMARY: &'static str = "A sword or a dagger, built up from the middle of its grip (hold it there): blade length, \
                                   width, thickness, point and curve, a cross-guard, grip and pommel, sheathed or bare, \
                                   and the colours of steel, fittings, wrap and scabbard.";

    fn validate(p: &BladeParams) -> Result<(), String> {
        between("length", p.length, 0.1, 1.2)?;
        between("width", p.width, 0.012, 0.1)?;
        between("thickness", p.thickness, 0.002, 0.015)?;
        between("point", p.point, 0.0, p.length)?;
        between("curve", p.curve, 0.0, 0.2)?;
        between("guard", p.guard, 0.0, 0.35)?;
        between("grip", p.grip, 0.05, 0.4)
    }

    fn build(p: &BladeParams) -> Result<PropOutput, String> {
        let half_grip = p.grip * 0.5;
        let guard_depth = (p.width * 0.35).max(0.012);
        let base = half_grip + guard_depth;
        let mut steel = Shape::new();
        let mut fittings = Shape::new();
        let mut cloth = Shape::new();
        if p.sheathed {
            // A scabbard a little wider and thicker than the blade, its end rounded off rather than pointed.
            let (w, t) = (p.width * 1.25 + 0.006, p.thickness * 2.0 + 0.008);
            cloth.blade(At::xyz(0.0, base, 0.0), p.length + 0.01, w, t, (w * 1.5).min(p.length), p.curve, p.scabbard);
            // The chape and the throat: metal bands at the end and the mouth.
            fittings.cuboid(At::xyz(0.0, base + 0.012, 0.0), [w * 0.56, 0.012, t * 0.6].into(), p.fittings);
        } else {
            steel.blade(At::xyz(0.0, base, 0.0), p.length, p.width, p.thickness, p.point, p.curve, p.steel);
        }
        if p.guard > 0.0 {
            let span = p.guard.max(p.width);
            fittings.bevelled(
                At::xyz(0.0, half_grip + guard_depth * 0.5, 0.0),
                [span * 0.5, guard_depth * 0.5, (p.thickness * 2.5).max(0.012)].into(),
                guard_depth * 0.2,
                p.fittings,
            );
        } else {
            // No guard: a collar where the blade meets the grip.
            fittings.cylinder(At::xyz(0.0, half_grip, 0.0), p.width * 0.32 + 0.004, guard_depth, 16, p.fittings);
        }
        // The grip: a wrapped tube, its ridges the turns of the wrap.
        let grip_radius = (p.width * 0.28).clamp(0.009, 0.018);
        let turns = (p.grip / 0.018).round().max(3.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a few dozen turns
        let rings = turns as u32 * 2;
        for i in 0..rings {
            #[allow(clippy::cast_precision_loss)] // small counts
            let (y0, y1) = (-half_grip + p.grip * i as f32 / rings as f32, -half_grip + p.grip * (i + 1) as f32 / rings as f32);
            let swell = if i % 2 == 0 { 1.0 } else { 0.9 };
            cloth.cylinder(At::xyz(0.0, y0, 0.0), grip_radius * swell, y1 - y0, 14, p.wrap);
        }
        if p.pommel {
            let r = grip_radius * 1.45;
            fittings.ellipsoid(At::xyz(0.0, -half_grip - r * 0.8, 0.0), [r, r, r].into(), 10, 16, p.fittings);
        }
        let parts = vec![
            (steel, metal(darker(p.steel, 0.6), Colour::hex(0x00ff_ffff))),
            (fittings, metal(darker(p.fittings, 0.6), Colour::hex(0x00ff_e8b0))),
            (cloth, painted(darker(p.wrap, 0.55))),
        ];
        Ok(output(parts))
    }
}

register_prop_kind!(Blade);

#[cfg(test)]
mod tests {
    use super::super::shape::tests::assert_solid;
    use super::*;

    fn built(p: &BladeParams) -> PropOutput {
        Blade::validate(p).unwrap();
        Blade::build(p).unwrap()
    }

    #[test]
    fn a_sword_and_a_dagger_build_closed_at_their_extremes() {
        let dagger = BladeParams { length: 0.2, width: 0.025, point: 0.06, guard: 0.06, grip: 0.1, ..BladeParams::default() };
        let katana = BladeParams { length: 1.2, width: 0.03, curve: 0.2, guard: 0.0, grip: 0.4, pommel: false, ..BladeParams::default() };
        let smallest = BladeParams {
            length: 0.1,
            width: 0.012,
            thickness: 0.002,
            point: 0.0,
            guard: 0.0,
            grip: 0.05,
            ..BladeParams::default()
        };
        let widest =
            BladeParams { width: 0.1, thickness: 0.015, point: 0.75, guard: 0.35, sheathed: true, ..BladeParams::default() };
        for (name, p) in [("sword", BladeParams::default()), ("dagger", dagger), ("katana", katana), ("smallest", smallest), ("widest", widest)] {
            let out = built(&p);
            assert!(!out.parts.is_empty(), "{name}");
            for part in &out.parts {
                assert_solid(&part.mesh, true);
            }
            let top = out.parts.iter().flat_map(|part| part.mesh.positions.iter().map(|v| v.y)).fold(f32::MIN, f32::max);
            assert!(top > p.length * 0.95, "{name}: the blade reaches up its length ({top} for {})", p.length);
        }
    }

    #[test]
    fn out_of_range_numbers_are_refused_naming_the_field() {
        for (p, field) in [
            (BladeParams { length: 1.5, ..BladeParams::default() }, "length"),
            (BladeParams { width: 0.2, ..BladeParams::default() }, "width"),
            (BladeParams { thickness: 0.0, ..BladeParams::default() }, "thickness"),
            (BladeParams { point: 0.8, ..BladeParams::default() }, "point"),
            (BladeParams { curve: -0.1, ..BladeParams::default() }, "curve"),
            (BladeParams { guard: 0.5, ..BladeParams::default() }, "guard"),
            (BladeParams { grip: 0.01, ..BladeParams::default() }, "grip"),
        ] {
            let error = Blade::validate(&p).unwrap_err();
            assert!(error.starts_with(field), "{field}: {error}");
        }
    }

    #[test]
    fn the_curve_bends_the_tip_forward_and_the_scabbard_hides_the_steel() {
        let straight = built(&BladeParams::default());
        let curved = built(&BladeParams { curve: 0.15, ..BladeParams::default() });
        let reach = |out: &PropOutput| out.parts[0].mesh.positions.iter().map(|v| v.x).fold(f32::MIN, f32::max);
        assert!(reach(&curved) > reach(&straight) + 0.1, "the curved tip sits forward");
        let sheathed = built(&BladeParams { sheathed: true, ..BladeParams::default() });
        assert_eq!(sheathed.parts.len(), 2, "no bare steel: fittings and the scabbard with the grip");
    }
}
