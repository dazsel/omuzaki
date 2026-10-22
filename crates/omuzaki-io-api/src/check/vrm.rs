//! Rules a scene must satisfy to be written as a VRM (`vrm.*` and `vrm0.*`).

use std::collections::BTreeMap;

use crate::{
    Avatar, ColliderShape, Finding, HUMAN_BONES, HumanBoneName, IoScene, Report, VRM_LICENSE_URLS, VrmVersion,
};

/// Everything a VRM exporter of `version` needs from the scene, beyond the structural invariants.
pub fn vrm_profile(scene: &IoScene, version: VrmVersion) -> Report {
    let mut r = Report::new();
    meta(scene, version, &mut r);
    let Some(avatar) = &scene.avatar else {
        r.push(
            Finding::error("vrm.humanoid_missing", "the scene has no avatar data, so there is no humanoid")
                .hint("assign at least the 15 required bones"),
        );
        return r;
    };
    humanoid(scene, avatar, version, &mut r);
    spring_losses(avatar, version, &mut r);
    if version == VrmVersion::V0 {
        material_names(scene, avatar, &mut r);
    }
    r
}

fn meta(scene: &IoScene, version: VrmVersion, r: &mut Report) {
    let m = &scene.meta;
    if m.name.trim().is_empty() {
        r.push(Finding::error("vrm.meta_name_missing", "the model needs a name").at("meta/name"));
    }
    if !m.authors.iter().any(|a| !a.trim().is_empty()) {
        r.push(Finding::error("vrm.meta_authors_missing", "at least one author is required").at("meta/authors"));
    }
    if version == VrmVersion::V1 {
        let url = m.license_url.trim();
        if url.is_empty() {
            let hint = format!("use {}", VRM_LICENSE_URLS[0]);
            r.push(
                Finding::error("vrm.meta_license_url_missing", "licenseUrl is required")
                    .at("meta/licenseUrl")
                    .hint(hint),
            );
        } else if !VRM_LICENSE_URLS.contains(&url) {
            let msg = "licenseUrl should be a VRM Public License 1.0 page; put other terms in otherLicenseUrl";
            r.push(Finding::warning("vrm.meta_license_url_unknown", msg).at("meta/licenseUrl"));
        }
    }
}

fn humanoid(scene: &IoScene, avatar: &Avatar, version: VrmVersion, r: &mut Report) {
    for spec in HUMAN_BONES.iter().filter(|b| b.required_in(version)) {
        if avatar.humanoid.node(spec.name).is_none() {
            r.push(
                Finding::error("vrm.humanoid_required_bone", format!("required bone {} isn't assigned", spec.name))
                    .at(format!("avatar/humanoid/{}", spec.name)),
            );
        }
    }
    let parents = parent_table(scene);
    let bone_of: BTreeMap<usize, HumanBoneName> = avatar.humanoid.bones.iter().map(|(b, n)| (n.index(), *b)).collect();
    for (bone, node) in &avatar.humanoid.bones {
        let at = format!("avatar/humanoid/{bone}");
        if let Some(n) = scene.nodes.get(node.index())
            && n.transform.scale.min_element() <= 0.0
        {
            r.push(
                Finding::error("vrm.humanoid_scale", format!("{bone} has a zero or negative scale"))
                    .at(&at)
                    .hint("humanoid scales must be positive"),
            );
        }
        parent_rule(avatar, &parents, &bone_of, *bone, node.index(), &at, r);
    }
}

/// The bone `bone` must sit under: its spec parent, or the next present one up when the parent is optional and
/// absent. `Err` names the parent that `bone` cannot exist without.
fn expected_parent(avatar: &Avatar, bone: HumanBoneName) -> Result<Option<&'static str>, &'static str> {
    let spec = bone.spec();
    let Some(first) = spec.parent else { return Ok(None) };
    if avatar.humanoid.node(first).is_some() {
        return Ok(Some(first));
    }
    if spec.needs_parent {
        return Err(first);
    }
    let mut up = first;
    while let Some(next) = HumanBoneName::new(up).ok().and_then(|b| b.spec().parent) {
        if avatar.humanoid.node(next).is_some() {
            return Ok(Some(next));
        }
        up = next;
    }
    Ok(None)
}

fn parent_rule(
    avatar: &Avatar,
    parents: &[Option<usize>],
    bone_of: &BTreeMap<usize, HumanBoneName>,
    bone: HumanBoneName,
    node: usize,
    at: &str,
    r: &mut Report,
) {
    let want = match expected_parent(avatar, bone) {
        Ok(Some(want)) => want,
        Ok(None) => return,
        Err(missing) => {
            r.push(
                Finding::error("vrm.humanoid_missing_parent", format!("{bone} needs {missing} to be assigned")).at(at),
            );
            return;
        }
    };
    // The nearest ancestor node that is itself a humanoid bone. Bounded by the table size, so a cycle can't hang it.
    let mut cur = parents.get(node).copied().flatten();
    for _ in 0..parents.len() {
        match cur {
            Some(n) if !bone_of.contains_key(&n) => cur = parents.get(n).copied().flatten(),
            _ => break,
        }
    }
    let actual = cur.and_then(|n| bone_of.get(&n)).map(|b| b.as_str());
    if actual != Some(want) {
        let found = actual.unwrap_or("nothing");
        r.push(
            Finding::error("vrm.humanoid_parent", format!("{bone} sits under {found}; the humanoid tree wants {want}"))
                .at(at),
        );
    }
}

fn parent_table(scene: &IoScene) -> Vec<Option<usize>> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, n) in scene.nodes.iter().enumerate() {
        for c in &n.children {
            if let Some(slot) = parent.get_mut(c.index()) {
                *slot = Some(i);
            }
        }
    }
    parent
}

/// Spring features VRM 0.x cannot carry: inside-out and plane colliders and joint limits, which a 0.x file leaves out
/// (`vrm0.collider_unsupported`, `vrm0.limit_unsupported`). VRM 1.0 carries them all
/// (`VRMC_springBone_extended_collider`, `VRMC_springBone_limit`), so it warns about none; a writer that cannot write
/// them says so in its own check. Here rather than in the writers so that strict mode and `Exporter::check` see them
/// before anything is written.
fn spring_losses(avatar: &Avatar, version: VrmVersion, r: &mut Report) {
    let VrmVersion::V0 = version else { return };
    let sb = &avatar.spring_bones;
    // Counted over every collider, once each: an ungrouped one is dropped too, and a shared one is still one collider.
    let colliders = sb
        .colliders
        .iter()
        .filter(|c| !matches!(c.shape, ColliderShape::Sphere { .. } | ColliderShape::Capsule { .. }))
        .count();
    let limits = sb.chains.iter().flat_map(|c| &c.joints).filter(|j| j.limit.is_some()).count();
    if colliders > 0 {
        let msg = format!("0.x has only outside spheres; {colliders} inside-out or plane collider(s) are dropped");
        r.push(Finding::warning("vrm0.collider_unsupported", msg).at("avatar/springBone/colliders"));
    }
    if limits > 0 {
        let msg = format!("0.x has no joint limits; {limits} limit(s) are dropped");
        r.push(Finding::warning("vrm0.limit_unsupported", msg).at("avatar/springBone/springs"));
    }
}

/// VRM 0.x expressions refer to materials by *name*, so the names they use must be unique.
fn material_names(scene: &IoScene, avatar: &Avatar, r: &mut Report) {
    let used = avatar
        .expressions
        .values()
        .flat_map(|e| e.color_binds.iter().map(|b| b.material).chain(e.uv_binds.iter().map(|b| b.material)));
    for m in used {
        let Some(name) = scene.materials.get(m.index()).map(|m| m.name.as_str()) else { continue };
        if scene.materials.iter().filter(|o| o.name == name).count() > 1 {
            r.push(
                Finding::error(
                    "vrm0.material_name_duplicate",
                    format!("material name {name:?} is used more than once; 0.x binds find materials by name"),
                )
                .at(format!("materials/{}", m.0)),
            );
        }
    }
}
