//! The hair's shared volume and flow field (#2478): one parting, one crown whorl and one fullness over the head that every
//! generator reads ([`HairVolume`]), where each used to work out its own.
//!
//! **Directions.** Everything here is a unit direction from the head's centre, in the head's frame: `+y` up, `+z` the
//! face, `+x` the character's left.
//!
//! **The parting** is an arc on the head in a plane through its centre: the sagittal plane (`x = 0`) turned
//! [`HairParting::side_deg`] about the front-to-back axis, from [`HairParting::front_deg`] forward of the top to
//! [`HairParting::back_deg`] behind it ([`Field::part`]).
//!
//! **Combing.** A point of the scalp is combed away from its nearer source, the parting or the whorl ([`Field::comb`]):
//! its distance from that source is how far along the strands it lies, and its position along the parting (or its angle
//! round the whorl) says which strand it is on. The scalp shell lays its strand texture and `hair-uv` by it, so its
//! strands run off the parting on each side and radiate from the whorl behind it.
//!
//! **Fullness** ([`Field::fullness`]) scales how far each lock and shell stands off the head ([`swell`]), more at the crown
//! or the back as [`HairVolume::crown`] and [`HairVolume::back`] say, fading out below the ears so hanging hair keeps
//! its fall.

use omuzaki_char_api::{HairClump, HairVolume, HeadSurface};
use omuzaki_core::glam::{DQuat, DVec3, Vec3};
use omuzaki_core::mesh::MeshData;

use crate::hair::compile::{CompiledEntry, GroupGeometry};

/// The polar angles (radians from the top) over which [`swell`] fades out: whole above the first, none below the second.
const SWELL_FADE: (f64, f64) = (1.6, 2.1);

/// The volume's field over one head.
#[derive(Clone, Debug)]
pub struct Field {
    part: Option<Part>,
    whorl: DVec3,
    fullness: f64,
    crown: f64,
    back: f64,
}

/// The parting's arc: its plane's normal, and its two ends as angles in that plane.
#[derive(Clone, Copy, Debug)]
struct Part {
    side_deg: f64,
    turn: DQuat,
    normal: DVec3,
    front: f64,
    back: f64,
}

/// Where a point of the scalp is combed from: how far it lies from its source, radians over the head, and which strand
/// it is on ([`Field::comb`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Comb {
    /// Radians from the source (the parting or the whorl).
    pub distance: f64,
    /// Which strand: radians along the parting from its front, or round the whorl.
    pub across: f64,
    /// Whether the source is the parting (`false`: the whorl).
    pub from_part: bool,
}

impl Field {
    /// The field `volume` describes.
    pub fn new(volume: &HairVolume) -> Self {
        let part = volume.part.map(|p| {
            let turn = DQuat::from_rotation_z(-p.side_deg.to_radians());
            let (front, back) = (-p.front_deg.to_radians(), p.back_deg.to_radians());
            Part { side_deg: p.side_deg, turn, normal: turn * DVec3::X, front, back }
        });
        let (polar, azimuth) = (volume.whorl.polar_deg.to_radians(), volume.whorl.azimuth_deg.to_radians());
        let whorl = DVec3::new(polar.sin() * azimuth.sin(), polar.cos(), polar.sin() * azimuth.cos());
        Self { part, whorl, fullness: volume.fullness, crown: volume.crown, back: volume.back }
    }

    /// The direction `s` (`0..=1`) of the way along the parting from its front, or `None` without one.
    pub fn part(&self, s: f64) -> Option<DVec3> {
        self.part.map(|p| p.at(p.front + (p.back - p.front) * s.clamp(0.0, 1.0)))
    }

    /// Which way across the parting is the character's left (`+x` turned with it), or `None` without one: a lock rooted
    /// on the parting is nudged this way to its own side.
    pub fn part_normal(&self) -> Option<DVec3> {
        self.part.map(|p| p.normal)
    }

    /// How far the parting sits off the midline, degrees toward `+x`, or `None` without one.
    pub fn part_side_deg(&self) -> Option<f64> {
        self.part.map(|p| p.side_deg)
    }

    /// The whorl's direction.
    pub fn whorl(&self) -> DVec3 {
        self.whorl
    }

    /// Where `dir` is combed from: the nearer of the parting and the whorl.
    pub fn comb(&self, dir: DVec3) -> Comb {
        let whorl = self.whorl_comb(dir);
        match self.part {
            Some(part) => {
                let parted = part.comb(dir);
                if parted.distance <= whorl.distance { parted } else { whorl }
            }
            None => whorl,
        }
    }

    /// The fullness at `dir`: [`HairVolume::fullness`], times [`HairVolume::crown`] over the top and
    /// [`HairVolume::back`] over the back, blending smoothly between.
    pub fn fullness(&self, dir: DVec3) -> f64 {
        let top = smoothstep(dir.y, 0.5, 0.95);
        let behind = smoothstep(-dir.z, 0.2, 0.8) * (1.0 - top);
        self.fullness * (1.0 + (self.crown - 1.0) * top) * (1.0 + (self.back - 1.0) * behind)
    }

    /// [`Comb`] from the whorl alone: its distance, and its angle round the whorl from the direction toward the face.
    fn whorl_comb(&self, dir: DVec3) -> Comb {
        let w = self.whorl;
        let distance = w.dot(dir).clamp(-1.0, 1.0).acos();
        let toward = (DVec3::Z - w * w.z).try_normalize().unwrap_or(DVec3::X);
        let side = w.cross(toward);
        let flat = dir - w * w.dot(dir);
        Comb { distance, across: flat.dot(side).atan2(flat.dot(toward)), from_part: false }
    }
}

impl Part {
    /// The direction at angle `a` in the parting's plane: `0` the top, negative forward.
    fn at(&self, a: f64) -> DVec3 {
        self.turn * DVec3::new(0.0, a.cos(), -a.sin())
    }

    /// [`Comb`] from the parting: the distance to its arc, and how far along the arc the nearest point lies.
    fn comb(&self, dir: DVec3) -> Comb {
        let local = self.turn.inverse() * dir;
        let a = (-local.z).atan2(local.y);
        let (lo, hi) = (self.front.min(self.back), self.front.max(self.back));
        let at = a.clamp(lo, hi);
        let distance = if (lo..=hi).contains(&a) {
            local.x.clamp(-1.0, 1.0).asin().abs()
        } else {
            self.at(at).dot(dir).clamp(-1.0, 1.0).acos()
        };
        Comb { distance, across: at - self.front, from_part: true }
    }
}

/// Every lock and shell of `entries` stands off the head as `field`'s fullness says: each point's height over the head
/// surface is scaled by it, fully over the head and fading out below the ears ([`SWELL_FADE`]). Points on or inside the
/// head stay where they are.
pub fn swell(entries: &mut [CompiledEntry], field: &Field, head: &dyn HeadSurface) {
    let centre = head.centre();
    let moved = |p: Vec3| -> Vec3 {
        let offset = p - centre;
        let Some(dir) = offset.try_normalize() else { return p };
        let surface = head.radius(dir);
        let height = offset.length() - surface;
        if height <= 0.0 {
            return p;
        }
        let d = dir.as_dvec3();
        let fade = smoothstep(-d.y, -SWELL_FADE.0.cos(), -SWELL_FADE.1.cos());
        let factor = 1.0 + (field.fullness(d) - 1.0) * (1.0 - fade);
        #[allow(clippy::cast_possible_truncation)] // a factor near 1
        let height = height * factor as f32;
        centre + dir * (surface + height)
    };
    for entry in entries {
        for clump in &mut entry.group.clumps {
            swell_clump(clump, &moved);
        }
        if let GroupGeometry::Shell(shell) = &mut entry.group.geometry {
            swell_shell(shell, &moved);
        }
    }
}

fn swell_clump(clump: &mut HairClump, moved: &impl Fn(Vec3) -> Vec3) {
    for p in &mut clump.rest {
        *p = moved(*p);
    }
}

fn swell_shell(shell: &mut MeshData, moved: &impl Fn(Vec3) -> Vec3) {
    for p in &mut shell.positions {
        *p = moved(*p);
    }
}

/// 0 at or below `lo`, 1 at or above `hi`, an S-curve between.
fn smoothstep(x: f64, lo: f64, hi: f64) -> f64 {
    let u = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{HairParting, HairWhorl};

    use super::*;

    fn parted(side_deg: f64) -> Field {
        Field::new(&HairVolume {
            part: Some(HairParting { side_deg, front_deg: 60.0, back_deg: 30.0 }),
            ..HairVolume::default()
        })
    }

    #[test]
    fn the_parting_runs_from_front_to_back_on_its_side() {
        let centre = parted(0.0);
        let (front, back) = (centre.part(0.0).unwrap(), centre.part(1.0).unwrap());
        assert!(front.z > 0.8 && back.z < -0.4, "{front} to {back}");
        assert!(front.x.abs() < 1e-9 && back.x.abs() < 1e-9);
        let side = parted(20.0);
        assert!(side.part(0.5).unwrap().x > 0.3, "a side part sits toward +x: {}", side.part(0.5).unwrap());
    }

    #[test]
    fn a_point_on_the_parting_is_combed_from_it_and_one_off_it_lies_farther() {
        let field = parted(15.0);
        let on = field.part(0.4).unwrap();
        let comb = field.comb(on);
        assert!(comb.from_part && comb.distance < 1e-6, "{comb:?}");
        let off = (on + field.part_normal().unwrap() * 0.2).normalize();
        assert!(field.comb(off).distance > 0.15);
        // Further along the parting is further along `across`.
        assert!(field.comb(field.part(0.8).unwrap()).across > comb.across);
    }

    #[test]
    fn without_a_parting_the_hair_radiates_from_the_whorl() {
        let field = Field::new(&HairVolume {
            whorl: HairWhorl { polar_deg: 30.0, azimuth_deg: 180.0 },
            ..HairVolume::default()
        });
        let w = field.whorl();
        assert!(w.z < -0.4 && w.y > 0.8);
        assert!(field.comb(w).distance < 1e-6);
        let left = field.comb((w + DVec3::X * 0.3).normalize());
        let right = field.comb((w - DVec3::X * 0.3).normalize());
        assert!((left.distance - right.distance).abs() < 1e-9 && (left.across + right.across).abs() < 1e-9);
    }

    #[test]
    fn fullness_rises_at_the_crown_and_the_back() {
        let field = Field::new(&HairVolume { fullness: 1.2, crown: 1.5, back: 0.8, ..HairVolume::default() });
        assert!((field.fullness(DVec3::Y) - 1.8).abs() < 1e-9);
        assert!((field.fullness(DVec3::X) - 1.2).abs() < 1e-9);
        assert!((field.fullness(-DVec3::Z) - 0.96).abs() < 1e-9);
    }
}
