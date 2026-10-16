//! The collar and the tie (#191) on a shirt, on every template and shape.
//!
//! The garment conformance suite builds one garment over the bare body, and these parts need a shirt under them, so the
//! checks that matter for neckwear are made here on the stack body, shirt, collar, tie: nothing inside the body or the
//! shirt, closed manifold meshes with the borders the kinds document, valid skin, names after the part, deterministic
//! bytes. Then the shapes: the collar clears the jaw, the tie lies on the shirt, and the stripe texture is the
//! prototype's.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;
#[path = "support/outfit_render.rs"]
mod render;

use std::collections::HashMap;

use omuzaki_char::outfit::common::LAYER_SHIRT;
use omuzaki_char::outfit::neckwear::collar::{self, CollarParams, CollarStyle};
use omuzaki_char::outfit::neckwear::tie::{self, TieParams};
use omuzaki_char::outfit::shirt::frame::{Column, shirt_shell};
use omuzaki_char::outfit::shirt::{SHIRT_EXPONENT, ShirtParams};
use omuzaki_char_api::{BuildServices, FitFact, KindProvenance, PartCategory, PartDef, PartId, PartKindId, find_kind};
use omuzaki_core::glam::{DVec2, DVec3};
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;
use serde_json::{Value, json};

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn attached(layer: i32, kind: &str, params: Value) -> (&'static str, PartDef) {
    let def = PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer,
        attach: Some(
            serde_json::from_value(json!({ "to": "anchor", "part": "shirt", "anchor": "collar_ring" })).unwrap(),
        ),
        params,
    };
    (if kind == "collar" { "collar" } else { "tie" }, def)
}

/// Body, shirt, collar and tie on `template` with `shape`.
fn stack(
    template: &str,
    shape: &omuzaki_char_api::BodyShape,
    collar: &CollarParams,
    tie: &TieParams,
) -> conformance::Stack {
    let mut def = conformance::character(
        template,
        shape,
        &[("shirt", "shirt", LAYER_SHIRT, serde_json::to_value(ShirtParams::default()).unwrap())],
    );
    for (id, part) in [
        attached(21, "collar", serde_json::to_value(collar).unwrap()),
        attached(22, "tie", serde_json::to_value(tie).unwrap()),
    ] {
        def.parts.insert(PartId::new(id).unwrap(), part);
    }
    conformance::build(def, &services()).unwrap()
}

// ------------------------------------------------------------------------------------------------------ the kinds

#[test]
fn the_kinds_are_project_owned_garments_that_need_a_shirt() {
    for (id, defaults) in [
        ("collar", serde_json::to_value(CollarParams::default()).unwrap()),
        ("tie", serde_json::to_value(TieParams::default()).unwrap()),
    ] {
        let reg = find_kind(id).unwrap();
        let meta = (reg.meta)();
        assert_eq!((meta.category, meta.requires), (PartCategory::Garment, &["shirt"][..]), "{id}");
        assert_eq!((reg.provenance)(), KindProvenance::ProjectOwned);
        assert_eq!((reg.defaults)(), defaults);
    }
}

#[test]
fn bad_numbers_are_refused_naming_their_field() {
    assert!(CollarParams::default().validate().is_ok() && TieParams::default().validate().is_ok());
    let collar = CollarParams {
        style: CollarStyle { stand: f64::NAN, thickness: 0.5, ..CollarStyle::default() },
        ..CollarParams::default()
    };
    let paths: Vec<_> = collar.validate().unwrap_err().0.into_iter().map(|e| e.path).collect();
    assert_eq!(paths, ["style/stand", "style/thickness"]);
    let tie = TieParams { length: 0.0, width: 1.0, ..TieParams::default() };
    let paths: Vec<_> = tie.validate().unwrap_err().0.into_iter().map(|e| e.path).collect();
    assert_eq!(paths, ["length", "width"]);
}

#[test]
fn without_a_shirt_to_hang_on_the_build_says_what_is_missing() {
    let shape = conformance::shapes()[0].1.clone();
    for (kind, attach) in [("collar", None), ("tie", Some(json!({ "to": "bone", "bone": { "humanoid": "neck" } })))] {
        let mut def = conformance::character(
            "soft-sd",
            &shape,
            &[("shirt", "shirt", LAYER_SHIRT, serde_json::to_value(ShirtParams::default()).unwrap())],
        );
        let params = json!({});
        def.parts.insert(
            PartId::new(kind).unwrap(),
            PartDef {
                kind: PartKindId::new(kind).unwrap(),
                parent: None,
                enabled: true,
                layer: 21,
                attach: attach.map(|a| serde_json::from_value(a).unwrap()),
                params,
            },
        );
        let error = conformance::build(def, &services()).err().expect("a refusal").to_string();
        assert!(error.contains("collar_ring"), "{kind}: {error}");
    }
}

// --------------------------------------------------------------------------------------------- the stack's checks

fn welded(positions: &[omuzaki_core::glam::Vec3]) -> Vec<usize> {
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    positions
        .iter()
        .map(|p| {
            let key = [p.x, p.y, p.z].map(|c| (f64::from(c) / 1e-6).round() as i64);
            let next = ids.len();
            *ids.entry(key).or_insert(next)
        })
        .collect()
}

/// The border loops of a mesh after welding, or why it isn't a manifold.
fn loops(mesh: &omuzaki_char_api::PartMesh) -> Result<usize, String> {
    let g = &mesh.geometry;
    let ids = welded(&g.positions);
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
    for t in g.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| ids[i as usize]);
        if a == b || b == c || c == a {
            continue;
        }
        for e in [(a, b), (b, c), (c, a)] {
            *edges.entry(e).or_default() += 1;
        }
    }
    let mut next = HashMap::new();
    for (&(a, b), &n) in &edges {
        if n > 1 {
            return Err(format!("`{}`: edge used {n} times the same way", mesh.name));
        }
        if !edges.contains_key(&(b, a)) && next.insert(a, b).is_some() {
            let p = g.positions.iter().zip(&ids).find(|(_, i)| **i == a).map(|(p, _)| *p);
            return Err(format!("`{}`: pinched border at {p:?}", mesh.name));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut count = 0;
    let mut starts: Vec<usize> = next.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        if !seen.insert(s) {
            continue;
        }
        count += 1;
        let mut at = next[&s];
        while at != s {
            if !seen.insert(at) {
                return Err(format!("`{}`: borders run into each other", mesh.name));
            }
            at = *next.get(&at).ok_or("open border ends")?;
        }
    }
    Ok(count)
}

/// How far outside the column's surface `p` lies, metres (negative inside); infinite above and below the column.
fn outside(column: &Column, p: DVec3) -> f64 {
    if !(column.bottom()..=column.neck_top()).contains(&p.y) {
        return f64::INFINITY;
    }
    let ring = column.ring(p.y, 0.0).unwrap();
    let d = p - ring.centre;
    ring.along_ray(DVec2::new(d.dot(ring.across), d.dot(ring.through))).1
}

#[test]
fn collar_and_tie_stay_off_the_body_and_the_shirt_on_every_template_and_shape() {
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let case = format!("{template}/{label}");
            let stack = stack(template, &shape, &CollarParams::default(), &TieParams::default());
            let fits = &stack.published.get::<FitFact>().fits;
            assert_eq!(fits.len(), 2, "{case}: only the body and the shirt publish a fit");
            let body = Column::new(fits[0].clone(), 2.0).unwrap();
            let shirt = Column::new(shirt_shell(fits[1].clone(), SHIRT_EXPONENT), SHIRT_EXPONENT).unwrap();
            // The collar's inner shell is 2 mm over the shirt at its lower edge. The tie lies on it, its knot's back sunk 0.4 mm
            // into the cloth as the prototype's is (the superellipse and the section ellipse differ a little more).
            for (part, floor) in [("collar", 0.0015), ("tie", -0.0015)] {
                for mesh in &stack.output(part).meshes {
                    let g = &mesh.geometry;
                    let point = |i: u32| g.positions[i as usize].as_dvec3();
                    let mut least = f64::INFINITY;
                    let mut check = |p: DVec3, what: &str| {
                        let (b, s) = (outside(&body, p), outside(&shirt, p));
                        let on_vertex = what.starts_with("vertex");
                        if on_vertex {
                            least = least.min(s);
                        }
                        assert!(
                            b >= 0.0,
                            "{case}: {what} of `{}` is {:.2} mm inside the body at {p}",
                            mesh.name,
                            -b * 1e3
                        );
                        if (mesh.name.ends_with("-shell") || mesh.name.ends_with("-tie")) && on_vertex {
                            let twin = what
                                .strip_prefix("vertex ")
                                .and_then(|k| k.parse::<usize>().ok())
                                .filter(|k| *k >= 1335)
                                .map(|k| outside(&shirt, g.positions[k - 1335].as_dvec3()));
                            assert!(
                                s >= floor - 1e-4,
                                "{case}: {what} of `{}` is {:.2} mm inside the shirt at {p}; its outer twin {:?} m out at {:?}; neck top {} bottom {}",
                                mesh.name,
                                -s * 1e3,
                                twin,
                                what.strip_prefix("vertex ")
                                    .and_then(|k| k.parse::<usize>().ok())
                                    .filter(|k| *k >= 1335)
                                    .map(|k| g.positions[k - 1335]),
                                shirt.neck_top(),
                                shirt.bottom()
                            );
                        }
                    };
                    for (k, p) in g.positions.iter().enumerate() {
                        check(p.as_dvec3(), &format!("vertex {k}"));
                    }
                    // Triangles run straight between vertices, where the shirt curves away (the tie's, 15 mm apart, as the
                    // prototype's do): the vertices are checked against the shirt, the triangles' points only against the body.
                    for (n, t) in g.indices.chunks_exact(3).enumerate() {
                        let [a, b, c] = [point(t[0]), point(t[1]), point(t[2])];
                        for (what, p) in [
                            ("centroid", (a + b + c) / 3.0),
                            ("edge", (a + b) / 2.0),
                            ("edge", (b + c) / 2.0),
                            ("edge", (c + a) / 2.0),
                        ] {
                            check(p, &format!("{what} of triangle {n}"));
                        }
                    }
                    mesh.validate().unwrap();
                    assert!(mesh.geometry.is_skinned());
                    assert!(mesh.name.starts_with(part));
                    println!("{case}: `{}` is at least {:.2} mm off the shirt", mesh.name, least * 1e3);
                }
            }
        }
    }
}

#[test]
fn the_meshes_are_closed_manifolds_with_the_documented_borders() {
    let stack = stack("soft-sd", &conformance::shapes()[0].1, &CollarParams::default(), &TieParams::default());
    let collar_loops: usize = stack.output("collar").meshes.iter().map(|m| loops(m).unwrap()).sum();
    assert_eq!(collar_loops, collar::OPEN_BORDERS);
    let tie_loops: usize = stack.output("tie").meshes.iter().map(|m| loops(m).unwrap()).sum();
    assert_eq!(tie_loops, tie::OPEN_BORDERS);
}

#[test]
fn builds_are_byte_identical_and_everything_is_named_after_its_part() {
    let shape = conformance::shapes()[1].1.clone();
    let a = stack("standard", &shape, &CollarParams::default(), &TieParams::default());
    let b = stack("standard", &shape, &CollarParams::default(), &TieParams::default());
    for part in ["collar", "tie"] {
        let (x, y) = (a.output(part), b.output(part));
        assert_eq!((&x.meshes, &x.materials, &x.textures), (&y.meshes, &y.materials, &y.textures), "{part}");
        for (name, _) in &x.materials {
            assert!(name.as_str().starts_with(part), "{name}");
        }
        for mesh in &x.meshes {
            assert!(x.materials.iter().any(|(m, _)| *m == mesh.material));
        }
    }
    assert_eq!(
        a.output("collar").meshes.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["collar-shell", "collar-lines"]
    );
}

// ------------------------------------------------------------------------------------------------------ the shapes

#[test]
fn the_collar_has_two_halves_that_meet_at_the_back_and_part_in_front() {
    let stack = stack("soft-sd", &conformance::shapes()[0].1, &CollarParams::default(), &TieParams::default());
    let shell = &stack.output("collar").meshes[0].geometry;
    assert_eq!(shell.positions.len(), 2 * 15 * 89, "two shells, each one grid of both halves");
    let front: Vec<DVec3> = shell.positions.iter().map(|p| p.as_dvec3()).filter(|p| p.z > 0.02).collect();
    assert!(front.iter().any(|p| p.x > 0.0) && front.iter().any(|p| p.x < 0.0));
    let gap = front.iter().filter(|p| p.x > 0.0).map(|p| p.x).fold(f64::INFINITY, f64::min);
    assert!(gap > 0.002, "the points leave a V between them in front: {gap}");
    let back_x =
        shell.positions.iter().filter(|p| p.z < -0.05).map(|p| f64::from(p.x.abs())).fold(f64::INFINITY, f64::min);
    assert!(back_x < 1e-4, "the halves meet at the back: {back_x}");
}

#[test]
fn the_tie_lies_on_the_shirts_front_with_the_prototypes_blade_and_knot() {
    let stack = stack("soft-sd", &conformance::shapes()[0].1, &CollarParams::default(), &TieParams::default());
    let tie = &stack.output("tie").meshes[0].geometry;
    assert_eq!(tie.positions.len(), (12 + 16) * 27, "a blade of 12 rings and a knot of 16");
    let fits = &stack.published.get::<FitFact>().fits;
    let shirt = shirt_shell(fits[1].clone(), SHIRT_EXPONENT);
    // The blade at the centre line is 3.8 mm off the shirt: 0.6 mm standoff plus its 3.2 mm thickness.
    let blade = &tie.positions[..12 * 27];
    let ring = blade.chunks(27).nth(3).unwrap();
    let front = ring.iter().map(|p| f64::from(p.z)).fold(f64::NEG_INFINITY, f64::max);
    let y = f64::from(ring[0].y);
    let shirt_front = shirt.section(omuzaki_char_api::BodyRegion::Torso, ((y - 0.325) / 0.22) as f32).unwrap();
    let gap = front - f64::from(shirt_front.centre.z + shirt_front.radii.y);
    assert!((gap - 0.0038).abs() < 3e-4, "the blade's front face is {gap} m off the shirt");
    assert_eq!(tie.uvs.len(), tie.positions.len());
    let (u, v) = (tie.uvs[0].x, tie.uvs[0].y);
    let p = tie.positions[0];
    assert!((f64::from(u) - (f64::from(p.x) + 0.5) / 0.036).abs() < 1e-4, "u from the position");
    assert!((f64::from(v) - (1.0 - f64::from(p.y) / 0.028)).abs() < 1e-3, "v from the height, top-left origin");
}

#[test]
fn the_tie_texture_is_the_prototypes_stripes_at_128_pixels() {
    let stack = stack("soft-sd", &conformance::shapes()[0].1, &CollarParams::default(), &TieParams::default());
    let texture = &stack.output("tie").textures[0].1;
    assert_eq!((texture.desc.size.x, texture.desc.size.y), (128, 128));
    assert_eq!(texture.desc.sampler.wrap, omuzaki_render_api::Wrap::Repeat);
    assert!(texture.desc.mips);
    let at = |x: usize, y: usize| {
        let i = (y * 128 + x) * 4;
        [texture.rgba8[i], texture.rgba8[i + 1], texture.rgba8[i + 2]]
    };
    // The mock rasteriser samples a pixel's centre, so the weave's one-pixel bands (which a canvas weighs by coverage)
    // darken whole columns; the columns between them (x = 1 mod 3) are clean. Top row: the dark edging, the silver
    // stripe, the ground, the thin stripe.
    let exact = |got: [u8; 3], want: [u8; 3]| got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 4);
    assert!(exact(at(10, 0), [0xc9, 0xcc, 0xd3]), "the silver stripe {:?}", at(10, 0));
    assert!(exact(at(4, 0), [0x27, 0x49, 0x3a]), "the edging {:?}", at(4, 0));
    assert!(exact(at(31, 0), [0x35, 0x6a, 0x52]), "the ground {:?}", at(31, 0));
    assert!(exact(at(37, 0), [0xa8, 0xb3, 0xad]), "the thin stripe {:?}", at(37, 0));
    // The stripes slant at 45 degrees: 21 pixels along and 21 down is the same stripe.
    assert!(exact(at(31, 21), at(10, 0)), "slanted 45 degrees");
}

#[test]
fn the_weave_is_drawn_as_one_pixel_bands_for_a_rasteriser_to_weigh_by_coverage() {
    use omuzaki_face_api::{Cmd, PathEl};
    let list = tie::stripes_list(
        omuzaki_core::colour::Colour::rgb(1, 2, 3),
        omuzaki_core::colour::Colour::rgb(4, 5, 6),
        omuzaki_core::colour::Colour::rgb(7, 8, 9),
        omuzaki_core::colour::Colour::rgb(10, 11, 12),
    );
    let fills: Vec<_> = list
        .cmds()
        .iter()
        .filter_map(|c| if let Cmd::Fill { path, opacity, .. } = c { Some((*path, *opacity)) } else { None })
        .collect();
    let weave: Vec<_> = fills.iter().filter(|(_, o)| *o < 1.0).collect();
    assert_eq!(weave.len(), 43, "a line at x = 0, 3, ... 126");
    for (k, (path, opacity)) in weave.iter().enumerate() {
        assert_eq!(*opacity, 0.10);
        let xs: Vec<f32> = list
            .elements(*path)
            .iter()
            .filter_map(|e| match e {
                PathEl::Move(p) | PathEl::Line(p) => Some(p.x),
                _ => None,
            })
            .collect();
        let (lo, hi) = (xs.iter().copied().fold(f32::INFINITY, f32::min), xs.iter().copied().fold(0.0, f32::max));
        let centre = 3.0 * k as f32;
        assert_eq!(
            (lo, hi),
            ((centre - 0.5).max(0.0), centre + 0.5),
            "band {k} straddles x = {centre}, half a pixel either side"
        );
    }
}

#[test]
fn a_longer_wider_tie_and_a_taller_collar_change_the_geometry() {
    let shape = conformance::shapes()[0].1.clone();
    let base = stack("soft-sd", &shape, &CollarParams::default(), &TieParams::default());
    let big = stack(
        "soft-sd",
        &shape,
        &CollarParams { style: CollarStyle { stand: 0.012, ..CollarStyle::default() }, ..CollarParams::default() },
        &TieParams { length: 0.12, width: 0.04, ..TieParams::default() },
    );
    let low = |s: &conformance::Stack| {
        s.output("tie").meshes[0].geometry.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min)
    };
    let wide = |s: &conformance::Stack| {
        s.output("tie").meshes[0].geometry.positions.iter().map(|p| p.x).fold(0.0_f32, f32::max)
    };
    let high = |s: &conformance::Stack| {
        s.output("collar").meshes[0].geometry.positions.iter().map(|p| p.y).fold(0.0_f32, f32::max)
    };
    assert!(low(&big) < low(&base) - 0.02 && wide(&big) > wide(&base) + 0.003 && high(&big) > high(&base) + 0.004);
}

#[test]
fn the_collar_clears_the_chin_for_the_default_head_and_a_dropped_one() {
    // The head part is built over the stack, lowered by the neck's drop; every collar vertex stays a millimetre off it.
    for drop in [0.016, -0.022] {
        let shape = conformance::shapes()[0].1.clone();
        let mut def = conformance::character(
            "soft-sd",
            &shape,
            &[("shirt", "shirt", LAYER_SHIRT, serde_json::to_value(ShirtParams::default()).unwrap())],
        );
        let mut head = omuzaki_char_api::HeadParams::default();
        head.neck.drop = drop;
        def.parts.insert(
            PartId::new("head").unwrap(),
            PartDef {
                kind: PartKindId::new("head").unwrap(),
                parent: None,
                enabled: true,
                layer: 0,
                attach: None,
                params: serde_json::to_value(&head).unwrap(),
            },
        );
        let (id, part) = attached(21, "collar", serde_json::to_value(CollarParams::default()).unwrap());
        def.parts.insert(PartId::new(id).unwrap(), part);
        let stack = conformance::build(def, &services()).unwrap();
        let surface = stack.published.get::<omuzaki_char_api::HeadFact>().clone().expect("a head");
        let mut closest = f64::INFINITY;
        for mesh in &stack.output("collar").meshes {
            for p in &mesh.geometry.positions {
                let d = *p - surface.centre();
                let outside = f64::from(d.length() - surface.radius(d.normalize()));
                closest = closest.min(outside);
            }
        }
        assert!(closest >= 0.001, "head drop {drop}: the collar is {:.2} mm from the head", closest * 1e3);
    }
}

/// Renders body, shirt, collar and tie on every template from four sides when `OMUZAKI_NECKWEAR_PPM` names a folder.
#[test]
fn render_the_neckwear_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_NECKWEAR_PPM") else { return };
    for template in conformance::TEMPLATES {
        let stack = stack(template, &conformance::shapes()[0].1, &CollarParams::default(), &TieParams::default());
        // The collar and the tie, with the shirt's shoulders around them.
        let framing = render::Framing::parts(&stack, &["collar", "tie"]);
        let framing = render::Framing { distance: framing.distance * 1.4, ..framing };
        render::render(&stack, framing, template, directory.as_ref());
    }
}
