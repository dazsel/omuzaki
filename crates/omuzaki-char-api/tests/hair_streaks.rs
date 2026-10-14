//! Hair streaks, inner darkening and the RNG switch (CB-34, decision G6): the generator pass is deterministic, the
//! defaults change nothing, and the data model round-trips and validates with paths.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    Bulge, Falloff, Follow, GroupId, HairClump, HairParams, HairRng, HairShading, HairShadingError, Streak,
    StreakBlend, Taper,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{ColourRef, Palette, Validate};

/// `n` plain clumps in `group`, all at full shade and untinted.
fn clumps(group: &str, n: usize) -> Vec<HairClump> {
    let group = GroupId::new(group).unwrap();
    (0..n)
        .map(|_| HairClump {
            group: group.clone(),
            rest: vec![Vec3::ZERO, Vec3::Y],
            width: Taper::default(),
            thickness: Bulge::default(),
            stiffness: Falloff::default(),
            shade: 1.0,
            simulate: false,
            follow: Follow::Head,
            tint: None,
        })
        .collect()
}

/// A streak over every group, replacing the hair with `colour` at full strength.
fn streak(fraction: f64, colour: ColourRef, seed: u32) -> Streak {
    Streak { fraction, colour, strength: 1.0, groups: Vec::new(), seed, blend: StreakBlend::Replace }
}

fn purple() -> ColourRef {
    ColourRef::rgb(0x9b, 0x6e, 0xd6)
}

fn apply(shading: &HairShading, clumps: &mut [HairClump]) -> Result<(), HairShadingError> {
    shading.apply_to_clumps(&Palette::default(), clumps)
}

#[test]
fn the_default_shading_changes_no_clump() {
    let mut set = clumps("cap", 3);
    set[1].shade = 0.7;
    apply(&HairShading::default(), &mut set).unwrap();
    assert_eq!(set[0].shade, 1.0);
    assert_eq!(set[1].shade.to_bits(), 0.7_f32.to_bits(), "the default keeps the shade bit for bit");
    assert!(set.iter().all(|c| c.tint.is_none()));
}

#[test]
fn inner_darkening_one_is_the_designed_darkening_zero_is_none_and_it_is_monotonic() {
    let mut set = clumps("bangs", 1);
    set[0].shade = 0.6;
    let shade_at = |d: f64| {
        let mut copy = set.clone();
        let shading = HairShading { inner_darkening: d, ..HairShading::default() };
        apply(&shading, &mut copy).unwrap();
        copy[0].shade
    };
    assert_eq!(shade_at(1.0).to_bits(), 0.6_f32.to_bits(), "1 keeps the designed shade bit for bit");
    assert!((shade_at(0.5) - 0.8).abs() < 1e-6, "halfway: {}", shade_at(0.5));
    assert!((shade_at(0.0) - 1.0).abs() < 1e-6, "0 draws full shade: {}", shade_at(0.0));
    let steps: Vec<f32> = (0..=10).map(|i| shade_at(f64::from(i) / 10.0)).collect();
    assert!(steps.windows(2).all(|w| w[0] >= w[1]), "shade falls as d rises: {steps:?}");
}

#[test]
fn a_streak_takes_its_share_and_the_same_seed_picks_the_same_clumps() {
    let shading = HairShading { streaks: vec![streak(0.3, purple(), 42)], ..HairShading::default() };
    let (mut first, mut second) = (clumps("curtain", 2000), clumps("curtain", 2000));
    apply(&shading, &mut first).unwrap();
    apply(&shading, &mut second).unwrap();
    let taken = first.iter().filter(|c| c.tint.is_some()).count();
    assert!((540..=660).contains(&taken), "about 30 % of 2000 clumps, got {taken}");
    assert_eq!(first, second, "a build is identical every time");

    let other = HairShading { streaks: vec![streak(0.3, purple(), 43)], ..HairShading::default() };
    let mut third = clumps("curtain", 2000);
    apply(&other, &mut third).unwrap();
    assert_ne!(first, third, "another seed picks other clumps");
}

#[test]
fn the_tints_of_the_groups_alone_are_the_tints_the_generator_pass_writes() {
    // Streaks that overlap, blend two ways and reach one group, over three groups of clumps: the recolour of built hair
    // knows the groups and nothing else, and must get the numbers the build wrote into the vertex colours.
    let streaks = vec![
        streak(0.4, purple(), 3),
        Streak {
            groups: vec![GroupId::new("bangs").unwrap()],
            blend: StreakBlend::Multiply,
            ..streak(0.7, purple(), 9)
        },
        streak(0.2, ColourRef::rgb(10, 200, 30), 11),
    ];
    let shading = HairShading { streaks, ..HairShading::default() };
    let mut set = [clumps("cap", 40), clumps("bangs", 50), clumps("curtain", 30)].concat();
    let groups: Vec<&GroupId> = set.iter().map(|c| &c.group).collect();
    let palette = Palette::default();
    let tints = shading.streak_tints(&palette, &groups).unwrap();
    shading.apply_to_clumps(&palette, &mut set).unwrap();
    assert_eq!(tints, set.iter().map(|c| c.tint).collect::<Vec<_>>());
    assert!(tints.iter().any(Option::is_some) && tints.iter().any(Option::is_none), "a mix, or the test proves little");
}

#[test]
fn streak_tints_follow_the_palette_and_fail_like_the_generator_pass() {
    let slot = ColourRef::slot_or("streak", ColourRef::rgb(1, 2, 3));
    let shading = HairShading { streaks: vec![streak(1.0, slot, 1)], ..HairShading::default() };
    let groups = [GroupId::new("cap").unwrap()];
    let groups: Vec<&GroupId> = groups.iter().collect();
    let mut palette = Palette::default();
    let err = shading.streak_tints(&palette, &groups).unwrap_err();
    assert_eq!(Err(err), apply(&shading, &mut clumps("cap", 1)), "the same error");
    palette.0.insert(omuzaki_face_api::PaletteSlot::new("streak").unwrap(), Colour::rgb(200, 0, 0));
    let red = shading.streak_tints(&palette, &groups).unwrap();
    palette.0.insert(omuzaki_face_api::PaletteSlot::new("streak").unwrap(), Colour::rgb(0, 0, 200));
    let blue = shading.streak_tints(&palette, &groups).unwrap();
    assert_ne!(red, blue);
}

#[test]
fn fractions_of_zero_and_one_take_none_and_all() {
    for (fraction, expected) in [(0.0, 0), (1.0, 50)] {
        let shading = HairShading { streaks: vec![streak(fraction, purple(), 5)], ..HairShading::default() };
        let mut set = clumps("cap", 50);
        apply(&shading, &mut set).unwrap();
        assert_eq!(set.iter().filter(|c| c.tint.is_some()).count(), expected, "fraction {fraction}");
    }
}

#[test]
fn a_streak_only_reaches_its_groups() {
    let shading = HairShading {
        streaks: vec![Streak { groups: vec![GroupId::new("b").unwrap()], ..streak(1.0, purple(), 1) }],
        ..HairShading::default()
    };
    let mut set = clumps("a", 4);
    set.extend(clumps("b", 4));
    apply(&shading, &mut set).unwrap();
    assert!(set[..4].iter().all(|c| c.tint.is_none()), "group a is untouched");
    assert!(set[4..].iter().all(|c| c.tint.is_some()), "group b is streaked");
}

#[test]
fn replace_at_full_strength_gives_the_streak_colour_exactly() {
    let shading = HairShading { streaks: vec![streak(1.0, purple(), 1)], ..HairShading::default() };
    let mut set = clumps("cap", 1);
    apply(&shading, &mut set).unwrap();
    assert_eq!(set[0].tint, Some(Colour::rgb(0x9b, 0x6e, 0xd6)));
}

#[test]
fn multiply_by_white_keeps_the_hair_colour() {
    let white = ColourRef::rgb(0xff, 0xff, 0xff);
    let shading = HairShading {
        streaks: vec![Streak { blend: StreakBlend::Multiply, ..streak(1.0, white, 1) }],
        ..HairShading::default()
    };
    let mut set = clumps("cap", 1);
    apply(&shading, &mut set).unwrap();
    let mid = Palette::default().resolve(&HairShading::default().mid).unwrap();
    assert_eq!(set[0].tint, Some(mid));
}

#[test]
fn later_streaks_draw_over_earlier_ones() {
    let blue = ColourRef::rgb(0x20, 0x40, 0xf0);
    let shading =
        HairShading { streaks: vec![streak(1.0, purple(), 1), streak(1.0, blue, 2)], ..HairShading::default() };
    let mut set = clumps("cap", 1);
    apply(&shading, &mut set).unwrap();
    assert_eq!(set[0].tint, Some(Colour::rgb(0x20, 0x40, 0xf0)));
}

#[test]
fn an_undefined_palette_slot_is_an_error() {
    let shading =
        HairShading { streaks: vec![streak(1.0, ColourRef::parse("@nope").unwrap(), 1)], ..HairShading::default() };
    let mut set = clumps("cap", 1);
    assert!(matches!(apply(&shading, &mut set), Err(HairShadingError::Colour(_))));
}

#[test]
fn validation_reports_the_streak_paths() {
    let mut p = HairParams::default();
    p.shading.inner_darkening = 2.0;
    p.shading.streaks =
        vec![Streak { fraction: 1.5, groups: vec![GroupId::new("nope").unwrap()], ..Streak::default() }];
    let e = p.validate().unwrap_err();
    let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
    for expected in ["shading/inner_darkening", "shading/streaks/0/fraction", "shading/streaks/0/groups"] {
        assert!(paths.contains(&expected), "{expected} missing from {paths:?}");
    }
}

#[test]
fn an_old_fixture_loads_to_the_same_hairstyle_and_the_new_fields_round_trip() {
    let mut old = serde_json::to_value(HairParams::default()).unwrap();
    old.as_object_mut().unwrap().remove("rng");
    let shading = old["shading"].as_object_mut().unwrap();
    shading.remove("streaks");
    shading.remove("inner_darkening");
    assert_eq!(serde_json::from_value::<HairParams>(old).unwrap(), HairParams::default());

    let streaks = vec![Streak { groups: vec![GroupId::new("c-curtain").unwrap()], ..Streak::default() }];
    let shading = HairShading { streaks, ..HairShading::default() };
    let p = HairParams { shading, rng: HairRng::LegacyLcg { seed: 7 }, ..HairParams::default() };
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(serde_json::from_str::<HairParams>(&json).unwrap(), p);
    assert_eq!(
        serde_json::to_value(HairRng::LegacyLcg { seed: 7 }).unwrap(),
        serde_json::json!({"legacy_lcg": {"seed": 7}})
    );
    assert_eq!(serde_json::to_value(HairRng::Forked).unwrap(), serde_json::json!("forked"));
}

#[test]
fn the_schema_names_the_streaks_and_the_rng_switch() {
    let schema = serde_json::to_string(&schemars::schema_for!(HairParams)).unwrap();
    for name in ["streaks", "inner_darkening", "legacy_lcg", "forked", "replace", "multiply"] {
        assert!(schema.contains(name), "the schema lacks `{name}`");
    }
}

#[test]
fn the_default_hair_outline_is_the_prototypes_and_the_line_colour_is_not() {
    let shading = HairShading::default();
    assert_eq!(shading.outline, omuzaki_face_api::ColourRef::rgb(0xa4, 0x78, 0x68));
    assert_eq!(shading.outline_width, 0.0014);
    assert_ne!(shading.line, shading.outline, "strand lines and the outline are separate colours");
}
