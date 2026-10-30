//! The hair stays out of the body after it settles (#2485): every style of the library, on every template, hung at rest
//! by its simulation for three seconds, keeps every point of its locks (still and simulated) out of the body and the
//! head, within the 2 mm the clearance check allows. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason

use omuzaki_char::hair::clearance::{Clearance, TOLERANCE};
use omuzaki_char::hair::styles;
use omuzaki_char::template::registry::templates;
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, CharId, CharacterDef, HairParams, HeadFact, JointRef,
    PartDef, PartId, PartKindId, PoseSource, StepCtx, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use serde_json::json;

/// The character at rest: every joint where it was bound.
struct Rest;

impl PoseSource for Rest {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(Mat4::IDENTITY)
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The deepest point, metres, of `hair` inside the body and the head on `template` after three seconds hanging at rest,
/// and how many points pass the tolerance.
fn settled(template: &str, hair: &HairParams) -> (f32, f32, usize) {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    for p in plan(&def).unwrap() {
        let mut out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template}: {e}"));
        if p.as_str() != "hair" {
            assembler.add(&p, out).unwrap();
            continue;
        }
        let published = assembler.published();
        let head = published.get::<HeadFact>().clone().unwrap();
        let fit = published.outer_fit().cloned();
        let mut points: Vec<Vec3> =
            out.meshes.iter().filter(|m| !m.dynamic).flat_map(|m| m.geometry.positions.iter().copied()).collect();
        if let Some(rt) = out.runtime.as_mut() {
            for frame in 0..180 {
                rt.step(&StepCtx::new(1.0 / 60.0, Seconds(f64::from(frame) / 60.0), &Rest, Vec3::ZERO));
            }
            points.extend(rt.dynamic_mesh(0).unwrap().positions.iter().copied());
        }
        let c = Clearance::new(head.as_ref(), fit.as_deref()).measure(&points);
        return (c.body, c.head, c.points);
    }
    panic!("no hair part was planned")
}

#[test]
fn every_style_on_every_template_settles_clear_of_the_body_and_head() {
    let mut worst = Vec::new();
    for template in templates() {
        for style in styles::list().unwrap() {
            let hair = style.applied_to(&HairParams::default());
            let (body, head, points) = settled(template.id(), &hair);
            println!(
                "{:>12} {:>16}: body {:.1} mm, head {:.1} mm, {points} points",
                template.id(),
                style.id,
                body * 1000.0,
                head * 1000.0
            );
            if points > 0 {
                worst.push(format!(
                    "{} on {}: {:.1} mm into the body, {:.1} mm into the head ({points} points)",
                    style.id,
                    template.id(),
                    body * 1000.0,
                    head * 1000.0
                ));
            }
        }
    }
    assert!(worst.is_empty(), "past the {} mm tolerance:\n{}", TOLERANCE * 1000.0, worst.join("\n"));
}
