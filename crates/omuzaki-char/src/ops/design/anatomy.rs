//! The anatomy under the shape (#2614): `anatomy.list`, `anatomy.set`, `anatomy.randomise` and `anatomy.reset`.
//!
//! The anatomy is the bone, fat, muscle and skin under the head's and the body's surface, each structure a z-score
//! (standard deviations from the mean adult) with a spread in millimetres, moved by a few shared factors (adiposity,
//! robusticity, muscularity, laxity) and by the character's age. Setting a factor moves every structure it loads, so
//! one edit makes a heavier or a more gracile face that is still one face.

use omuzaki_char_api::CharId;
use omuzaki_char_api::design::{
    AnatomyDef, DesignDef, DialSetting, DialSide, Factor, STRUCTURE_RANGE, StructureDef, Tissue, design_library,
};
use omuzaki_core::rng::Rng;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dials::{SetSide, normal};
use super::{load, store};
use crate::ops::character::access;

/// Every structure in the library.
fn structures() -> Result<&'static [StructureDef], OpError> {
    design_library()
        .map(|l| l.structures.as_slice())
        .map_err(|e| OpError::internal(format!("the anatomy library doesn't load: {e}")))
}

/// The factor called `name`, if it is one.
fn factor_named(name: &str) -> Option<Factor> {
    Factor::ALL.into_iter().find(|f| f.as_str() == name)
}

/// The tissue called `name`, or `invalid` (field `field`).
fn tissue_named(name: &str, field: &str) -> Result<Tissue, OpError> {
    Tissue::ALL.into_iter().find(|t| t.as_str() == name).ok_or_else(|| {
        OpError::invalid(field, format!("there is no tissue `{name}`"))
            .hint(format!("the tissues are {}", access::list(Tissue::ALL.iter().map(|t| t.as_str()))))
    })
}

/// The structure `id`, or `not_found` (field `field`) with the nearest id.
fn structure(id: &str, field: &str) -> Result<&'static StructureDef, OpError> {
    let all = structures()?;
    all.iter().find(|s| s.id.as_str() == id).ok_or_else(|| {
        let names = all.iter().map(|s| s.id.as_str()).chain(Factor::ALL.iter().map(|f| f.as_str()));
        let hint = match omuzaki_core::text::nearest(id, names) {
            Some(near) => format!("did you mean `{near}`? anatomy.list lists the {} structures and the factors", all.len()),
            None => format!("anatomy.list lists the {} structures and the factors", all.len()),
        };
        access::not_found(field, format!("there is no structure or factor `{id}`")).hint(hint)
    })
}

/// A structure's entry as the list shows it, with its values in `design` when there is one.
fn listed(s: &StructureDef, design: Option<&DesignDef>) -> Value {
    let mut entry = json!({
        "id": s.id, "name": s.name, "summary": s.summary, "tissue": s.tissue.as_str(), "group": s.group,
        "sided": s.sided, "sd_mm": s.sd_mm,
        "factors": s.factors.iter().map(|(f, w)| (f.as_str().to_owned(), json!(w))).collect::<serde_json::Map<_, _>>(),
        "ages": !s.age.is_empty(), "descends": !s.descent.is_empty(),
    });
    if let (Some(design), Value::Object(fields)) = (design, &mut entry) {
        let at = |side| s.value(&design.anatomy, design.age, side);
        let value = if s.sided {
            json!({ "left": at(Some(DialSide::Left)), "right": at(Some(DialSide::Right)) })
        } else {
            json!(at(None))
        };
        fields.insert("value".to_owned(), value);
        if let Some(own) = design.anatomy.structures.get(&s.id) {
            fields.insert("setting".to_owned(), json!(own));
        }
    }
    entry
}

/// Lists the anatomy: the shared factors and every structure (bone, fat, muscle, skin) with its spread in millimetres,
/// its factor loadings and whether age moves it; with a character, each one's value there (factors, own setting and
/// age together, in standard deviations). Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {},
        {"tissue": "fat"},
        {"character": "kit", "group": "face"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct AnatomyList {
    /// Only this group's structures (`face`, `neck`, `torso`, `arms`, `legs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Only this tissue's: `bone`, `fat`, `muscle` or `skin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tissue: Option<String>,
    /// A character whose values are shown beside each structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
}

impl Op for AnatomyList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "anatomy.list",
            summary: "List the anatomy under the shape (factors, and bone, fat, muscle and skin structures), with a character's values.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let design = match &self.character {
            Some(id) => Some(load(ctx, id)?.1),
            None => None,
        };
        let tissue = self.tissue.as_deref().map(|t| tissue_named(t, "tissue")).transpose()?;
        let all = structures()?;
        if let Some(group) = &self.group
            && !all.iter().any(|s| &s.group == group)
        {
            let mut groups: Vec<&str> = all.iter().map(|s| s.group.as_str()).collect();
            groups.sort_unstable();
            groups.dedup();
            return Err(access::not_found("group", format!("there is no anatomy group `{group}`"))
                .hint(format!("the groups are {}", access::list(groups))));
        }
        let list: Vec<Value> = all
            .iter()
            .filter(|s| self.group.as_ref().is_none_or(|g| &s.group == g) && tissue.is_none_or(|t| s.tissue == t))
            .map(|s| listed(s, design.as_ref()))
            .collect();
        let factors: Vec<Value> = Factor::ALL
            .iter()
            .map(|f| {
                let mut entry = json!({ "id": f.as_str(), "summary": f.summary() });
                if let (Some(d), Value::Object(fields)) = (&design, &mut entry) {
                    fields.insert("value".to_owned(), json!(d.anatomy.factor(*f)));
                }
                entry
            })
            .collect();
        let mut out = json!({ "factors": factors, "count": list.len(), "structures": list });
        if let (Some(design), Value::Object(fields)) = (&design, &mut out) {
            fields.insert("age".to_owned(), json!(design.age));
        }
        Ok(Plan::answer(out))
    }
}

register_op!(AnatomyList);

/// Sets one shared factor or one structure of a character's anatomy, in standard deviations from the mean adult
/// (`-3` to `3`). A factor (`adiposity`, `robusticity`, `muscularity`, `laxity`) moves every structure it loads, so
/// `adiposity: 1` fills the cheeks, jowls, neck, belly and hips together; a structure (`malar_fat`, `mandible`,
/// `nasolabial_fold`) moves only itself, on both sides or, for a sided one, one. `0` is the mean; setting every side to
/// `0` removes the entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "name": "adiposity", "value": 1.2},
        {"character": "kit", "name": "mandible", "value": 0.8},
        {"character": "kit", "name": "malar_fat", "value": -0.5, "side": "left"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct AnatomySet {
    /// The character.
    pub character: CharId,
    /// A factor or a structure (`anatomy.list` lists both).
    pub name: String,
    /// The value, `-3` to `3` standard deviations.
    #[schemars(range(min = -3.0, max = 3.0))]
    pub value: f64,
    /// Which side (sided structures only): `both` (the default), `left` or `right`.
    #[serde(default, skip_serializing_if = "is_both")]
    pub side: SetSide,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_both(side: &SetSide) -> bool {
    *side == SetSide::Both
}

impl Op for AnatomySet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "anatomy.set",
            summary: "Set a factor (adiposity, robusticity, muscularity, laxity) or a structure (a fat pad, a bone, a muscle, a fold) of a character's anatomy.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        let (lo, hi) = STRUCTURE_RANGE;
        if !(self.value.is_finite() && (lo..=hi).contains(&self.value)) {
            return Err(OpError::invalid("value", format!("{} is outside {lo} to {hi} standard deviations", self.value))
                .hint("0 is the mean adult; ±1 is one standard deviation"));
        }
        if let Some(factor) = factor_named(&self.name) {
            if self.side != SetSide::Both {
                return Err(OpError::invalid("side", format!("`{}` is a factor of the whole body", self.name))
                    .hint("leave `side` out"));
            }
            if self.value == 0.0 {
                design.anatomy.factors.remove(&factor);
            } else {
                design.anatomy.factors.insert(factor, self.value);
            }
            let changes = store(ctx, &self.character, &design)?;
            let moved: Vec<&str> =
                structures()?.iter().filter(|s| s.factors.contains_key(&factor)).map(|s| s.id.as_str()).collect();
            let summary = json!({
                "character": self.character, "factor": factor.as_str(), "value": self.value, "moves": moved,
                "changed": !changes.is_empty(),
            });
            return Ok(access::edit_plan(changes, summary));
        }
        let s = structure(&self.name, "name")?;
        if self.side != SetSide::Both && !s.sided {
            return Err(OpError::invalid("side", format!("`{}` is one structure for both sides", s.id))
                .hint("leave `side` out (or set it to `both`)"));
        }
        let mut setting = design.anatomy.structures.get(&s.id).copied().unwrap_or_default();
        match self.side {
            SetSide::Both => setting = DialSetting::new(self.value),
            SetSide::Left => setting.left = self.value - setting.value,
            SetSide::Right => setting.right = self.value - setting.value,
        }
        if setting.is_zero() {
            design.anatomy.structures.remove(&s.id);
        } else {
            design.anatomy.structures.insert(s.id.clone(), setting);
        }
        let changes = store(ctx, &self.character, &design)?;
        let summary = json!({
            "character": self.character, "structure": s.id, "tissue": s.tissue.as_str(), "setting": setting,
            "changed": !changes.is_empty(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(AnatomySet);

/// How large a side's own offset is at `asymmetry: 1`, as a fraction of a structure's residual.
const SIDE_SPREAD: f64 = 0.4;

/// Samples a new anatomy into `anatomy` from `seed`: the factors at `amount` standard deviations, then each
/// structure's residual around them, with a per-side offset at `asymmetry`. Coherent by construction: the factors carry
/// most of the variation, so a sample is heavier or leaner, gracile or robust, as a whole.
pub(crate) fn sample(anatomy: &mut AnatomyDef, seed: u64, amount: f64, asymmetry: f64) -> Result<usize, OpError> {
    let (lo, hi) = STRUCTURE_RANGE;
    // One stream per name, seeded by the seed and the name alone, so adding a structure to the library doesn't
    // reshuffle the rest.
    let stream = |name: &str| {
        let key = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        Rng::new(seed ^ key.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    };
    anatomy.factors.clear();
    for f in Factor::ALL {
        let v = (normal(&mut stream(f.as_str())) * amount).clamp(lo, hi);
        if v != 0.0 {
            anatomy.factors.insert(f, v);
        }
    }
    anatomy.structures.clear();
    let all = structures()?;
    for s in all {
        let mut r = stream(s.id.as_str());
        let value = (normal(&mut r) * s.residual * amount).clamp(lo, hi);
        let mut setting = DialSetting::new(value);
        if s.sided && asymmetry > 0.0 {
            let spread = s.residual * SIDE_SPREAD * asymmetry * amount.max(0.5);
            setting.left = (normal(&mut r) * spread).clamp(lo - value, hi - value);
            setting.right = (normal(&mut r) * spread).clamp(lo - value, hi - value);
        }
        if !setting.is_zero() {
            anatomy.structures.insert(s.id.clone(), setting);
        }
    }
    Ok(all.len())
}

/// Samples a plausible anatomy from a seed: the shared factors first (how heavy, how robust, how muscular, how loose
/// the skin), then each structure's own small variation around them, so a random face stays one coherent face. `amount`
/// scales the spread (`1`: the adult population's; `0.5`: subtler); `asymmetry` gives each side of the sided
/// structures its own small offset. The same seed gives the same anatomy; the age and the dials stay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "seed": 7},
        {"character": "kit", "seed": 3, "amount": 0.6, "asymmetry": 0.4}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct AnatomyRandomise {
    /// The character.
    pub character: CharId,
    /// The seed: the same seed gives the same anatomy.
    pub seed: u64,
    /// How far from the mean: `0` to `2`, `1` (the default) the population's spread.
    #[serde(default = "one")]
    #[schemars(range(min = 0.0, max = 2.0))]
    pub amount: f64,
    /// How different the two sides are: `0` (the default, symmetric) to `1`.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub asymmetry: f64,
}

fn one() -> f64 {
    1.0
}

impl Op for AnatomyRandomise {
    fn meta() -> OpMeta {
        OpMeta {
            name: "anatomy.randomise",
            summary: "Sample a plausible, coherent anatomy from a seed: the shared factors, then each structure around them.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        if !(0.0..=2.0).contains(&self.amount) {
            return Err(OpError::invalid("amount", format!("{} is outside 0 to 2", self.amount)).hint("1 is the population's spread"));
        }
        if !(0.0..=1.0).contains(&self.asymmetry) {
            return Err(OpError::invalid("asymmetry", format!("{} is outside 0 to 1", self.asymmetry)).hint("0 is symmetric"));
        }
        let count = sample(&mut design.anatomy, self.seed, self.amount, self.asymmetry)?;
        let changes = store(ctx, &self.character, &design)?;
        let factors: serde_json::Map<String, Value> =
            design.anatomy.factors.iter().map(|(f, v)| (f.as_str().to_owned(), json!(v))).collect();
        let summary = json!({ "character": self.character, "seed": self.seed, "factors": factors, "structures": count });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(AnatomyRandomise);

/// Puts the anatomy back to the mean adult: everything, the factors only, or one structure or factor. The age, the
/// dials and the sculpt strokes stay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "name": "adiposity"},
        {"character": "kit", "name": "malar_fat"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct AnatomyReset {
    /// The character.
    pub character: CharId,
    /// Only this factor or structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Op for AnatomyReset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "anatomy.reset",
            summary: "Reset a character's anatomy to the mean adult: all of it, or one factor or structure.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        let before = design.anatomy.factors.len() + design.anatomy.structures.len();
        match self.name.as_deref() {
            None => design.anatomy = AnatomyDef::default(),
            Some(name) => match factor_named(name) {
                Some(f) => {
                    design.anatomy.factors.remove(&f);
                }
                None => {
                    let s = structure(name, "name")?;
                    design.anatomy.structures.remove(&s.id);
                }
            },
        }
        let after = design.anatomy.factors.len() + design.anatomy.structures.len();
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "reset": before - after })))
    }
}

register_op!(AnatomyReset);
