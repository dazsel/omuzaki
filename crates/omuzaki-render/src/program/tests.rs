use super::*;

const NONE: &str = "fn program_albedo() {}";

static GLOW: ProgramSpec = ProgramSpec {
    name: "glow",
    params: &[
        ParamSpec::colour("tint", 0x0080_4020),
        ParamSpec::float("strength", 0.5, 0.0, 1.0),
        ParamSpec { name: "centre", default: ParamValue::Vec2(Vec2::new(0.25, 0.75)), range: Some((0.0, 1.0)) },
        ParamSpec { name: "flip", default: ParamValue::Bool(false), range: None },
    ],
    attributes: &[AttributeSpec { name: "glow-uv", components: 2 }],
    toon: |_| Toon::default(),
    wgsl: NONE,
};

static AMBER: ProgramSpec =
    ProgramSpec { name: "amber", params: &[], attributes: &[], toon: |_| Toon::default(), wgsl: NONE };

static AMBER_AGAIN: ProgramSpec = ProgramSpec { name: "amber", ..AMBER };

static GREEDY: ProgramSpec = ProgramSpec {
    name: "greedy",
    params: &[
        ParamSpec::colour("a", 0),
        ParamSpec::colour("b", 0),
        ParamSpec::colour("c", 0),
        ParamSpec::colour("d", 0),
        ParamSpec::colour("e", 0),
        ParamSpec::colour("f", 0),
        ParamSpec::colour("g", 0),
        ParamSpec::colour("h", 0),
        ParamSpec::colour("i", 0),
        ParamSpec::colour("j", 0),
        ParamSpec::colour("k", 0),
    ],
    attributes: &[],
    toon: |_| Toon::default(),
    wgsl: NONE,
};

static WIDE: ProgramSpec = ProgramSpec {
    name: "wide",
    params: &[],
    attributes: &[AttributeSpec { name: "w", components: 5 }],
    toon: |_| Toon::default(),
    wgsl: NONE,
};

fn registry() -> Registry {
    Registry::build(vec![&GLOW, &AMBER]).unwrap()
}

fn shading(name: &str, params: &[(&str, ParamValue)]) -> Shading {
    Shading::Program {
        name: Slug::new(name).unwrap(),
        params: params.iter().map(|(k, v)| (Slug::new(*k).unwrap(), *v)).collect(),
    }
}

#[test]
fn programs_are_sorted_by_name_whatever_order_they_registered_in() {
    let r = Registry::build(vec![&GLOW, &AMBER]).unwrap();
    assert_eq!(r.names().iter().map(Slug::as_str).collect::<Vec<_>>(), ["amber", "glow"]);
    assert_eq!(r.lookup("glow").map(|(i, s)| (i, s.name)), Ok((1, "glow")));
}

#[test]
fn a_duplicate_name_is_a_backend_error_naming_the_program() {
    let Err(RenderError::Backend(message)) = Registry::build(vec![&AMBER, &AMBER_AGAIN]) else {
        panic!("a duplicate must fail")
    };
    assert!(message.contains("\"amber\"") && message.contains("twice"), "{message}");
}

#[test]
fn declarations_that_cannot_work_are_refused_at_registration() {
    for (spec, needle) in [(&GREEDY, "33 floats"), (&WIDE, "5 components")] {
        let Err(RenderError::Backend(message)) = Registry::build(vec![spec]) else { panic!("{} must fail", spec.name) };
        assert!(message.contains(needle), "{message}");
    }
}

#[test]
fn an_unknown_name_gets_did_you_mean_suggestions_and_the_list_of_programs() {
    let r = registry();
    let e = r.lookup("glwo").unwrap_err();
    assert_eq!(e.suggestions, ["glow"]);
    assert_eq!(e.known, ["amber", "glow"]);
    assert_eq!(e.to_string(), "unknown shading program \"glwo\"; did you mean \"glow\"? (known programs: amber, glow)");
    // Nothing close: no suggestion, but the list is still there.
    let far = r.lookup("water").unwrap_err();
    assert_eq!(far.suggestions, Vec::<String>::new());
    assert!(!far.to_string().contains("did you mean"));
    // A prefix of a name is close enough.
    assert_eq!(r.lookup("amb").unwrap_err().suggestions, ["amber"]);
}

#[test]
fn an_unknown_program_is_not_an_error_for_a_material_it_falls_back() {
    assert!(registry().resolve(&shading("glwo", &[])).unwrap().is_none());
    assert!(registry().resolve(&Shading::Unlit).unwrap().is_none());
}

#[test]
fn parameters_pack_in_declaration_order_with_defaults_and_linear_colours() {
    let r = registry();
    let set = [("strength", ParamValue::Float(0.75)), ("flip", ParamValue::Bool(true))];
    let packed = r.resolve(&shading("glow", &set)).unwrap().unwrap().packed;
    let flat: Vec<f32> = packed.iter().flatten().copied().collect();
    let tint = Colour::hex(0x0080_4020).to_linear();
    // tint (3), strength (1), centre (2), flip (1): tightly packed, so `centre` straddles no padding.
    assert_eq!(&flat[..7], &[tint.x, tint.y, tint.z, 0.75, 0.25, 0.75, 1.0]);
    assert!(flat[7..].iter().all(|v| *v == 0.0));
}

#[test]
fn the_shader_gets_the_float_offset_of_every_parameter() {
    assert_eq!(
        offsets(&GLOW),
        "const P_TINT: u32 = 0u;\nconst P_STRENGTH: u32 = 3u;\nconst P_CENTRE: u32 = 4u;\nconst P_FLIP: u32 = 6u;\n"
    );
}

#[test]
fn a_bad_parameter_is_invalid_and_says_what_is_valid() {
    let r = registry();
    let reason = |params: &[(&str, ParamValue)]| match r.resolve(&shading("glow", params)) {
        Err(RenderError::Invalid { reason, .. }) => reason,
        other => panic!("expected Invalid, got {:?}", other.map(|o| o.map(|r| r.index))),
    };
    let unknown = reason(&[("strenght", ParamValue::Float(0.5))]);
    assert!(
        unknown.contains("did you mean \"strength\"") && unknown.contains("tint, strength, centre, flip"),
        "{unknown}"
    );
    let wrong = reason(&[("strength", ParamValue::Bool(true))]);
    assert!(wrong.contains("is a float, not the bool"), "{wrong}");
    let range = reason(&[("strength", ParamValue::Float(1.5))]);
    assert!(range.contains("1.5") && range.contains("0..=1"), "{range}");
    let component = reason(&[("centre", ParamValue::Vec2(Vec2::new(0.5, -0.1)))]);
    assert!(component.contains("-0.1"), "{component}");
}

#[test]
fn edit_distance_counts_insertions_deletions_substitutions_and_swaps() {
    for (a, b, d) in [
        ("", "", 0),
        ("hair", "hair", 0),
        ("hair", "hiar", 1),
        ("hair", "hairs", 1),
        ("", "abc", 3),
        ("kitten", "sitting", 3),
    ] {
        assert_eq!(edit_distance(a, b), d, "{a} -> {b}");
    }
}

#[test]
fn the_hair_program_registers_itself_and_the_renderer_lists_it() {
    let registry = Registry::global().unwrap();
    assert!(registry.names().iter().any(|n| n.as_str() == "hair"));
    assert_eq!(check_shading_program("hair"), Ok(()));
    let e = check_shading_program("hiar").unwrap_err();
    assert_eq!(e.suggestions, ["hair"]);
}

#[test]
fn every_registered_program_declares_the_hooks_and_uses_only_offsets_it_declares() {
    let registry = Registry::global().unwrap();
    for name in registry.names() {
        let spec = registry.lookup(name.as_str()).unwrap().1;
        for hook in ["fn program_albedo(", "fn program_light("] {
            assert!(spec.wgsl.contains(hook), "{}: no `{hook}`", spec.name);
        }
        let prelude = offsets(spec);
        for word in spec.wgsl.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| w.starts_with("P_"))
        {
            assert!(prelude.contains(&format!("const {word}:")), "{}: `{word}` is not a declared parameter", spec.name);
        }
    }
}
