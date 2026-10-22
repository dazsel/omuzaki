//! The `measure-flat` light rig (#290, `docs/design/likeness-measures.md` D10): on the GPU, a kit character's face front
//! under it is one tone, with no toon shade band across it, so a `measure.*` op reads the face's shape and not its
//! shading. The face front is the cheeks' band: across the middle of the head, from well under the eye line (the eyes
//! reach below it) to well above the chin, projected through the shot's camera. Its skin pixels are the ones near
//! the region's median hue (the eye whites and the mouth fall outside). The `measure` render preset file is checked
//! here too; `recipe::RenderPreset` reads it for `render.preview` (#376).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "render/support.rs"]
mod support;

/// The keys of the `measure` preset, as design D10 and #290 spell them.
#[test]
fn the_measure_preset_names_the_flat_rig_and_the_keys_of_d10() {
    let text = include_str!("../library/render-presets/measure.toml");
    let preset: toml::Table = toml::from_str(text).expect("the preset parses");
    assert_eq!(preset["name"].as_str(), Some("measure"));
    assert_eq!(preset["light_rig"].as_str(), Some("measure-flat"));
    assert_eq!(preset["expression"].as_str(), Some("neutral"));
    assert_eq!(preset["post"].as_str(), Some("off"));
    assert_eq!(preset["min_iod_px"].as_integer(), Some(160));
    let camera = preset["camera"].as_table().expect("a camera table");
    assert_eq!(camera["framing"].as_str(), Some("face"));
    assert_eq!(camera["projection"].as_str(), Some("orthographic"));
    assert_eq!((camera["yaw"].as_integer(), camera["pitch"].as_integer()), (Some(0), Some(0)));
    let outline = preset["outline"].as_table().expect("an outline table");
    assert_eq!(outline["colour"].as_str(), Some("#1a1a1a"));
    assert_eq!(outline["min_px"].as_float(), Some(1.5));
    assert!(preset["summary"].as_str().is_some_and(|s| s.contains("measure")), "the summary says what it is for");
    let rig = omuzaki_stage::recipe::LightRig::load("measure-flat").expect("the preset's rig is in the library");
    assert!(rig.rim.is_none() && rig.fill.is_none() && rig.point.is_none(), "one key and the ambient, nothing else");
}

/// On lavapipe: the kit's `standard` and `mini` characters at the face framing, 384 px square, under `measure-flat` and
/// under `dev-studio` for comparison. Under `measure-flat` the face front's skin varies by less than 5 in CIELAB `L*`,
/// while the same measure reads a 15% shade band painted onto half of it as more than 5.
/// Set `OMUZAKI_EVIDENCE_DIR` to keep the renders.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
fn under_measure_flat_the_face_front_is_one_tone() {
    use omuzaki_char::ops::character::create::CharacterCreate;
    use omuzaki_char_api::CharId;
    use omuzaki_core::glam::{UVec2, Vec3};
    use omuzaki_core::time::{FakeClock, Seconds};
    use omuzaki_doc_api::DocWrite;
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{Actor, Op, OpCtx};
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_stage::ops::render::job::{RenderQuality, still_light};
    use omuzaki_stage::ops::render::still::{Shot, StillSpec, encode_png, render_body_stills};
    use omuzaki_stage::scene::frame::frame;
    use omuzaki_stage_api::{CameraDef, Framing};
    use serde_json::json;
    use support::{Harness, TestDoc};

    const SIDE: u32 = 384;
    let clock = FakeClock::new(Seconds(0.0));
    let mut doc = MemDoc::from_value(json!({}));
    for template in ["standard", "mini"] {
        let actor = Actor::Person { name: "test".to_owned() };
        let op: CharacterCreate =
            serde_json::from_value(json!({ "id": template, "name": template, "template": template, "seed": 7 }))
                .unwrap();
        let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
        doc.apply(&plan.patch).unwrap();
    }
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = Harness::on(TestDoc(doc), Box::new(renderer));

    // `skin_spread` of the face front of `template` under the light rig `rig`.
    let spread = |template: &str, rig: &str, band: bool| -> (f64, usize) {
        harness.with_job(|job| {
            let body = job
                .read(|doc| {
                    let services = omuzaki_stage::body::BodyServices::new(
                        job.service::<dyn omuzaki_char_api::CharacterBuilder>()?,
                        job.service::<omuzaki_char_api::BuildServices>()?,
                        job.service::<dyn omuzaki_store_api::Store>()?,
                        omuzaki_render_api::MtoonSupport::NONE,
                        &clock,
                    );
                    omuzaki_stage::body::load_body(
                        doc,
                        &services,
                        &CharId::new(template).unwrap(),
                        None,
                        RenderQuality::Offline.detail(),
                    )
                })
                .unwrap();
            let landmarks = body.landmarks().unwrap();
            let camera = frame(Framing::Face, &landmarks, &CameraDef::preset(Framing::Face), 1.0).unwrap();
            let spec = StillSpec::new(still_light(rig).unwrap(), UVec2::splat(SIDE), RenderQuality::Offline.quality());
            let stills = omuzaki_stage::ops::render::job::with_renderer(job, |r| {
                render_body_stills(r, body, &spec, &[Shot::new(camera)])
            })
            .unwrap();
            let image = &stills[0].image;
            if let (false, Some(dir)) = (band, std::env::var_os("OMUZAKI_EVIDENCE_DIR")) {
                let png = encode_png(image).unwrap();
                std::fs::write(std::path::Path::new(&dir).join(format!("measure-flat-{template}-{rig}.png")), png)
                    .unwrap();
            }

            // The face front: across the middle of the head, from well under the eye line to well above the chin (the
            // jaw narrows toward it, so lower down the corners would reach the line), on the head's front.
            let head = landmarks.head.expect("a kit character has a head");
            let (eyes, chin) = (landmarks.eyes.expect("and eyes"), head.min.y);
            let (centre, half) = (head.min.x.midpoint(head.max.x), (head.max.x - head.min.x) / 2.0);
            let to_clip = camera.projection_matrix(1.0) * camera.view();
            let pixel = |p: Vec3| {
                let ndc = to_clip.project_point3(p);
                (ndc.x.midpoint(1.0) * SIDE as f32, (1.0 - ndc.y) / 2.0 * SIDE as f32)
            };
            let height = eyes - chin;
            let (x0, y0) = pixel(Vec3::new(centre - 0.45 * half, eyes - 0.4 * height, head.max.z));
            let (x1, y1) = pixel(Vec3::new(centre + 0.45 * half, chin + 0.4 * height, head.max.z));
            let (x0, x1) = (x0.min(x1).max(0.0) as u32, x0.max(x1).min(SIDE as f32) as u32);
            let (y0, y1) = (y0.min(y1).max(0.0) as u32, y0.max(y1).min(SIDE as f32) as u32);
            assert!(x1 > x0 + 10 && y1 > y0 + 10, "{template}: the face front is in frame ({x0}..{x1}, {y0}..{y1})");

            skin_spread(&image.rgba8, SIDE, (x0, x1, y0, y1), band)
        })
    };

    for template in ["standard", "mini"] {
        let (flat, flat_pixels) = spread(template, "measure-flat", false);
        let (studio, studio_pixels) = spread(template, "dev-studio", false);
        let (control, _) = spread(template, "measure-flat", true);
        println!(
            "{template}: face-front skin L* spread {flat:.2} under measure-flat ({flat_pixels} px), {studio:.2} under \
             dev-studio ({studio_pixels} px), {control:.2} with a 15% shade band painted on"
        );
        assert!(control > 5.0, "{template}: the measure sees a shade band of the skin's hue ({control:.2})");
        assert!(flat_pixels > 500, "{template}: the face front has skin pixels to read ({flat_pixels})");
        assert!(flat < 5.0, "{template}: the face front's skin L* varies by {flat:.2} under measure-flat");
    }
}

/// The spread of the skin `L*` (2nd to 98th percentile) in the region `x0..x1` by `y0..y1` of an RGBA picture `side`
/// pixels across, and how many skin pixels it read. Skin is near the region's median hue and not as dark as the line or
/// the mouth; a shade band of the skin keeps its hue, so it stays in. With `band`, the region's right half is darkened by
/// 15% first: a shade band of the skin's own hue, the control that shows the measure sees one.
#[cfg(feature = "wgpu")]
fn skin_spread(rgba8: &[u8], side: u32, (x0, x1, y0, y1): (u32, u32, u32, u32), band: bool) -> (f64, usize) {
    use omuzaki_measure::colour::lab;

    let mut colours = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let at = ((y * side + x) * 4) as usize;
            let px = &rgba8[at..at + 4];
            if px[3] == 255 {
                let shade = if band && x >= x0.midpoint(x1) { 0.85 } else { 1.0 };
                let [r, g, b] = [0, 1, 2].map(|i| (f32::from(px[i]) * shade).round() as u8);
                colours.push(lab([r, g, b]));
            }
        }
    }
    let median = |i: usize| {
        let mut values: Vec<f64> = colours.iter().map(|c| c[i]).collect();
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    let reference = [median(0), median(1), median(2)];
    let mut skin: Vec<f64> = colours
        .iter()
        .filter(|c| (c[1] - reference[1]).hypot(c[2] - reference[2]) < 12.0 && (c[0] - reference[0]).abs() < 30.0)
        .map(|c| c[0])
        .collect();
    skin.sort_by(f64::total_cmp);
    let at = |q: f64| skin[((skin.len() - 1) as f64 * q).round() as usize];
    (at(0.98) - at(0.02), skin.len())
}
