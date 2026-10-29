//! The auto-rig on small synthetic art: the standard parameters exist and do what they say, expressions stand in for
//! the features they replace, the mapping drives the puppet from channels, and a bundle round-trips.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "test pixels")]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_puppet::autorig::autorig;
use omuzaki_puppet::layers::{LayerDef, LayeredArt, LayersManifest};
use omuzaki_puppet::{Driver, Evaluator, bundle};
use omuzaki_puppet_api::{DrawCmd2D, NodeIx};

const W: u32 = 200;
const H: u32 = 300;

/// A full-canvas layer with a filled ellipse.
fn disc(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (u, v) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            if u * u + v * v <= 1.0 {
                px[((y * W + x) * 4) as usize..][..4].copy_from_slice(&[200, 180, 170, 255]);
            }
        }
    }
    px
}

fn layer(name: &str, show_when: Option<&str>) -> LayerDef {
    LayerDef {
        name: name.into(),
        file: name.into(),
        offset: [0, 0],
        role: None,
        side: None,
        depth: None,
        blend: omuzaki_puppet_api::BlendMode::Normal,
        opacity: 1.0,
        show_when: show_when.map(Into::into),
        hide_when: None,
        replaces: None,
    }
}

fn art() -> LayeredArt {
    let names: [(&str, Option<&str>); 9] = [
        ("body", None),
        ("face", None),
        ("eye white L", None),
        ("iris L", None),
        ("eye white R", None),
        ("iris R", None),
        ("mouth", None),
        ("bangs", None),
        ("eyes squeeze", Some("eyesSqueeze")),
    ];
    let manifest = LayersManifest {
        name: "tiny".into(),
        canvas: [W, H],
        artist: None,
        layers: names.iter().map(|(n, s)| layer(n, *s)).collect(),
    };
    LayeredArt::from_manifest(manifest, |file| {
        let px = match file {
            "body" => disc(100.0, 260.0, 90.0, 60.0),
            "face" => disc(100.0, 110.0, 60.0, 70.0),
            "eye white L" => disc(78.0, 110.0, 12.0, 9.0),
            "iris L" => disc(78.0, 110.0, 7.0, 8.0),
            "eye white R" => disc(122.0, 110.0, 12.0, 9.0),
            "iris R" => disc(122.0, 110.0, 7.0, 8.0),
            "mouth" => disc(100.0, 150.0, 10.0, 3.0),
            "bangs" => disc(100.0, 60.0, 64.0, 30.0),
            _ => disc(100.0, 110.0, 40.0, 10.0),
        };
        Ok(([W, H], px))
    })
    .unwrap_or_else(|e| panic!("synthetic art: {e}"))
}

fn node(e: &Evaluator, name: &str) -> NodeIx {
    e.def().node(name).unwrap_or_else(|| panic!("a node {name}"))
}

fn opacity_of_first_vertex(e: &Evaluator, x: f32, y: f32) -> Vec<f32> {
    // The opacity of each mesh whose first vertex is at (x, y).
    let list = e.draw_list();
    list.cmds
        .iter()
        .filter_map(|c| match c {
            DrawCmd2D::Mesh { first, state, .. } => {
                let v = list.vertices[list.indices[*first as usize] as usize].pos;
                ((v[0] - x).abs() < 1e-3 && (v[1] - y).abs() < 1e-3).then_some(state.opacity)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_rig_has_the_standard_parameters_and_they_move_the_right_parts() {
    let rigged = autorig(&art()).unwrap_or_else(|e| panic!("{e}"));
    let mut e = Evaluator::new(rigged.def.clone()).unwrap_or_else(|e| panic!("{e}"));
    for name in [
        "ParamAngleX",
        "ParamAngleY",
        "ParamAngleZ",
        "ParamEyeLOpen",
        "ParamEyeROpen",
        "ParamEyeBallX",
        "ParamMouthOpenY",
        "ParamBreath",
        "Expr eyesSqueeze",
    ] {
        assert!(e.def().param(name).is_some(), "{name}");
    }
    let iris = node(&e, "iris L");
    let rest: Vec<_> = e.points(iris).to_vec();
    let span = |pts: &[omuzaki_core::glam::Vec2]| {
        let (lo, hi) = pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        hi - lo
    };
    // A blink of the picture-left eye (the character's right) flattens its iris.
    let open = e.def().param("ParamEyeROpen").unwrap_or_else(|| panic!("eye param"));
    e.set(open, [0.0, 0.0]);
    e.evaluate();
    assert!(span(e.points(iris)) < span(&rest) * 0.2, "closed: {} against {}", span(e.points(iris)), span(&rest));
    e.reset();
    // A turn to the picture's right slides the face's features right, the far eye less than... both right.
    let angle = e.def().param("ParamAngleX").unwrap_or_else(|| panic!("angle"));
    e.set(angle, [30.0, 0.0]);
    e.evaluate();
    let moved = e.points(iris)[0].x - rest[0].x;
    assert!(moved > 2.0, "the iris moves with the turn: {moved}");
    let body = node(&e, "body");
    e.reset();
    e.evaluate();
    let body_rest = e.points(body)[0];
    e.set(angle, [30.0, 0.0]);
    e.evaluate();
    assert_eq!(e.points(body)[0], body_rest, "the head turn leaves the body");
}

#[test]
fn an_expression_shows_and_hides_the_eyes_it_replaces() {
    let rigged = autorig(&art()).unwrap_or_else(|e| panic!("{e}"));
    let mut e = Evaluator::new(rigged.def.clone()).unwrap_or_else(|e| panic!("{e}"));
    let white = node(&e, "eye white L");
    let squeeze = node(&e, "eyes squeeze");
    let (w0, s0) = (e.points(white)[0], e.points(squeeze)[0]);
    assert_eq!(opacity_of_first_vertex(&e, w0.x, w0.y), vec![1.0]);
    assert_eq!(opacity_of_first_vertex(&e, s0.x, s0.y), vec![0.0]);
    let mut driver = Driver::new(&rigged.def, &rigged.mapping);
    assert!(driver.unresolved.is_empty(), "{:?}", driver.unresolved);
    let mut channels = ChannelBuf::NEUTRAL;
    if let Some(c) = Channel::from_name("eyesSqueeze") {
        channels.set(c, 1.0);
    }
    driver.apply(&channels, &mut e);
    e.evaluate();
    let (w1, s1) = (e.points(white)[0], e.points(squeeze)[0]);
    assert_eq!(opacity_of_first_vertex(&e, w1.x, w1.y), vec![0.0], "the eyes hide");
    assert_eq!(opacity_of_first_vertex(&e, s1.x, s1.y), vec![1.0], "the squeeze shows");
}

#[test]
fn the_mapping_drives_the_head_and_blinks_from_channels() {
    let rigged = autorig(&art()).unwrap_or_else(|e| panic!("{e}"));
    let mut e = Evaluator::new(rigged.def.clone()).unwrap_or_else(|e| panic!("{e}"));
    let mut driver = Driver::new(&rigged.def, &rigged.mapping);
    let mut channels = ChannelBuf::NEUTRAL;
    driver.apply(&channels, &mut e);
    let open = e.def().param("ParamEyeLOpen").unwrap_or_else(|| panic!("eye"));
    assert_eq!(e.value(open)[0], 1.0, "no blink, open eyes");
    for (name, v) in [("eyeBlinkLeft", 1.0), ("headYaw", 0.3)] {
        if let Some(c) = Channel::from_name(name) {
            channels.set(c, v);
        }
    }
    driver.apply(&channels, &mut e);
    assert_eq!(e.value(open)[0], 0.0);
    let angle = e.def().param("ParamAngleX").unwrap_or_else(|| panic!("angle"));
    assert!((e.value(angle)[0] - 0.3f32.to_degrees()).abs() < 0.01);
}

#[test]
fn a_bundle_saves_and_loads_the_same_puppet() {
    let rigged = autorig(&art()).unwrap_or_else(|e| panic!("{e}"));
    let dir = std::env::temp_dir().join(format!("omuzaki-puppet-bundle-{}", std::process::id()));
    bundle::save(&dir, &rigged.def, &rigged.mapping, &rigged.images).unwrap_or_else(|e| panic!("{e}"));
    let loaded = bundle::load(&dir).unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(loaded.def, rigged.def);
    assert_eq!(loaded.mapping, rigged.mapping);
    assert_eq!(loaded.textures.len(), rigged.images.len());
}
