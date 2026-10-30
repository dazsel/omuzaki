//! Anatomy under the shape (#2614): the head and body as bone, fat, muscle and skin, so every change stays plausible.
//!
//! Shape dials move the surface directly, which is what a person reaching for "wider jaw" wants, but dials combined at
//! random (or blended) make faces no skull could carry. The anatomy layer is the model underneath: a set of
//! **structures**, each one piece of tissue (the zygomatic arch, the malar fat pad, the masseter, the skin over the
//! cheek), with
//! - a **spread**: one standard deviation of that tissue's thickness across adults, in millimetres (`sd_mm`), taken
//!   from published soft-tissue depth and facial-ageing studies as design targets;
//! - **loadings** on a few shared [`Factor`]s (adiposity, robusticity, muscularity, laxity), so one number moves every
//!   fat pad together, or every bony landmark, as bodies do: a heavier face is heavier everywhere, not in one pad;
//! - an **age course** (`age`, in standard deviations) and, for the soft tissue that sags, a **descent** (`descent`,
//!   how far the tissue's centre has moved down the face): the malar fat deflates and slides toward the jowl, the
//!   orbit widens and the maxilla recedes as bone resorbs, the folds deepen as the skin thins.
//!
//! A structure's value is a z-score: the sum of its factors' values times its loadings, its own setting and its age
//! course, clamped to [`STRUCTURE_RANGE`]. The surface is solved as the kit's head (the adult mean anatomy) moved by
//! every structure's change of thickness at its place: bone moves the surface under the soft tissue and the soft tissue
//! rides on it, so the two add. Each structure's effects are written in the dials' frames ([`DialEffect`]): a landmark
//! of the face, or a fraction along a body region, so one anatomy fits every template, profile and stature. Thickness
//! is in millimetres of an adult head (`HEAD_REFERENCE_MM` from the head's centre to its surface) and an adult limb's
//! girth, so a stylised head keeps its tissue in proportion, as an artist draws it.
//!
//! The age of the whole character runs through here where a structure carries it ([`StructureDef::age`]), so ageing
//! is coherent across the face: dials keep their own age curves only where no structure stands under them.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::dial::{DialEffect, DialSetting, DialSide};
use super::frame::RegionGroup;
use crate::ids::DialId;

/// The z-scores a structure or a factor may take: three standard deviations either side of the mean adult.
pub const STRUCTURE_RANGE: (f64, f64) = (-3.0, 3.0);

/// The distance from an adult head's centre to its surface that head thicknesses are a fraction of, millimetres.
pub const HEAD_REFERENCE_MM: f32 = 95.0;

/// What a structure is made of.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tissue {
    /// The skull and skeleton: the brow ridge, the zygoma, the orbit's rim, the maxilla, the mandible.
    Bone,
    /// A fat pad or subcutaneous fat: malar, buccal, jowl, temporal, submental; the belly, the hips.
    Fat,
    /// A muscle's bulk: the masseter, the temporalis; the deltoid, the calf.
    Muscle,
    /// The skin and what it does with age: its thickness, and the folds it makes as it thins.
    Skin,
}

impl Tissue {
    /// Every tissue, in order.
    pub const ALL: [Self; 4] = [Self::Bone, Self::Fat, Self::Muscle, Self::Skin];

    /// Its name, as the files and the ops spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bone => "bone",
            Self::Fat => "fat",
            Self::Muscle => "muscle",
            Self::Skin => "skin",
        }
    }
}

/// A shared cause that moves many structures together, as one trait of a body does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Factor {
    /// Body fat: every fat pad and the subcutaneous fat of the body.
    Adiposity,
    /// The skeleton's build: a heavier brow ridge, jaw, chin and cheekbones, wider wrists and ankles.
    Robusticity,
    /// Muscle bulk: the jaw's and the temples' muscles, the shoulders, arms and legs.
    Muscularity,
    /// How loose the skin is: deeper folds and more descent (sun, smoking and genes age a face early).
    Laxity,
}

impl Factor {
    /// Every factor, in order.
    pub const ALL: [Self; 4] = [Self::Adiposity, Self::Robusticity, Self::Muscularity, Self::Laxity];

    /// Its name, as the files and the ops spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adiposity => "adiposity",
            Self::Robusticity => "robusticity",
            Self::Muscularity => "muscularity",
            Self::Laxity => "laxity",
        }
    }

    /// What `+1` does, for a picker.
    pub fn summary(self) -> &'static str {
        match self {
            Self::Adiposity => "+1 more body fat everywhere (fuller cheeks, jowls, neck, belly, hips); -1 lean",
            Self::Robusticity => "+1 a heavier skeleton (brow ridge, jaw, chin, cheekbones, wrists); -1 gracile",
            Self::Muscularity => "+1 more muscle (jaw, temples, shoulders, arms, legs); -1 slight",
            Self::Laxity => "+1 looser skin: deeper folds and more sag at any age; -1 firm",
        }
    }
}

/// `characters/<id>/design/anatomy`: the character's factors and its own structure settings, as z-scores. The empty
/// value is the kit's mean adult.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct AnatomyDef {
    /// The shared factors' values, within [`STRUCTURE_RANGE`]. A factor not listed is at 0.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub factors: BTreeMap<Factor, f64>,
    /// Each structure's own value on top of its factors, by structure id, with a per-side offset on sided ones.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub structures: BTreeMap<DialId, DialSetting>,
}

impl AnatomyDef {
    /// True when it sets nothing.
    pub fn is_empty(&self) -> bool {
        self.factors.values().all(|v| *v == 0.0) && self.structures.values().all(DialSetting::is_zero)
    }

    /// The value of `factor` (0 if unset).
    pub fn factor(&self, factor: Factor) -> f64 {
        self.factors.get(&factor).copied().unwrap_or(0.0)
    }

    /// Checks every factor's range and every structure's id against `structures` and its values against its sides.
    ///
    /// # Errors
    /// A path under `anatomy` and what is wrong (`structures/malar_fat/left: ...`).
    pub fn check(&self, structures: &[StructureDef]) -> Result<(), String> {
        let (lo, hi) = STRUCTURE_RANGE;
        for (factor, v) in &self.factors {
            if !(v.is_finite() && (lo..=hi).contains(v)) {
                return Err(format!("factors/{}: {v} is outside {lo} to {hi}", factor.as_str()));
            }
        }
        for (id, setting) in &self.structures {
            let def = structures
                .iter()
                .find(|s| &s.id == id)
                .ok_or_else(|| format!("structures/{id}: there is no structure `{id}` (`anatomy.list` lists them)"))?;
            setting.check_in(STRUCTURE_RANGE, def.sided, id.as_str()).map_err(|e| format!("structures/{id}/{e}"))?;
        }
        Ok(())
    }
}

/// One structure, as the library holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StructureDef {
    /// The structure's id.
    pub id: DialId,
    /// What a picker shows.
    pub name: String,
    /// One line: what it is, and what `+1` does.
    pub summary: String,
    /// What it is made of.
    pub tissue: Tissue,
    /// Where a picker files it: `face`, `head`, `neck`, `torso`, `arms`, `legs`.
    pub group: String,
    /// Whether it has a left and a right side.
    #[serde(default)]
    pub sided: bool,
    /// One standard deviation of its thickness across adults, millimetres: how far `+1` moves the surface at its
    /// centre.
    pub sd_mm: f32,
    /// How strongly each shared factor moves it, in standard deviations per unit of the factor.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub factors: BTreeMap<Factor, f64>,
    /// The spread of its own value around what its factors give it, for sampling (a fraction of one sd).
    #[serde(default = "default_residual")]
    pub residual: f64,
    /// Its value by age, `[age, z]` points in age order (linear between, constant past the ends); empty: age doesn't
    /// change it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub age: Vec<[f64; 2]>,
    /// How far its centre has moved down the face by age, `[age, fraction]` points in the head's own proportions (head
    /// effects only); empty: it stays where it is. Scaled by `1 + 0.5 · laxity`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub descent: Vec<[f64; 2]>,
    /// The mean thickness of the soft tissue that descends, millimetres: as it moves down, that much is taken from
    /// where it was and added where it went (a malar fat pad leaves a hollow under the eye and fills the jowl). 0: its
    /// descent only carries its own change of thickness.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth_mm: f32,
    /// Where it is and how it is shaped: each effect's `amount` is a weight, `1` the full spread at its centre.
    pub effects: Vec<DialEffect>,
}

fn default_residual() -> f64 {
    0.5
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// The adult girth a body effect's millimetres are a fraction of, by region group: the radius of the section.
fn body_reference_mm(group: RegionGroup) -> f32 {
    match group {
        RegionGroup::Torso => 130.0,
        RegionGroup::Neck => 55.0,
        RegionGroup::Head => HEAD_REFERENCE_MM,
        RegionGroup::ArmLeft | RegionGroup::ArmRight => 45.0,
        RegionGroup::HandLeft | RegionGroup::HandRight => 20.0,
        RegionGroup::LegLeft | RegionGroup::LegRight => 65.0,
        RegionGroup::FootLeft | RegionGroup::FootRight => 35.0,
    }
}

impl StructureDef {
    /// Its age course at `age` years, in standard deviations (0 if it has none).
    pub fn at_age(&self, age: f64) -> f64 {
        super::age::curve_at(&self.age, age)
    }

    /// How far its centre has moved down the face at `age` with `laxity`, in the head's proportions.
    pub fn descent_at(&self, age: f64, laxity: f64) -> f64 {
        super::age::curve_at(&self.descent, age) * (1.0 + 0.5 * laxity).max(0.0)
    }

    /// Its value on `side` (`None` for an unsided structure), a z-score: its factors, its own setting and its age
    /// course, clamped to [`STRUCTURE_RANGE`].
    pub fn value(&self, anatomy: &AnatomyDef, age: Option<f64>, side: Option<DialSide>) -> f64 {
        let from_factors: f64 = self.factors.iter().map(|(f, w)| w * anatomy.factor(*f)).sum();
        let own = anatomy.structures.get(&self.id).map_or(0.0, |s| s.on(side));
        let aged = age.map_or(0.0, |a| self.at_age(a));
        (from_factors + own + aged).clamp(STRUCTURE_RANGE.0, STRUCTURE_RANGE.1)
    }

    /// Its effects as dial effects at one standard deviation, with `descent` added to where the head effects aim:
    /// each `amount` turned from a weight into the fraction of the frame's own length that its millimetres are.
    pub fn effects_at(&self, descent: f64) -> Vec<DialEffect> {
        self.effects_with(self.sd_mm, descent)
    }

    /// The descent of its mean tissue at `descent`, as `(effect, value)` pairs: `-1` where the tissue was, `+1` where
    /// it went. Empty when nothing descends.
    pub fn sag_at(&self, descent: f64) -> Vec<(DialEffect, f64)> {
        if descent <= 0.0 || self.depth_mm <= 0.0 {
            return Vec::new();
        }
        let from = self.effects_with(self.depth_mm, 0.0).into_iter().map(|e| (e, -1.0));
        let to = self.effects_with(self.depth_mm, descent).into_iter().map(|e| (e, 1.0));
        from.chain(to).filter(|(e, _)| matches!(e, DialEffect::Head { .. })).collect()
    }

    /// Its effects with `mm` at weight 1 and `descent` added to the head effects' aim.
    #[allow(clippy::cast_possible_truncation)] // a descent is a small fraction
    fn effects_with(&self, mm: f32, descent: f64) -> Vec<DialEffect> {
        self.effects
            .iter()
            .map(|effect| {
                let mut e = effect.clone();
                match &mut e {
                    DialEffect::Body { region, amount, amount_down, .. } => {
                        let k = mm / body_reference_mm(*region);
                        *amount *= k;
                        if let Some(d) = amount_down {
                            *d *= k;
                        }
                    }
                    DialEffect::Head { nudge, amount, amount_down, .. } => {
                        let k = mm / HEAD_REFERENCE_MM;
                        *amount *= k;
                        if let Some(d) = amount_down {
                            *d *= k;
                        }
                        if descent != 0.0 {
                            let n = nudge.unwrap_or([0.0; 3]);
                            *nudge = Some([n[0], n[1] - descent as f32, n[2]]);
                        }
                    }
                }
                e
            })
            .collect()
    }

    /// Checks the definition: a positive spread, curves in age order, sane loadings, and effects a dial could have.
    ///
    /// # Errors
    /// What is wrong, naming the structure.
    pub fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if !(self.sd_mm > 0.0 && self.sd_mm.is_finite() && self.sd_mm < 60.0) {
            return Err(format!("structure `{id}`: sd_mm must be above 0 and below 60"));
        }
        if !(self.depth_mm >= 0.0 && self.depth_mm < 60.0) {
            return Err(format!("structure `{id}`: depth_mm must be 0 to 60"));
        }
        if !(self.residual >= 0.0 && self.residual.is_finite()) {
            return Err(format!("structure `{id}`: its residual must not be negative"));
        }
        if self.factors.values().any(|w| !w.is_finite() || w.abs() > 2.0) {
            return Err(format!("structure `{id}`: a factor loading must be within ±2"));
        }
        for (name, curve) in [("age", &self.age), ("descent", &self.descent)] {
            if curve.windows(2).any(|w| w[1][0] <= w[0][0]) {
                return Err(format!("structure `{id}`: its {name} curve must run in age order"));
            }
        }
        if self.descent.iter().any(|p| !(0.0..=0.5).contains(&p[1])) {
            return Err(format!("structure `{id}`: a descent is 0 to 0.5 of the head's proportions"));
        }
        // The effects follow a dial's rules.
        let as_dial = super::dial::DialDef {
            id: self.id.clone(),
            name: self.name.clone(),
            summary: self.summary.clone(),
            group: self.group.clone(),
            sided: self.sided,
            range: STRUCTURE_RANGE,
            prior: 1.0,
            age: Vec::new(),
            effects: self.effects.clone(),
        };
        as_dial.check().map_err(|e| e.replacen("dial", "structure", 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::dial::Push;
    use crate::design::frame::Landmark;

    fn malar() -> StructureDef {
        StructureDef {
            id: DialId::new("malar_fat").unwrap(),
            name: "Malar fat pad".into(),
            summary: "s".into(),
            tissue: Tissue::Fat,
            group: "face".into(),
            sided: true,
            sd_mm: 3.8,
            factors: [(Factor::Adiposity, 0.8)].into_iter().collect(),
            residual: 0.5,
            age: vec![[3.0, 1.2], [25.0, 0.0], [80.0, -1.0]],
            descent: vec![[35.0, 0.0], [80.0, 0.12]],
            depth_mm: 6.0,
            effects: vec![DialEffect::Head {
                landmark: Some(Landmark::CheekLeft),
                dir: None,
                nudge: None,
                reach: 25.0,
                push: Push::Radial,
                amount: 1.0,
                amount_down: None,
            }],
        }
    }

    #[test]
    fn a_value_adds_its_factors_its_own_setting_and_its_age_and_is_clamped() {
        let s = malar();
        let mut anatomy = AnatomyDef::default();
        anatomy.factors.insert(Factor::Adiposity, 1.0);
        anatomy.structures.insert(s.id.clone(), DialSetting { value: 0.5, left: 0.25, right: 0.0 });
        let left = s.value(&anatomy, Some(25.0), Some(DialSide::Left));
        assert!((left - (0.8 + 0.75)).abs() < 1e-9, "{left}");
        let right = s.value(&anatomy, Some(80.0), Some(DialSide::Right));
        assert!((right - (0.8 + 0.5 - 1.0)).abs() < 1e-9, "{right}");
        anatomy.factors.insert(Factor::Adiposity, 3.0);
        anatomy.structures.insert(s.id.clone(), DialSetting::new(3.0));
        assert!((s.value(&anatomy, None, None) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn millimetres_become_the_frames_fraction_and_descent_moves_the_aim_down() {
        let s = malar();
        let effects = s.effects_at(0.1);
        let DialEffect::Head { amount, nudge, .. } = &effects[0] else { panic!("a head effect") };
        assert!((amount - 3.8 / HEAD_REFERENCE_MM).abs() < 1e-6);
        assert!((nudge.unwrap()[1] + 0.1).abs() < 1e-6);
        assert!((s.descent_at(80.0, 0.0) - 0.12).abs() < 1e-9);
        assert!(s.descent_at(80.0, 1.0) > s.descent_at(80.0, 0.0), "laxity deepens the descent");
        let sag = s.sag_at(0.1);
        assert_eq!(
            sag.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
            [-1.0, 1.0],
            "taken from where it was, added below"
        );
        assert_eq!(s.sag_at(0.0), Vec::new(), "nothing descends at no descent");
        assert!(s.check().is_ok());
    }

    #[test]
    fn a_definition_out_of_bounds_is_refused() {
        let mut s = malar();
        s.sd_mm = 0.0;
        assert!(s.check().unwrap_err().contains("sd_mm"));
        let mut s = malar();
        s.descent = vec![[30.0, 0.0], [80.0, 0.9]];
        assert!(s.check().unwrap_err().contains("descent"));
        let mut s = malar();
        s.factors.insert(Factor::Laxity, 5.0);
        assert!(s.check().unwrap_err().contains("loading"));
    }

    #[test]
    fn a_setting_is_checked_against_the_structures_and_the_range() {
        let s = malar();
        let mut anatomy = AnatomyDef::default();
        anatomy.factors.insert(Factor::Robusticity, 4.0);
        assert!(anatomy.check(std::slice::from_ref(&s)).unwrap_err().starts_with("factors/robusticity"));
        let mut anatomy = AnatomyDef::default();
        anatomy.structures.insert(DialId::new("malr_fat").unwrap(), DialSetting::new(1.0));
        assert!(anatomy.check(std::slice::from_ref(&s)).unwrap_err().contains("no structure"));
        let mut anatomy = AnatomyDef::default();
        anatomy.structures.insert(s.id.clone(), DialSetting { value: 2.5, left: 1.0, right: 0.0 });
        assert!(anatomy.check(std::slice::from_ref(&s)).unwrap_err().starts_with("structures/malar_fat/left"));
        assert!(AnatomyDef::default().is_empty());
    }
}
