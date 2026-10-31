//! `staircase`: a straight flight of steps with a balustrade on either side or both: turned balusters, art-deco
//! metal panels or a solid wall, a sloped handrail, newel posts and an optional carpet runner. Procedural, in metres,
//! toon with vertex colours, like the other interior kinds.

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_render_api::Colour;
use omuzaki_stage_api::{PropKind, PropOutput, register_prop_kind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::decor::BRASS;
use super::shape::{At, Shape, darker, mix, painted};
use super::{between, output};

/// A straight staircase.
pub struct Staircase;

/// Which sides carry a balustrade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sides {
    /// Open steps, no balustrade.
    None,
    /// The left side (-X) only.
    Left,
    /// The right side (+X) only.
    Right,
    /// Both sides.
    #[default]
    Both,
}

/// What fills a balustrade under its handrail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Balustrade {
    /// Turned balusters standing on the steps.
    #[default]
    Balusters,
    /// Metal panels between the posts, each a frame round a fan of rays and a diamond: art deco.
    Panels,
    /// A solid sloped wall capped by the rail.
    Solid,
}

/// The staircase's parameters.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StaircaseParams {
    /// Metres across the flight (x), 0.6 to 4.
    pub width: f32,
    /// Metres from the floor to the top step, 0.2 to 6.
    pub rise: f32,
    /// Metres from the first riser to the top step's back edge (along -z), 0.3 to 12.
    pub run: f32,
    /// How many steps, 2 to 40.
    pub steps: u32,
    /// The steps' treads.
    pub tread: Colour,
    /// The risers and the flight's sides.
    pub riser: Colour,
    /// A carpet runner up the middle of the steps, this colour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carpet: Option<Colour>,
    /// Which sides carry a balustrade.
    pub sides: Sides,
    /// What fills the balustrade.
    pub balustrade: Balustrade,
    /// Metres from a step's nosing up to the top of the handrail, 0.5 to 1.4.
    pub rail_height: f32,
    /// The handrail and the newel posts.
    pub rail: Colour,
    /// The balusters, or the panels' frames and ornament.
    pub trim: Colour,
    /// The panels' ground behind their ornament (panels only).
    pub panel: Colour,
}

impl Default for StaircaseParams {
    fn default() -> Self {
        Self {
            width: 1.4,
            rise: 1.6,
            run: 2.6,
            steps: 9,
            tread: Colour::hex(0x00d8_ccb8),
            riser: Colour::hex(0x00c4_b6a0),
            carpet: None,
            sides: Sides::Both,
            balustrade: Balustrade::Balusters,
            rail_height: 0.9,
            rail: Colour::hex(0x004a_2e22),
            trim: BRASS,
            panel: Colour::hex(0x0030_3440),
        }
    }
}

impl PropKind for Staircase {
    type Params = StaircaseParams;
    const ID: &'static str = "staircase";
    const SUMMARY: &'static str = "A straight flight of steps standing on y = 0, its first riser at z = 0 and climbing \
                                   toward -Z: treads with nosings, an optional carpet runner, and a balustrade on \
                                   either side or both (turned balusters, art-deco metal panels or a solid wall) under \
                                   a sloped handrail between newel posts. Size, step count, colours.";

    fn validate(p: &StaircaseParams) -> Result<(), String> {
        between("width", p.width, 0.6, 4.0)?;
        between("rise", p.rise, 0.2, 6.0)?;
        between("run", p.run, 0.3, 12.0)?;
        between("rail_height", p.rail_height, 0.5, 1.4)?;
        if !(2..=40).contains(&p.steps) {
            return Err(format!("steps is 2 to 40, got {}", p.steps));
        }
        Ok(())
    }

    fn build(p: &StaircaseParams) -> Result<PropOutput, String> {
        let flight = Flight::of(p);
        let mut steps = Shape::new();
        let mut carpet = Shape::new();
        let mut rail = Shape::new();
        let mut trim = Shape::new();
        flight.steps(p, &mut steps, &mut carpet);
        let sides: &[f32] = match p.sides {
            Sides::None => &[],
            Sides::Left => &[-1.0],
            Sides::Right => &[1.0],
            Sides::Both => &[-1.0, 1.0],
        };
        for &side in sides {
            flight.balustrade(p, side, &mut rail, &mut trim, &mut steps);
        }
        let carpet_line = p.carpet.map_or(p.tread, |c| darker(c, 0.6));
        Ok(output(vec![
            (steps, painted(darker(p.riser, 0.55))),
            (carpet, painted(carpet_line)),
            (rail, painted(darker(p.rail, 0.5))),
            (trim, painted(darker(p.trim, 0.55))),
        ]))
    }
}

register_prop_kind!(Staircase);

/// A flight's measures.
struct Flight {
    /// Half the width.
    half: f32,
    /// One step's rise.
    riser: f32,
    /// One step's going (depth).
    going: f32,
    /// The step count.
    n: u32,
}

/// How far a tread's nosing reaches past its riser.
const NOSING: f32 = 0.025;
/// A tread's thickness.
const TREAD: f32 = 0.03;

impl Flight {
    fn of(p: &StaircaseParams) -> Self {
        // Step counts are at most 40: exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let n = p.steps as f32;
        Self { half: p.width * 0.5, riser: p.rise / n, going: p.run / n, n: p.steps }
    }

    /// The front edge (z) and top (y) of step `i`, counted from 0 at the bottom.
    fn step(&self, i: u32) -> (f32, f32) {
        // Step indices are at most 40: exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let k = i as f32;
        (-k * self.going, (k + 1.0) * self.riser)
    }

    /// The pitch line through the steps' nosings: its height at `z` (z <= 0), clamped to the flight.
    fn pitch(&self, z: f32) -> f32 {
        let top = self.step(self.n - 1).1;
        (self.riser + (-z) * self.riser / self.going).clamp(self.riser, top)
    }

    /// The solid steps, their nosings and the carpet runner.
    fn steps(&self, p: &StaircaseParams, steps: &mut Shape, carpet: &mut Shape) {
        let w = self.half;
        for i in 0..self.n {
            let (front, top) = self.step(i);
            // The step's body down to the floor, then a tread with a nosing over it.
            steps.block(Vec3::new(-w, 0.0, front - self.going), Vec3::new(w, top - TREAD, front), p.riser);
            steps.block(Vec3::new(-w, top - TREAD, front - self.going), Vec3::new(w, top, front + NOSING), p.tread);
            if let Some(c) = p.carpet {
                let cw = w * 0.72;
                let shade = mix(c, Colour::BLACK, 0.12);
                carpet.block(Vec3::new(-cw, top, front - self.going), Vec3::new(cw, top + 0.008, front + NOSING + 0.004), c);
                carpet.block(Vec3::new(-cw, top - self.riser, front + NOSING), Vec3::new(cw, top, front + NOSING + 0.008), shade);
            }
        }
    }

    /// One side's balustrade: newel posts at the foot and head, the fill, and the handrail along the pitch.
    fn balustrade(&self, p: &StaircaseParams, side: f32, rail: &mut Shape, trim: &mut Shape, steps: &mut Shape) {
        let x = side * (self.half - 0.06);
        let (foot, _) = self.step(0);
        let head = self.step(self.n - 1).0 - self.going * 0.5;
        let post = 0.07_f32;
        // Newel posts: a square shaft standing on the floor (or the top step), a collar and a ball.
        for (z, base) in [(foot - self.going * 0.5, 0.0), (head, self.step(self.n - 1).1)] {
            let top = self.pitch(z) + p.rail_height + 0.12;
            rail.bevelled(At::xyz(x, f32::midpoint(base, top), z), Vec3::new(post, (top - base) * 0.5, post), 0.008, p.rail);
            rail.bevelled(At::xyz(x, top + 0.015, z), Vec3::new(post + 0.012, 0.015, post + 0.012), 0.004, darker(p.rail, 0.1));
            trim.lathe(At::xyz(x, top + 0.03, z), &[Vec2::new(0.025, 0.0), Vec2::new(0.045, 0.04), Vec2::new(0.05, 0.075), Vec2::new(0.035, 0.11), Vec2::new(0.0, 0.125)], 20, p.trim);
        }
        // The handrail runs from post to post along the pitch.
        let (z0, z1) = (foot - self.going * 0.5, head);
        let line = |z: f32| self.pitch(z) + p.rail_height;
        let points: Vec<Vec3> = [z0, z1].iter().map(|&z| Vec3::new(x, line(z), z)).collect();
        rail.tube(&points, 0.035, 14, p.rail);
        match p.balustrade {
            Balustrade::Balusters => self.balusters(p, x, &line, trim),
            Balustrade::Panels => self.panels(p, x, (z0, z1), &line, rail, trim),
            Balustrade::Solid => {
                // A slab along the pitch from the rail down past the nosings (its foot runs inside the steps).
                let angle = (self.riser / self.going).atan();
                let along = (z0 - z1) / angle.cos();
                let (zm, depth) = (f32::midpoint(z0, z1), p.rail_height + self.riser);
                let centre = Vec3::new(x, line(zm) - 0.04 - depth * 0.5 * angle.cos(), zm);
                steps.cuboid(At::pos(centre).pitch(angle.to_degrees()), Vec3::new(0.05, depth * 0.5, along * 0.5), p.riser);
            }
        }
    }

    /// Turned balusters, two to a step, from the tread up to the rail.
    fn balusters(&self, p: &StaircaseParams, x: f32, line: &dyn Fn(f32) -> f32, trim: &mut Shape) {
        for i in 0..self.n {
            let (front, top) = self.step(i);
            for k in [0.25_f32, 0.75] {
                let z = front - self.going * k;
                if i == 0 && k < 0.5 {
                    continue; // the newel post stands there
                }
                let h = line(z) - top - 0.03;
                if h <= 0.1 {
                    continue;
                }
                let profile: Vec<Vec2> = BALUSTER.iter().map(|&(r, t)| Vec2::new(r, t * h)).collect();
                trim.lathe(At::xyz(x, top, z), &profile, 14, p.trim);
            }
        }
    }

    /// Metal panels between posts that split the flight into bays of about a metre.
    fn panels(&self, p: &StaircaseParams, x: f32, (z0, z1): (f32, f32), line: &dyn Fn(f32) -> f32, rail: &mut Shape, trim: &mut Shape) {
        let length = (z0 - z1).max(0.1);
        // Bays of about 0.9 m, at least one.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let bays = ((length / 0.9).round() as u32).max(1);
        #[allow(clippy::cast_precision_loss)]
        let bay = length / bays as f32;
        let bar = 0.012_f32;
        for b in 0..bays {
            #[allow(clippy::cast_precision_loss)]
            let (za, zb) = (z0 - bay * b as f32 - 0.08, z0 - bay * (b + 1) as f32 + 0.08);
            if b > 0 {
                // A post between bays, from the pitch to the rail.
                #[allow(clippy::cast_precision_loss)]
                let zp = z0 - bay * b as f32;
                let (lo, hi) = (self.pitch(zp) - self.riser * 0.5, line(zp));
                rail.bevelled(At::xyz(x, f32::midpoint(lo, hi), zp), Vec3::new(0.035, (hi - lo) * 0.5, 0.035), 0.005, p.rail);
            }
            // The panel's corners: a parallelogram that follows the pitch.
            let lo = |z: f32| self.pitch(z) + 0.04;
            let hi = |z: f32| line(z) - 0.07;
            let corners = [Vec3::new(x, lo(za), za), Vec3::new(x, lo(zb), zb), Vec3::new(x, hi(zb), zb), Vec3::new(x, hi(za), za)];
            // The ground: a thin sheet on each face of the balustrade.
            for face in [-1.0_f32, 1.0] {
                let off = Vec3::new(face * 0.004, 0.0, 0.0);
                let (o, u, v) = (corners[0] + off, corners[1] - corners[0], corners[3] - corners[0]);
                let (u, v) = if face > 0.0 { (u, v) } else { (v, u) };
                steps_sheet(trim, o, u, v, p.panel);
            }
            // The frame.
            let mut ring = corners.to_vec();
            ring.push(corners[0]);
            trim.tube(&ring, bar, 8, p.trim);
            // A fan of rays from the bottom middle, and a diamond above it.
            let mid = |t: f32, s: f32| {
                let z = za + (zb - za) * t;
                Vec3::new(x, lo(z) + (hi(z) - lo(z)) * s, z)
            };
            let root = mid(0.5, 0.0);
            for k in 0..7 {
                #[allow(clippy::cast_precision_loss)]
                let t = 0.08 + 0.84 * k as f32 / 6.0;
                trim.tube(&[root, mid(t, 0.62 + 0.3 * (1.0 - (t - 0.5).abs() * 2.0) * 0.4)], bar * 0.6, 6, p.trim);
            }
            let diamond = [mid(0.5, 0.42), mid(0.62, 0.68), mid(0.5, 0.94), mid(0.38, 0.68), mid(0.5, 0.42)];
            trim.tube(&diamond, bar * 0.8, 6, p.trim);
            trim.tube(&[mid(0.0, 0.5), mid(0.32, 0.5)], bar * 0.6, 6, p.trim);
            trim.tube(&[mid(0.68, 0.5), mid(1.0, 0.5)], bar * 0.6, 6, p.trim);
        }
    }
}

/// A plain sheet for a panel's ground, painted one colour.
fn steps_sheet(shape: &mut Shape, origin: Vec3, u: Vec3, v: Vec3, colour: Colour) {
    shape.sheet(origin, u, v, (1, 1), |_, _| colour);
}

/// A baluster's turned profile: `(radius, fraction of its height)` from the foot up.
const BALUSTER: [(f32, f32); 12] = [
    (0.026, 0.0),
    (0.026, 0.08),
    (0.02, 0.1),
    (0.018, 0.16),
    (0.03, 0.3),
    (0.033, 0.4),
    (0.022, 0.55),
    (0.013, 0.72),
    (0.018, 0.82),
    (0.013, 0.88),
    (0.022, 0.92),
    (0.022, 1.0),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::shape::tests::assert_solid;

    fn bounds(parts: &[omuzaki_stage_api::PropPart]) -> (Vec3, Vec3) {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for part in parts {
            for &p in &part.mesh.positions {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        (lo, hi)
    }

    #[test]
    fn the_default_flight_stands_on_the_floor_and_climbs_away() {
        let p = StaircaseParams::default();
        let out = Staircase::build(&p).unwrap();
        let (lo, hi) = bounds(&out.parts);
        assert!(lo.y.abs() < 1e-4, "stands on the floor: {lo}");
        assert!(hi.z < 0.05 && lo.z < -p.run + 0.01, "climbs toward -Z: {lo} {hi}");
        assert!(hi.y > p.rise + p.rail_height * 0.9, "the rail stands above the top step: {hi}");
        assert!(lo.x >= -p.width * 0.5 - 0.1 && hi.x <= p.width * 0.5 + 0.1, "stays within its width: {lo} {hi}");
        assert_solid(&out.parts[0].mesh, true);
    }

    #[test]
    fn every_balustrade_and_side_builds_and_a_carpet_adds_a_part() {
        for balustrade in [Balustrade::Balusters, Balustrade::Panels, Balustrade::Solid] {
            for sides in [Sides::None, Sides::Left, Sides::Right, Sides::Both] {
                let p = StaircaseParams { sides, balustrade, ..StaircaseParams::default() };
                Staircase::validate(&p).unwrap();
                let out = Staircase::build(&p).unwrap();
                for part in &out.parts {
                    part.mesh.validate().unwrap();
                }
                // Past the steps (part 0) come the rail and the trim, only on the sides asked for.
                let rails = &out.parts[1..];
                match sides {
                    Sides::None => assert!(rails.is_empty(), "{balustrade:?}: open steps"),
                    Sides::Left => assert!(bounds(rails).1.x < 0.0, "{balustrade:?}: only the left"),
                    Sides::Right => assert!(bounds(rails).0.x > 0.0, "{balustrade:?}: only the right"),
                    Sides::Both => assert!(bounds(rails).0.x < 0.0 && bounds(rails).1.x > 0.0, "{balustrade:?}: both"),
                }
            }
        }
        let plain = Staircase::build(&StaircaseParams::default()).unwrap().parts.len();
        let carpeted = StaircaseParams { carpet: Some(Colour::hex(0x0060_1820)), ..StaircaseParams::default() };
        assert_eq!(Staircase::build(&carpeted).unwrap().parts.len(), plain + 1);
    }

    #[test]
    fn the_rail_follows_the_pitch_of_the_steps() {
        let p = StaircaseParams::default();
        let f = Flight::of(&p);
        let (z, top) = f.step(4);
        assert!((f.pitch(z) - top).abs() < 1e-4, "the pitch line runs through the nosings");
        assert!((f.pitch(10.0) - f.riser).abs() < 1e-4 && (f.pitch(-100.0) - p.rise).abs() < 1e-4, "clamped to the flight");
    }

    #[test]
    fn out_of_range_params_are_refused() {
        for p in [
            StaircaseParams { steps: 1, ..StaircaseParams::default() },
            StaircaseParams { width: 0.1, ..StaircaseParams::default() },
            StaircaseParams { rail_height: 3.0, ..StaircaseParams::default() },
        ] {
            assert!(Staircase::validate(&p).is_err());
        }
    }
}
