//! Exports carry the character's look (LK-06): a look applied to the built character reaches the gathered
//! `MToon` fields, and leaves the colours the palette gave alone.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, ClassLook, LookParams, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{Material, Shading};
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};

fn mock_built() -> BuiltCharacter {
    RegistryBuilder
        .build(
            &CharId::new("mock").unwrap(),
            &mock_character().unwrap(),
            &BuildOptions::default(),
            &BuildServices::default(),
        )
        .unwrap()
}

fn gathered(built: &BuiltCharacter) -> Vec<Material> {
    gather(built, &GatherOptions { target_height: None, purpose: GatherPurpose::Gltf }).unwrap().scene.materials
}

/// The toon fields of a gathered material that a look may change, and those it must not.
fn toon(m: &Material) -> &omuzaki_io_api::Toon {
    match &m.shading {
        Shading::Toon(t) => t,
        other => panic!("material '{}' is not MToon: {other:?}", m.name),
    }
}

#[test]
fn the_gathered_mtoon_fields_follow_the_look() {
    let plain = mock_built();
    let mut looked = mock_built();
    let hard = LookParams {
        all: ClassLook {
            softness_scale: Some(0.0),
            rim_scale: Some(0.0),
            outline_width: Some(0.003),
            ..ClassLook::default()
        },
        ..LookParams::default()
    };
    looked.apply_look(&hard).unwrap();

    let (before, after) = (gathered(&plain), gathered(&looked));
    assert_eq!(before.len(), after.len());
    let mut toon_materials = 0;
    for (b, a) in before.iter().zip(&after) {
        assert_eq!(b.name, a.name);
        let (tb, ta) = (toon(b), toon(a));
        assert_eq!(ta.outline_width, 0.003, "{}: one uniform line", a.name);
        assert_eq!((a.base_color, tb.shade_color), (b.base_color, ta.shade_color), "{}: colours stay", a.name);
        let is_toon = looked.materials.iter().any(|(name, spec)| {
            name.as_str() == a.name && matches!(spec.shading, omuzaki_render_api::Shading::Toon(_))
        });
        if is_toon {
            toon_materials += 1;
            assert_eq!(ta.shading_toony, 1.0, "{}: a hard terminator is MToon's toony 1", a.name);
            assert_eq!(ta.rim_color, Vec3::ZERO, "{}: no rim", a.name);
            assert!(tb.shading_toony < 1.0 && tb.rim_color != Vec3::ZERO, "{}: the parts' own look was soft", a.name);
        }
    }
    assert!(toon_materials >= 3, "the mock's skin and garments are toon: {toon_materials}");

    // Back to the parts' own look: the gather is as before, field for field.
    looked.apply_look(&LookParams::default()).unwrap();
    assert_eq!(gathered(&looked), before);
}
