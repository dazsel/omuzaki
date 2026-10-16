//! `to_avatar`: an imported [`IoScene`] becomes an [`ImportedAvatar`], the performer body of `docs/design/byom.md`
//! (D1, D3, D4, D7, D8; ADR 0033).
//!
//! The avatar is rebuilt from the stored original plus its [`AvatarDef`] on every load (D5), so this function is the
//! whole preparation: `io.inspect` and `io.import` only call it. It works on a private copy of the scene, in this
//! order, and every step's findings join one [`Report`]. A scene that breaks its own invariants ([`IoScene::check`])
//! is refused first; readers never produce one.
//! 1. **Humanoid** ([`humanoid::auto_map`], [`humanoid::apply`]): the file's own map and the person's overrides in
//!    `def.humanoid` are checked like any assignment; an override whose node index went stale (the file changed) is
//!    found again by its stored name. A missing required bone rejects the import (`humanoid.required_missing`); an
//!    override is how a person fixes it.
//! 2. **T rest** ([`tpose::normalise_rest`]): its `bind` goes into the avatar.
//! 3. **Cleanup** ([`cleanup::cleanup`]): weld at 1e-6, merge primitives by material, drop what nothing uses.
//! 4. **`ARKit` synthesis** ([`arkit::synthesise`]), then the shapes it could not build, and halves of symmetric ones,
//!    made from geometry ([`arkit_shapes::generate`]): the outcomes choose the gaze route.
//! 5. **Conversion**, one private module each: the skeleton (`skeleton`), materials and textures (`materials`),
//!    meshes with their own skins and sparse morphs (`meshes`), expressions and drivers (`expressions`), the gaze route
//!    and look-at (`gaze`), and springs and constraints (`physics`).
//! 6. **Retained**: everything export needs that the runtime does not use, so an unedited avatar re-exports with
//!    every field of the original (D8). Node names, transforms and the scene graph are taken before any step changes
//!    them; materials, textures and images as cleanup left them (it only drops unused ones; values are untouched).
//!
//! Deterministic: no clock, no randomness, and every table is walked in a fixed order.

mod expressions;
mod gaze;
mod materials;
mod meshes;
mod physics;
pub(crate) mod skeleton;

pub(crate) use materials::apply_edit as apply_material_edit;

use std::collections::BTreeMap;
use std::time::Duration;

use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::AvatarDef;
use omuzaki_avatar_api::retained::{Retained, RetainedAnnotation, RetainedNode, RetainedSkin, RetainedTransform};
use omuzaki_core::time::Clock;
use omuzaki_io_api::{Finding, HumanBoneName, IoError, IoScene, NodeIx, Report};
use omuzaki_render_api::MtoonSupport;

use super::arkit::Synthesis;
use super::{arkit, arkit_shapes, cleanup, humanoid, tpose};
use crate::convert::bones::to_file;

/// What the cleanup step is asked to do on an imported avatar (`docs/design/byom.md` §5, BY3).
pub const CLEANUP: cleanup::CleanupOptions =
    cleanup::CleanupOptions { weld: Some(1e-6), merge_by_material: true, drop_unused: true };

/// Prepares `scene` as an avatar: the steps of the module docs, on a copy. `mtoon` says which `MToon` groups the
/// renderer draws (render-api `Capabilities::mtoon`); each group a material uses and the renderer cannot draw is one
/// `look.mtoon_not_drawn` finding.
///
/// # Errors
/// [`IoError::Rejected`] with the whole report when the model cannot become an avatar: the scene breaks its invariants
/// (`scene.*`, from [`IoScene::check`]), a required humanoid bone has no
/// node (`humanoid.required_missing`), an override is unusable (`humanoid.override_invalid`), the skeleton breaks a
/// rule (`avatar.skeleton_invalid`, `avatar.too_many_bones`), the node layout is from a later build
/// (`avatar.schema_unknown`), or the result does not validate (`avatar.invalid`).
pub fn to_avatar(scene: &IoScene, def: &AvatarDef, mtoon: MtoonSupport) -> Result<(ImportedAvatar, Report), IoError> {
    prepare(scene, def, mtoon, None).map(|(avatar, report, _)| (avatar, report))
}

/// How long [`to_avatar_timed`]'s stages took on the caller's clock: D6's "prepare" and "decode" rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrepareTimings {
    /// Everything but the texture decode: the humanoid map, the T rest, cleanup, synthesis and the conversion.
    pub prepare: Duration,
    /// Decoding the textures.
    pub decode: Duration,
}

/// [`to_avatar`], timing its stages on `clock` (the op's clock, so a test's fake clock gives fixed numbers). The
/// avatar and the report do not depend on the clock.
///
/// # Errors
/// As [`to_avatar`].
pub fn to_avatar_timed(
    scene: &IoScene,
    def: &AvatarDef,
    mtoon: MtoonSupport,
    clock: &dyn Clock,
) -> Result<(ImportedAvatar, Report, PrepareTimings), IoError> {
    prepare(scene, def, mtoon, Some(clock))
}

/// The time between two readings of a clock (zero if it went backwards).
pub(crate) fn between(start: omuzaki_core::time::Seconds, end: omuzaki_core::time::Seconds) -> Duration {
    Duration::try_from_secs_f64(end.0 - start.0).unwrap_or(Duration::ZERO)
}

/// The preparation of the module docs, timed when `clock` is given.
fn prepare(
    scene: &IoScene,
    def: &AvatarDef,
    mtoon: MtoonSupport,
    clock: Option<&dyn Clock>,
) -> Result<(ImportedAvatar, Report, PrepareTimings), IoError> {
    let start = clock.map(Clock::now);
    let mut report = Report::new();
    if let Err(e) = def.check_schema() {
        let hint = "update Omuzaki to open this project";
        report.push(Finding::error("avatar.schema_unknown", e.to_string()).at("avatar/schema").hint(hint));
        return Err(IoError::Rejected(report));
    }
    // Every step below relies on the scene's invariants (indices in range, arrays the same length). A reader's scene
    // already passed them (`run_import` enforces it), so only the errors are added: its warnings are in the read's
    // report already.
    let invariants = scene.check();
    if invariants.has_errors() {
        report.extend(invariants);
        return Err(IoError::Rejected(report));
    }
    let nodes = retained_nodes(scene);
    let mut work = scene.clone();

    let overrides = humanoid_overrides(&work, def, &mut report);
    match humanoid::auto_map(&work, &overrides) {
        Ok(proposal) => {
            humanoid::apply(&mut work, &proposal);
            report.extend(proposal.report);
        }
        Err(e) => {
            report.extend(e.report);
            return Err(IoError::Rejected(report));
        }
    }
    let rest = tpose::normalise_rest(&mut work);
    report.extend(rest.report);
    report.extend(cleanup::cleanup(&mut work, &CLEANUP).report());
    let mut arkit = arkit::synthesise(&mut work);
    let wanted: Vec<&str> =
        arkit.channels.iter().filter(|c| arkit_shapes::worth_generating(c)).map(|c| c.channel).collect();
    for made in arkit_shapes::generate(&mut work, &wanted) {
        if let (Synthesis::Generated { .. }, Some(slot)) =
            (&made.how, arkit.channels.iter_mut().find(|c| c.channel == made.channel))
        {
            *slot = made;
        }
    }
    report.extend(arkit.report());

    let rig = match skeleton::build(&work) {
        Ok(rig) => rig,
        Err(findings) => {
            report.extend(findings);
            return Err(IoError::Rejected(report));
        }
    };
    let mut avatar = ImportedAvatar::new(def.source.hash, rig.skeleton.clone());
    avatar.bind = rest.bind;
    let mut decode = Duration::ZERO;
    let names = materials::convert(&work, def, mtoon, clock, &mut decode, &mut avatar, &mut report);
    meshes::convert(&work, &rig, &names, &mut avatar, &mut report);
    expressions::convert(&work, def, &arkit, &names, &mut avatar, &mut report);
    physics::convert(&work, &rig, &mut avatar, &mut report);
    retain(scene, &work, &rig, nodes, &mut avatar.retained);
    // A step that could not convert part of the model said why in an error finding; that is the reason to give.
    if report.has_errors() {
        return Err(IoError::Rejected(report));
    }

    if let Err(e) = avatar.validate() {
        let hint = "this is a bug in Omuzaki's importer, or a file it does not understand yet; report it with the file";
        report.push(Finding::error("avatar.invalid", e.to_string()).at("avatar").hint(hint));
        return Err(IoError::Rejected(report));
    }
    let total = start.zip(clock).map_or(Duration::ZERO, |(start, clock)| between(start, clock.now()));
    Ok((avatar, report, PrepareTimings { prepare: total.saturating_sub(decode), decode }))
}

/// The humanoid overrides `auto_map` checks: the file's own map, with the person's choices in `def.humanoid` on top.
/// A choice's node index wins while it still names the node of that name; when it does not (the file was replaced and
/// the node moved), the node is found again by its stored name (`humanoid.override_refound`); when no node has that
/// name, the override is reported unusable (`humanoid.override_invalid`) and the file's own map stands.
fn humanoid_overrides(scene: &IoScene, def: &AvatarDef, report: &mut Report) -> BTreeMap<HumanBoneName, NodeIx> {
    let mut overrides = scene.avatar.as_ref().map(|a| a.humanoid.bones.clone()).unwrap_or_default();
    for (bone, choice) in &def.humanoid {
        let Ok(name) = to_file(*bone) else {
            debug_assert!(false, "every runtime humanoid bone has a file name");
            continue;
        };
        let at = format!("avatar/humanoid/{name}");
        let named = |i: usize| scene.nodes.get(i).is_some_and(|n| n.name == choice.name);
        let index = usize::try_from(choice.node).unwrap_or(usize::MAX);
        let node = if named(index) {
            Some(index)
        } else {
            let found = (0..scene.nodes.len()).find(|&i| named(i));
            if let Some(i) = found {
                let msg = format!(
                    "the override for {name} named node {} ({}); the file has it at {i} now, so that node was used",
                    choice.node, choice.name
                );
                report.push(Finding::info("humanoid.override_refound", msg).at(at.as_str()));
            } else {
                let msg = format!("the override for {name} names node {:?}, which the file no longer has", choice.name);
                let hint = "map the bone again with avatar.remap";
                report.push(Finding::warning("humanoid.override_invalid", msg).at(at.as_str()).hint(hint));
            }
            found
        };
        if let Some(node) = node.and_then(|i| u32::try_from(i).ok()) {
            // A node plays one bone: the person's choice takes it from whatever the file mapped to it.
            overrides.retain(|_, n| n.0 != node);
            overrides.insert(name, NodeIx(node));
        }
    }
    overrides
}

/// The file's nodes as it has them, before any step turns a rest or renames anything.
fn retained_nodes(scene: &IoScene) -> Vec<RetainedNode> {
    scene
        .nodes
        .iter()
        .map(|n| {
            let t = &n.transform;
            let mut node = RetainedNode::new(n.name.clone())
                .with_transform(RetainedTransform { translation: t.translation, rotation: t.rotation, scale: t.scale })
                .with_children(n.children.iter().map(|c| c.0).collect());
            if let Some(mesh) = n.mesh {
                node = node.with_mesh(mesh.0, n.skin.map(|s| s.0));
            }
            node.passthrough = n.passthrough.clone();
            node
        })
        .collect()
}

/// Fills what the conversion steps did not: the nodes with their bones, the roots, the skins, the first-person
/// annotations, the meta and the file's passthrough (D8).
fn retain(
    source: &IoScene,
    work: &IoScene,
    rig: &skeleton::Rig,
    mut nodes: Vec<RetainedNode>,
    retained: &mut Retained,
) {
    // No step adds or removes nodes, so the file's node indices are the scene's and the rig's.
    for (i, node) in nodes.iter_mut().enumerate() {
        node.bone = rig.bones.get(i).copied().flatten();
    }
    retained.nodes = nodes;
    retained.roots = source.roots.iter().map(|r| r.0).collect();
    retained.skins = work
        .skins
        .iter()
        .map(|s| {
            let skin =
                RetainedSkin::new(s.name.clone(), s.joints.iter().map(|j| j.0).collect(), s.inverse_bind.clone());
            match s.skeleton {
                Some(root) => skin.with_skeleton(root.0),
                None => skin,
            }
        })
        .collect();
    retained.first_person = source
        .avatar
        .as_ref()
        .and_then(|a| a.first_person.as_ref())
        .map(|fp| fp.annotations.iter().map(|a| RetainedAnnotation::new(a.node.0, a.flag.into())).collect());
    // The meta as cleanup left it: its thumbnail index follows the compacted image list that `retained.images` holds.
    retained.meta = source.avatar.is_some().then(|| work.meta.clone());
    retained.passthrough = source.passthrough.clone();
}
