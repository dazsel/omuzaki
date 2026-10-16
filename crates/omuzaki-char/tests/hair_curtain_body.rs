//! The curtain on a real body: it keeps off the shoulders and arms of every template and body shape, and its first row
//! tucks under the cap's shell on the back of the head. Bodies come from the template registry; heads are ellipsoids
//! placed on each template's neck.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::needless_range_loop)]

use omuzaki_char::hair::compile::{Forked, GroupGeometry, HairGenCtx};
use omuzaki_char::hair::r#gen::cap::fill_shell;
use omuzaki_char::hair::r#gen::curtain::{self, BodyKeepOut, Folds, GUIDE_POINTS};
use omuzaki_char::hair::tube::OutwardFrame;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char_api::{
    BodyFit, BodyParams, BodyRegion, BodyShape, CapGen, CurtainGen, Dynamics, GroupId, HeadParams, HeadSurface, Quality,
};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

/// The section's radius fraction a point must reach: 3 % under the surface (a few millimetres on a shoulder) is the keep-out's interpolation between the 64
/// sections it samples (this check reads 400), which the margin it keeps more than covers.
const CLEAR: f32 = 0.97;
/// What a pushed strand may give back below its push, as a fraction of the section's radius with a 7 mm margin: the
/// measured worst is 0.947, about 5 mm of a shoulder's radius short of the 10 mm clearance it is pushed to.
const GIVE_BACK: f32 = 0.92;
const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// A head ellipsoid from the head part's default cranium, raised to sit on `template`'s neck.
#[derive(Debug)]
struct Head {
    centre: Vec3,
    axes: Vec3,
}

impl HeadSurface for Head {
    fn centre(&self) -> Vec3 {
        self.centre
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / self.axes).length()
    }
}

fn bodies() -> Vec<(&'static str, BodyShape)> {
    vec![
        ("default", BodyShape::default()),
        ("broad", BodyShape { build: 1.0, musculature: 1.0, shoulder_hip: 1.0, ..BodyShape::default() }),
        ("slim", BodyShape { build: -1.0, shoulder_hip: -1.0, ..BodyShape::default() }),
    ]
}

/// The template's body fit and a head on it, and the curtain sized as the sheet's default style has it.
fn setup(template: &str, shape: &BodyShape) -> (TemplateFit, Head) {
    let body = BodyParams::default();
    let find = |name: &str| omuzaki_char::template::registry::find(name).unwrap();
    let layout = find(template).layout(&body.proportions, shape, &body.hand);
    let rise = layout.head_base().unwrap().y
        - find("soft-sd").layout(&body.proportions, &body.shape, &body.hand).head_base().unwrap().y;
    let c = HeadParams::default().cranium;
    let head = Head {
        centre: Vec3::new(c.centre[0] as f32, (c.centre[1] + rise) as f32, c.centre[2] as f32),
        axes: Vec3::new(
            c.half_width as f32,
            (0.5 * (c.half_height_up + c.half_height_down_back)) as f32,
            (0.5 * (c.half_depth_front + c.half_depth_back)) as f32,
        ),
    };
    (TemplateFit::new(layout), head)
}

fn compile(
    g: &CurtainGen,
    head: &Head,
    fit: Option<&dyn BodyFit>,
    seed: u64,
) -> (Vec<omuzaki_char_api::HairClump>, Vec<f32>) {
    let (palette, id, dynamics) = (Palette::default(), GroupId::new("curtain").unwrap(), Dynamics::default());
    let mut forked = Rng::new(seed);
    let mut stream = Forked(&mut forked);
    let mut ctx = HairGenCtx::new(head, &palette, Quality::Live, &id, &dynamics, &mut stream).with_body(fit);
    let group = curtain::compile(g, &mut ctx);
    let GroupGeometry::Strip { u, .. } = group.geometry else { panic!("a strip") };
    (group.clumps, u)
}

fn mesh(g: &CurtainGen, head: &Head, fit: &dyn BodyFit, seed: u64) -> (Vec<omuzaki_char_api::HairClump>, MeshData) {
    let (clumps, u) = compile(g, head, Some(fit), seed);
    let frame = OutwardFrame { head: head.centre, body_axis: Vec2::new(head.centre.x, -0.03) };
    let keep = BodyKeepOut::new(fit);
    let mesh = curtain::group_mesh(&clumps, &u, g.columns, &frame, Folds::Prototype, Some(&keep)).unwrap();
    (clumps, mesh)
}

/// A body's regions sampled at 400 sections each, for [`rho_in_body`].
fn sections_of(fit: &dyn BodyFit) -> Vec<omuzaki_char_api::Section> {
    [BodyRegion::Torso, BodyRegion::ArmL, BodyRegion::ArmR, BodyRegion::LegL, BodyRegion::LegR]
        .into_iter()
        .flat_map(|region| (0..=400).filter_map(move |i| Some((region, i as f32 / 400.0))))
        .filter_map(|(region, t)| fit.section(region, t))
        .collect()
}

/// How far inside the nearest body section `p` is, as the ellipse's `rho` (below 1 is inside), by brute force over
/// `sections` and with the section's own frame: independent of the keep-out's segment search.
fn rho_in_body(sections: &[omuzaki_char_api::Section], p: Vec3, margin: f32) -> f32 {
    let mut worst = f32::INFINITY;
    for s in sections {
        let d = p - s.centre;
        // Only the slice this section stands for: within a step of its plane.
        if d.dot(s.axis).abs() > 0.004 {
            continue;
        }
        let across = (Vec3::X - s.axis * s.axis.x).normalize_or(Vec3::Z);
        let through = s.axis.cross(across);
        let r = s.radii + Vec2::splat(margin);
        worst = worst.min((Vec2::new(d.dot(across), d.dot(through)) / r).length());
    }
    worst
}

#[test]
fn rest_points_and_strip_vertices_stay_out_of_the_body_on_every_template_and_shape() {
    let g = CurtainGen::default();
    let mut checked = 0;
    for template in TEMPLATES {
        for (shape_name, shape) in bodies() {
            let (fit, head) = setup(template, &shape);
            let (clumps, strips) = mesh(&g, &head, &fit, 3);
            let sections = sections_of(&fit);
            let mut inside = Vec::new();
            for (n, c) in clumps.iter().enumerate() {
                for (k, p) in c.rest.iter().enumerate() {
                    let rho = rho_in_body(&sections, *p, 0.0);
                    if rho < CLEAR {
                        inside.push(format!("guide {n} point {k} {p} rho {rho}"));
                    }
                    checked += 1;
                }
            }
            for (v, p) in strips.positions.iter().enumerate() {
                let rho = rho_in_body(&sections, *p, 0.0);
                if rho < CLEAR {
                    inside.push(format!("strip vertex {v} {p} rho {rho}"));
                }
                checked += 1;
            }
            assert!(
                inside.is_empty(),
                "{template}/{shape_name}: {} inside the body, e.g. {:?}",
                inside.len(),
                &inside[..inside.len().min(4)]
            );
        }
    }
    assert!(checked > 80_000, "the check looked at {checked} points");
}

#[test]
fn the_keep_out_moves_the_hair_where_the_body_is_in_the_way_and_nowhere_else() {
    let g = CurtainGen::default();
    // Broad standard: the shoulders are in the way, so the hair differs from the bare-head build.
    let (fit, head) = setup("standard", &bodies()[1].1);
    let (with, _) = compile(&g, &head, Some(&fit), 3);
    let (without, _) = compile(&g, &head, None, 3);
    let moved = with.iter().zip(&without).flat_map(|(a, b)| a.rest.iter().zip(&b.rest)).filter(|(a, b)| a != b).count();
    assert!(moved > 0, "a broad standard body is in the way of the hair");
    // The keep-out leaves a clear point alone, whatever the margin up to the gap.
    let keep = BodyKeepOut::new(&fit);
    let far = head.centre + Vec3::new(2.0, 0.0, 0.0);
    assert_eq!(keep.push_out(far, 0.01), far);
    assert_eq!(keep.push_out(Vec3::NAN, 0.01).is_nan(), Vec3::NAN.is_nan());
}

#[test]
fn a_pushed_point_ends_on_the_surface_plus_the_margin() {
    let (fit, _) = setup("standard", &BodyShape::default());
    let keep = BodyKeepOut::new(&fit);
    let s = fit.section(BodyRegion::Torso, 0.5).unwrap();
    let inside = s.centre + Vec3::X * (0.5 * s.radii.x);
    let out = keep.push_out(inside, 0.005);
    assert!(out.x > inside.x, "pushed outward across the body: {inside} to {out}");
    assert!((rho_in_body(&sections_of(&fit), out, 0.005) - 1.0).abs() < 0.03, "it ends on the margin's ellipse");
}

/// The distance from `p` to the nearest point of the triangle `abc`.
fn distance_to_triangle(p: Vec3, [a, b, c]: [Vec3; 3]) -> f32 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return ap.length();
    }
    let n = ab.cross(ac);
    let area = n.length_squared();
    if area < 1e-18 {
        return ap.length();
    }
    // Project onto the plane and clamp into the triangle by barycentric coordinates; fall back to the edges.
    let (u, v) = (ap.cross(ac).dot(n) / area, ab.cross(ap).dot(n) / area);
    if u >= 0.0 && v >= 0.0 && u + v <= 1.0 {
        return (ap.dot(n) / area.sqrt()).abs();
    }
    let edge = |p0: Vec3, p1: Vec3| {
        let t = ((p - p0).dot(p1 - p0) / (p1 - p0).length_squared()).clamp(0.0, 1.0);
        p.distance(p0 + (p1 - p0) * t)
    };
    edge(a, b).min(edge(b, c)).min(edge(c, a))
}

fn gap_to_cap(shell: &MeshData, p: Vec3) -> f32 {
    shell
        .indices
        .chunks(3)
        .map(|t| distance_to_triangle(p, [t[0], t[1], t[2]].map(|i| shell.positions[i as usize])))
        .fold(f32::INFINITY, f32::min)
}

/// The first row of the curtain's strips along the back: the worst distance of any vertex to the cap's shell.
fn first_row_gap(g: &CurtainGen, template: &str) -> f32 {
    let (fit, head) = setup(template, &BodyShape::default());
    let mut shell = MeshData::default();
    fill_shell(&CapGen::default(), &head, &mut shell);
    let (clumps, strips) = mesh(g, &head, &fit, 3);
    let columns = usize::from(g.columns);
    let mut worst = 0.0_f32;
    for (n, _) in clumps.iter().enumerate() {
        // The back: guides within 60 degrees of straight behind (the guides spread 118 degrees each way).
        let u = n as f32 / (clumps.len() - 1) as f32;
        if (u - 0.5).abs() * 2.0 * g.spread_deg as f32 > 60.0 {
            continue;
        }
        for c in 0..columns {
            let v = (n * columns + c) * GUIDE_POINTS;
            worst = worst.max(gap_to_cap(&shell, strips.positions[v]));
        }
    }
    worst
}

#[test]
fn the_curtains_first_row_tucks_under_the_cap_along_the_back_on_every_template() {
    let tucked = CurtainGen::default();
    let untucked = CurtainGen { root_tuck_deg: 0.0, ..CurtainGen::default() };
    for template in TEMPLATES {
        let (with, without) = (first_row_gap(&tucked, template), first_row_gap(&untucked, template));
        eprintln!("{template}: first row to cap, worst gap {with:.4} m tucked, {without:.4} m untucked");
        assert!(with < 0.006, "{template}: the first row stands {with} m from the cap");
        assert!(without > with, "{template}: the tuck is what closes the gap ({without} m without it)");
    }
}

/// The chest's frontal plane: the furthest forward the torso reaches over its upper half.
fn chest_front(fit: &dyn BodyFit) -> f32 {
    (0..=40)
        .filter_map(|i| fit.section(BodyRegion::Torso, 0.5 + 0.5 * i as f32 / 40.0))
        .map(|s| s.centre.z + s.radii.y)
        .fold(f32::MIN, f32::max)
}

/// Where the hair has finished settling behind the arms: 0.6 head half-widths below the head's lowest point.
fn settled_below(head: &Head) -> f32 {
    below_head(head) - 0.6 * head.axes.x
}

/// The head's lowest point: hair above it is the face's, not the fall's.
fn below_head(head: &Head) -> f32 {
    head.centre.y - head.radius(Vec3::NEG_Y)
}

#[test]
fn the_fall_hangs_behind_the_chest_and_the_arms_on_every_template_and_shape() {
    let g = CurtainGen::default();
    let front = CurtainGen { in_front: true, ..CurtainGen::default() };
    for template in TEMPLATES {
        for (shape_name, shape) in bodies() {
            let (fit, head) = setup(template, &shape);
            let plane = chest_front(&fit);
            let reach = |g: &CurtainGen| {
                let (clumps, _) = compile(g, &head, Some(&fit), 3);
                clumps
                    .iter()
                    .flat_map(|c| c.rest.iter())
                    .filter(|p| p.y < settled_below(&head))
                    .map(|p| p.z)
                    .fold(f32::MIN, f32::max)
            };
            let behind = reach(&g);
            assert!(behind <= plane, "{template}/{shape_name}: the fall reaches z {behind}, past the chest at {plane}");
            eprintln!(
                "{template}/{shape_name}: furthest z {behind:.3} (chest {plane:.3}); in front {:.3}",
                reach(&front)
            );
        }
    }
    // The switch does something: asked to hang in front, the hair reaches well further forward than behind the arms.
    for template in ["standard", "tall"] {
        let (fit, head) = setup(template, &BodyShape::default());
        let reach = |g: &CurtainGen| {
            let (clumps, _) = compile(g, &head, Some(&fit), 3);
            clumps
                .iter()
                .flat_map(|c| c.rest.iter())
                .filter(|p| p.y < settled_below(&head))
                .map(|p| p.z)
                .fold(f32::MIN, f32::max)
        };
        assert!(reach(&front) > reach(&g) + 0.05, "{template}: in_front changes nothing");
    }
}

#[test]
fn a_strand_never_comes_back_toward_the_body_below_where_it_was_pushed() {
    let g = CurtainGen { in_front: true, ..CurtainGen::default() }; // hair over the chest, so strands are pushed there
    let mut pushed_strands = 0;
    for template in TEMPLATES {
        for (shape_name, shape) in bodies() {
            let (fit, head) = setup(template, &shape);
            let sections = sections_of(&fit);
            let (clumps, _) = compile(&g, &head, Some(&fit), 3);
            let (bare, _) = compile(&g, &head, None, 3);
            for (n, (c, b)) in clumps.iter().zip(&bare).enumerate() {
                let Some(first) = c.rest.iter().zip(&b.rest).position(|(p, q)| p.distance(*q) > 0.001) else {
                    continue;
                };
                pushed_strands += 1;
                for (k, p) in c.rest.iter().enumerate().skip(first) {
                    // A few millimetres inside the margin it was pushed to is the most it may give back.
                    let rho = rho_in_body(&sections, *p, 0.007);
                    assert!(
                        rho >= GIVE_BACK,
                        "{template}/{shape_name}: guide {n} was pushed at point {first} and is {rho} of the way out at {k}"
                    );
                }
            }
        }
    }
    assert!(pushed_strands > 0, "no strand was pushed, so nothing was checked");
}
