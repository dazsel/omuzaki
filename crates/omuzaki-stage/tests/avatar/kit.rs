//! The kit's own character as a person's file: exported to VRM 1.0, read back and prepared as an avatar, the way
//! `io.import {as: avatar}` prepares a dropped model. Shared by the avatar stage tests; no third-party model.

// Not every test file uses every helper.
#![allow(dead_code, clippy::expect_used)]

use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    ExportCtx, ExportOptions, Expression, ExpressionName, ImportCtx, ImportInput, ImportOptions, IoScene, MorphTarget,
    PerfectSync, run_export, run_import,
};
use omuzaki_io_ops::gather::avatar::add_avatar;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use omuzaki_render_api::MtoonSupport;
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source};

/// The kit's mock character, built.
pub fn kit() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("kit").expect("an id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

/// `built` as a VRM 1.0 file: what a person would drop on the studio.
pub fn vrm_bytes(built: &BuiltCharacter) -> Vec<u8> {
    let mut gathered =
        gather(built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).expect("gathers");
    add_avatar(&mut gathered, built).expect("the avatar block");
    // The rights a person sets on their own character (the op layer records them); the export refuses a file without.
    let meta = &mut gathered.scene.meta;
    meta.name = "Kit".into();
    meta.authors = vec!["Omuzaki".into()];
    meta.license_url = "https://vrm.dev/licenses/1.0/".into();
    gathered.scene.provenance = Some(provenance());
    let options = ExportOptions::default().with_perfect_sync(PerfectSync::Off);
    let outcome = run_export(&Vrm1Exporter, &gathered.scene, &ExportCtx::new(&options)).expect("exports");
    outcome.files.into_iter().next().expect("the .vrm file").bytes
}

/// The record of a character the project made itself.
pub fn provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Omuzaki".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// `built` as a VRM 1.0 file, read back.
pub fn as_vrm(built: &BuiltCharacter) -> IoScene {
    let bytes = vrm_bytes(built);
    run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
        .expect("reads back")
        .scene
}

/// How far the morph target this file gains moves every vertex: 3 cm toward the viewer.
pub const MORPH: Vec3 = Vec3::new(0.0, 0.0, 0.03);

/// Adds one morph target to the first mesh a node draws and an `aa` expression bound to it, as a mouth shape would be.
pub fn with_morph(mut scene: IoScene) -> IoScene {
    let node = scene.nodes.iter().position(|n| n.mesh.is_some()).expect("a drawing node");
    let mesh = scene.nodes[node].mesh.expect("a mesh").index();
    for primitive in &mut scene.meshes[mesh].primitives {
        let moved = vec![MORPH; primitive.mesh.positions.len()];
        primitive.morph_targets.push(MorphTarget::new(moved));
    }
    let node = omuzaki_io_api::NodeIx(u32::try_from(node).expect("a small scene"));
    scene
        .avatar
        .as_mut()
        .expect("an avatar")
        .expressions
        .insert(ExpressionName::parse("aa").expect("a name"), Expression::morph(node, 0));
    scene
}

/// `scene` prepared as an avatar, as `io.import {as: avatar}` prepares it.
pub fn avatar_of(scene: &IoScene) -> ImportedAvatar {
    let def = AvatarDef::new(AvatarSource::new(ContentHash([7; 32]), SourceFormat::Vrm1, "kit.vrm"));
    to_avatar(scene, &def, MtoonSupport::NONE).expect("the kit's VRM imports as an avatar").0
}
