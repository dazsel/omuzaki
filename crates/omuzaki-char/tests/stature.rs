//! Real-world stature of the templates: each template stands at its declared height to a millimetre, the
//! body-shape `height` axis multiplies it, and a body published through the real pipeline at that stature is the
//! unscaled one scaled: joints, inverse bind matrices and the bind pose, on the real template skeletons.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_char::template::skeleton::{TemplateFit, joints_at};
use omuzaki_char::template::standard::HEAD;
use omuzaki_char::template::{Template, registry};
use omuzaki_char_api::{
    BodyFit, BodyShape, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, FitFact, HandParams, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta, PartOutput,
    Proportions, Publishes, RegistryBuilder, StatureFact,
};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{Check, FaceProfile, Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const MILLIMETRE: f64 = 1e-3;
const IDS: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

fn template(id: &str) -> &'static dyn Template {
    registry::find(id).unwrap_or_else(|| panic!("template '{id}' is registered"))
}

fn layout_of(t: &dyn Template, height: f64) -> omuzaki_char::template::layout::BodyLayout {
    let shape = BodyShape { height, ..BodyShape::default() };
    t.layout(&Proportions::default(), &shape, &HandParams::default())
}

// ---- a body part that publishes a template the way the body kind will: its fit and its stature

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct TemplateBodyParams {
    template: String,
    height: f64,
}

impl Validate for TemplateBodyParams {
    fn check(&self, _: &mut Check) {}
}

struct TemplateBody;

impl PartKind for TemplateBody {
    type Params = TemplateBodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "stature-template-body", summary: "A body.", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &TemplateBodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let t = registry::find(&p.template).ok_or_else(|| ctx.fail("no such template"))?;
        let layout = layout_of(t, p.height);
        let stature = t.stature(&layout).ok_or_else(|| ctx.fail("no stature"))?;
        Ok(PartOutput {
            publishes: Publishes::default()
                .with::<FitFact>(Arc::new(TemplateFit::new(layout)))
                .with::<StatureFact>(stature),
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(TemplateBody);

fn build(id: &str, height: f64) -> BuiltCharacter {
    let part = PartDef {
        kind: PartKindId::new("stature-template-body").unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: json!({"template": id, "height": height}),
    };
    let def = CharacterDef {
        name: "Test".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new(id).unwrap(),
        seed: 1,
        palette: Palette::default(),
        skin_tone: None,
        parts: BTreeMap::from([(PartId::new("body").unwrap(), part)]),
        face: FaceProfile::default(),
        weight_brushes: BTreeMap::new(),
    };
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

// ---- the numbers

#[test]
fn the_templates_declare_their_statures() {
    // The realistic templates take measured anthropometry (docs/research/2026-10-template-stature.md).
    assert_eq!(template("standard").default_stature_m(), 1.660);
    assert_eq!(template("tall").default_stature_m(), 1.714);
    // The SD templates share soft-sd's head: stature = head x heads.
    let mini = template("mini");
    assert!((mini.default_stature_m() - 3.0 * HEAD).abs() < 1e-12);
}

#[test]
fn soft_sd_builds_at_its_authored_height_with_scale_exactly_one() {
    let t = template("soft-sd");
    assert_eq!(t.stature_scale(), 1.0, "the prototype port is not rescaled");
    let built = build("soft-sd", 0.0);
    let stature = built.stature.expect("the body published its stature");
    assert_eq!(stature.scale, 1.0);
    assert!((f64::from(stature.height_m) - t.authored_height_m()).abs() < 1e-6);
    // Mini's scale is 1 within the template's own 3 head rounding.
    assert!((template("mini").stature_scale() - 1.0).abs() < 0.01);
}

#[test]
fn each_template_builds_at_its_stature_to_a_millimetre() {
    eprintln!("{:<9} {:>6} {:>13} {:>9} {:>11}", "template", "heads", "authored (m)", "scale", "stature (m)");
    for id in IDS {
        let t = template(id);
        let built = build(id, 0.0);
        let stature = built.stature.expect("the body published its stature");
        let (scale, height) = (f64::from(stature.scale), f64::from(stature.height_m));
        eprintln!("{id:<9} {:>6} {:>13.3} {scale:>9.4} {height:>11.4}", t.heads_tall(), t.authored_height_m());
        assert!((height - t.default_stature_m()).abs() < MILLIMETRE, "{id}: {height}");
        assert!((t.authored_height_m() * scale - t.default_stature_m()).abs() < MILLIMETRE, "{id}");
        assert!(
            scale > 0.0 && scale <= 1.0 + 1e-9,
            "{id}: a template is authored up to a little over its real size ({scale})"
        );
    }
}

#[test]
fn the_height_axis_multiplies_the_stature() {
    for id in IDS {
        let t = template(id);
        let neutral = f64::from(build(id, 0.0).stature.unwrap().height_m);
        let (short, tall) =
            (f64::from(build(id, -1.0).stature.unwrap().height_m), f64::from(build(id, 1.0).stature.unwrap().height_m));
        eprintln!("{id}: height axis -1 {short:.3} m, 0 {neutral:.3} m, +1 {tall:.3} m");
        // The body below the head grows 12 %; the head keeps its size, so the whole figure grows a little less.
        assert!(short < neutral && neutral < tall, "{id}");
        assert!((tall / neutral - 1.0) < 0.12 && (tall / neutral - 1.0) > 0.04, "{id}: {}", tall / neutral);
        assert!((1.0 - short / neutral) < 0.12 && (1.0 - short / neutral) > 0.04, "{id}: {}", short / neutral);
        assert!((neutral - t.default_stature_m()).abs() < MILLIMETRE);
    }
}

// ---- the built rig is the authored one scaled

#[test]
fn the_rig_of_each_template_is_the_unscaled_rig_scaled() {
    for id in IDS {
        let t = template(id);
        let built = build(id, 0.0);
        let scale = built.stature.unwrap().scale;
        let layout = layout_of(t, 0.0);
        let authored = omuzaki_char::template::skeleton::joints(&layout);
        let fit = TemplateFit::new(layout);
        // Posing the built skeleton at its bind puts every humanoid joint at the authored position times the scale.
        let placed = joints_at(&built.rig.skeleton, &built.rig.bind);
        assert_eq!(placed.len(), authored.len(), "{id}");
        for ((bone, want), (got_bone, got)) in authored.iter().zip(&placed) {
            assert_eq!(bone, got_bone);
            assert!((*want * scale - *got).length() < 2e-5, "{id} {bone}: {} vs {}", *want * scale, got);
        }
        // The bind rotations are the template's sculpted pose, untouched; the hips offset is zero for these templates.
        assert_eq!(built.rig.bind.rotations, fit.bind().rotations, "{id}: rotations are not lengths");
        assert_eq!(built.rig.bind.hips_offset, Vec3::ZERO);
        // The inverse bind matrices put each bone's bind position at the origin: translation is minus the bone's joint.
        let inverse_bind = built.rig.inverse_bind();
        assert_eq!(inverse_bind.len(), built.rig.skeleton.len());
        let unscaled = omuzaki_char::template::skeleton::of_layout(&layout_of(t, 0.0)).unwrap();
        let reference = omuzaki_char_api::CharacterRig {
            skeleton: unscaled,
            springs: omuzaki_anim_api::SpringSet::default(),
            drivers: Vec::new(),
            bind: fit.bind(),
        }
        .inverse_bind();
        for (i, (s, u)) in inverse_bind.iter().zip(&reference).enumerate() {
            let mut expect = *u;
            expect.w_axis = (u.w_axis.truncate() * scale).extend(1.0);
            let worst =
                s.to_cols_array().iter().zip(expect.to_cols_array()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 2e-5, "{id}: inverse bind {i} differs by {worst}");
        }
    }
}
