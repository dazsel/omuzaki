//! Finding a hair part's bangs group, checking a `BangsGen`, and writing it back, shared by the `hair.bangs.*` ops.
//!
//! The ops read the character's definition only to find the part, and this one group's generator; no other group of
//! the part is rewritten.

use omuzaki_char_api::{BangsGen, CharId, GroupId, Generator, HairParams, PartCategory, PartId, paths};
use omuzaki_doc_api::{Change, DocPath, Value};
use omuzaki_ops_api::{OpCtx, OpError};

use crate::ops::character::access as character;
use crate::ops::character::access::{list, not_found};
use crate::ops::part::describe::{at, kind_of, nearest};

/// Most layers a fringe may have.
pub const MAX_LAYERS: usize = 6;
/// Most clumps a layer may have (the generator's own limit).
pub const MAX_CLUMPS: u8 = 30;
/// Most tip heights a layer may list: one per clump.
pub const MAX_TIP_HEIGHTS: usize = MAX_CLUMPS as usize;
/// Most wisps a fringe may have.
pub const MAX_WISPS: usize = 8;
/// Tip heights and wisp heights are head-local, within this distance of the brow line.
pub const HEIGHT_LIMIT: f64 = 0.5;
/// The fringe's sweep, either way, in degrees.
pub const SWEEP_LIMIT_DEG: f64 = 60.0;
/// The tip hook, either way, in degrees.
pub const HOOK_LIMIT_DEG: f64 = 45.0;
/// Range of a layer's fan half-angle, degrees (the generator's own).
pub const SPREAD_DEG: (f64, f64) = (5.0, 90.0);
/// Range of a layer's shade (the generator's own).
pub const SHADE: (f64, f64) = (0.5, 1.0);
/// Range of a clump's width.
pub const WIDTH: (f64, f64) = (0.002, 0.2);
/// Range of a layer's root and tip distances from the skull.
pub const OFFSET: (f64, f64) = (0.0, 0.2);
/// Range of a wisp's width.
pub const WISP_WIDTH: (f64, f64) = (0.002, 0.1);
/// A wisp's angle from the front, either way, in degrees.
pub const WISP_PHI_LIMIT_DEG: f64 = 90.0;

/// A bangs group as stored: where it is, the whole part's normalised parameters, and the fringe.
pub(crate) struct BangsGroup {
    /// Where the part's parameters are.
    params_path: DocPath,
    /// The kind's `check`, to validate what is written.
    kind: &'static omuzaki_char_api::PartKindRegistration,
    /// The part's parameters as the kind normalises them.
    hair: HairParams,
    /// The group.
    pub(crate) group: GroupId,
    /// The group's fringe.
    pub(crate) bangs: BangsGen,
}

/// The bangs group `group` of the hair part `part` of `character`.
///
/// # Errors
/// `not_found` on `character`, `part` or `group` (naming what exists); `invalid` on `part` for a part that is not
/// hair or whose stored parameters the kind rejects; `invalid` on `group` for a group that is not a bangs group.
pub(crate) fn load(ctx: &OpCtx<'_>, id: &CharId, part: &PartId, group: &GroupId) -> Result<BangsGroup, OpError> {
    let def = character::load(ctx, id)?;
    let stored = character::part(&def, id, part, "part")?;
    let kind = kind_of(part, stored)?;
    if (kind.meta)().category != PartCategory::Hair {
        return Err(OpError::invalid("part", format!("part `{part}` is a `{}` part, not hair", stored.kind))
            .hint(format!("hair parts of `{id}`: {}", list(hair_parts(&def)))));
    }
    let normalised = (kind.check)(&stored.params).map_err(|e| {
        OpError::invalid("part", format!("the hair `{part}` is stored with parameters its kind rejects: {e}"))
            .hint("repair the part with part.set_param, then retry")
    })?;
    let hair: HairParams = serde_json::from_value(normalised)
        .map_err(|e| OpError::invalid("part", format!("the hair `{part}` doesn't hold hair parameters: {e}")))?;
    let Some(found) = hair.groups.get(group) else {
        let names = bangs_groups(&hair);
        return Err(not_found("group", format!("hair `{part}` has no group `{group}`"))
            .hint(format!("the bangs groups of `{part}` are: {}", list(names))));
    };
    let Generator::Bangs(bangs) = &found.generator else {
        return Err(OpError::invalid("group", format!("group `{group}` of `{part}` is not a bangs group"))
            .hint(format!("the bangs groups of `{part}` are: {}", list(bangs_groups(&hair)))));
    };
    let bangs = bangs.clone();
    Ok(BangsGroup { params_path: paths::part_params(id, part), kind, hair, group: group.clone(), bangs })
}

/// The ids of the character's hair parts, for hints.
fn hair_parts(def: &omuzaki_char_api::CharacterDef) -> Vec<String> {
    def.parts
        .iter()
        .filter(|(_, p)| omuzaki_char_api::find_kind(p.kind.as_str()).is_some_and(|k| (k.meta)().category == PartCategory::Hair))
        .map(|(id, _)| id.to_string())
        .collect()
}

/// The ids of the groups that hold bangs.
fn bangs_groups(hair: &HairParams) -> Vec<String> {
    hair.groups
        .iter()
        .filter(|(_, g)| matches!(g.generator, Generator::Bangs(_)))
        .map(|(id, _)| id.to_string())
        .collect()
}

/// A value of a `BangsGen` that is out of range: where it is, and why.
pub type Fault = (String, String);

/// Checks every number of `bangs` for finiteness and range, and its lists for length. The first fault found is
/// returned, with the path of the value inside the `BangsGen` (`layers/1/hook_deg`).
///
/// # Errors
/// The first value that is not finite, out of range or the wrong length.
pub fn check(bangs: &BangsGen) -> Result<(), Fault> {
    if !(1..=MAX_LAYERS).contains(&bangs.layers.len()) {
        return Err(("layers".into(), format!("{} layers is out of range: use 1 to {MAX_LAYERS}", bangs.layers.len())));
    }
    for (i, layer) in bangs.layers.iter().enumerate() {
        let at = |name: &str| format!("layers/{i}/{name}");
        if !(1..=MAX_CLUMPS).contains(&layer.count) {
            return Err((at("clumps"), format!("{} clumps is out of range: use 1 to {MAX_CLUMPS}", layer.count)));
        }
        if !(1..=MAX_TIP_HEIGHTS).contains(&layer.tip_heights.len()) {
            let n = layer.tip_heights.len();
            return Err((at("tip_heights"), format!("{n} heights is out of range: use 1 to {MAX_TIP_HEIGHTS}")));
        }
        for (k, height) in layer.tip_heights.iter().enumerate() {
            within(&format!("{}/{k}", at("tip_heights")), *height, -HEIGHT_LIMIT, HEIGHT_LIMIT)?;
        }
        within(&at("spread_deg"), layer.spread_deg, SPREAD_DEG.0, SPREAD_DEG.1)?;
        within(&at("width"), layer.width, WIDTH.0, WIDTH.1)?;
        within(&at("hook_deg"), layer.hook_deg, -HOOK_LIMIT_DEG, HOOK_LIMIT_DEG)?;
        within(&at("root_offset"), layer.root_offset, OFFSET.0, OFFSET.1)?;
        within(&at("tip_offset"), layer.tip_offset, OFFSET.0, OFFSET.1)?;
        within(&at("shade"), layer.shade, SHADE.0, SHADE.1)?;
    }
    if bangs.wisps.len() > MAX_WISPS {
        return Err(("wisps".into(), format!("{} wisps is out of range: use 0 to {MAX_WISPS}", bangs.wisps.len())));
    }
    for (i, wisp) in bangs.wisps.iter().enumerate() {
        within(&format!("wisps/{i}/phi_deg"), wisp.phi_deg, -WISP_PHI_LIMIT_DEG, WISP_PHI_LIMIT_DEG)?;
        within(&format!("wisps/{i}/tip_height"), wisp.tip_height, -HEIGHT_LIMIT, HEIGHT_LIMIT)?;
        within(&format!("wisps/{i}/width"), wisp.width, WISP_WIDTH.0, WISP_WIDTH.1)?;
    }
    within("sweep_deg", bangs.sweep_deg, -SWEEP_LIMIT_DEG, SWEEP_LIMIT_DEG)
}

/// `Ok` when `value` is finite and within `lo..=hi`.
fn within(path: &str, value: f64, lo: f64, hi: f64) -> Result<(), Fault> {
    if value.is_finite() && (lo..=hi).contains(&value) {
        Ok(())
    } else {
        Err((path.to_owned(), format!("{value} is out of range: use {lo} to {hi}")))
    }
}

/// The refusal for a fault of a value the caller gave: `invalid` on `field`, naming where in the fringe it is.
pub(crate) fn refusal(field: &str, (path, message): &Fault) -> OpError {
    OpError::invalid(field, format!("`{path}`: {message}"))
        .hint("hair.bangs.list_presets shows fringes whose values are all in range")
}

/// The changes that store `after` as the group's generator: one change, none when the group already holds it. The
/// value written is what the part's kind normalises, and the change replaces the deepest node the document already
/// holds on the way (a group a file leaves to its defaults is created whole), so undo restores what the file said.
///
/// # Errors
/// `invalid` on `value` for a fringe that is out of range or that the part's kind rejects; `internal` if the
/// parameters can't be turned into a document value.
pub(crate) fn changes(ctx: &OpCtx<'_>, target: &BangsGroup, after: &BangsGen) -> Result<Vec<Change>, OpError> {
    check(after).map_err(|fault| refusal("value", &fault))?;
    if *after == target.bangs {
        return Ok(Vec::new());
    }
    let mut hair = target.hair.clone();
    let Some(group) = hair.groups.get_mut(&target.group) else {
        return Err(OpError::internal(format!("group `{}` vanished while it was edited", target.group)));
    };
    group.generator = Generator::Bangs(after.clone());
    let value = serde_json::to_value(&hair).map_err(|e| OpError::internal(format!("serialising hair failed: {e}")))?;
    let normalised = (target.kind.check)(&value)
        .map_err(|e| OpError::invalid("value", format!("the part's kind rejects the fringe: {e}")))?;

    let steps = ["groups", target.group.as_str(), "generator"];
    let held = (0..=steps.len()).rev().find(|depth| ctx.doc.get(&below(&target.params_path, &steps[..*depth])).is_some());
    let depth = held.unwrap_or(0);
    let path = below(&target.params_path, &steps[..depth]);
    let written = at_steps(&normalised, &steps[..depth]).cloned();
    let change = character::change(ctx, path, written);
    Ok(if change.before == change.after { Vec::new() } else { vec![change] })
}

/// `base` with `steps` appended.
fn below(base: &DocPath, steps: &[&str]) -> DocPath {
    steps.iter().fold(base.clone(), |path, step| path.join(*step))
}

/// The value at `steps` below `root`.
fn at_steps<'a>(root: &'a Value, steps: &[&str]) -> Option<&'a Value> {
    if steps.is_empty() { Some(root) } else { at(root, &steps.join("/")) }
}

/// The nearest of `names` to `wanted`, as a "did you mean" clause (empty when nothing is near).
pub(crate) fn did_you_mean<'a>(wanted: &str, names: impl IntoIterator<Item = &'a str>) -> String {
    nearest(wanted, names).map_or_else(String::new, |n| format!("did you mean `{n}`? "))
}
