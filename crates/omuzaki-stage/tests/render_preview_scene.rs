//! `render.preview` in a scene (#416, design scenes D5, D9, D10): the character stands alone in a copy of a stored
//! scene, in its subject's place, lit, backed and grounded as the scene is, and shot from one of its cameras or a preset
//! framing, optionally under another light rig. The document never changes. On the GPU: the kit's four body templates
//! in the default scene against the light-rig path it replaces, and every library rig.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use omuzaki_stage::scene::alone::{ShotChoice, cast_alone};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{Anchor, CameraId, Framing, MemberId, SceneId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use support::{Harness, TestDoc, mock_doc};

/// Plans `args` as op `T` on `doc` and applies its patch.
fn apply<T: Op + DeserializeOwned>(doc: &mut MemDoc, args: Value) {
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args).expect("the arguments read");
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{}: {e:?}", T::meta().name));
    doc.apply(&plan.patch).expect("applies");
}

/// The mock character in a project with two scenes: `main` (the default, casting it as `mock`, the studio rig) and
/// `night` (a transparent backdrop, no ground, the candle rig, opening on a `close` camera).
fn project() -> MemDoc {
    use omuzaki_stage::ops::scene::camera::{AddCamera, SetActiveCamera};
    use omuzaki_stage::ops::scene::create::CreateScene;
    use omuzaki_stage::ops::scene::light::SetLightRig;
    use omuzaki_stage::ops::scene::look::{SetBackdrop, SetGround};

    let TestDoc(mut doc) = mock_doc();
    apply::<CreateScene>(&mut doc, json!({ "id": "main", "cast": ["mock"] }));
    apply::<CreateScene>(&mut doc, json!({ "id": "night", "cast": [] }));
    apply::<SetBackdrop>(&mut doc, json!({ "scene": "night", "kind": "transparent" }));
    apply::<SetGround>(&mut doc, json!({ "scene": "night", "kind": "none" }));
    apply::<SetLightRig>(&mut doc, json!({ "scene": "night", "rig": "viewer-candle" }));
    apply::<AddCamera>(&mut doc, json!({ "scene": "night", "id": "close", "framing": "face", "yaw": -20.0 }));
    apply::<SetActiveCamera>(&mut doc, json!({ "scene": "night", "camera": "close" }));
    doc
}

fn run(harness: &Harness, args: Value) -> Result<PreviewResult, OpError> {
    let op: Preview = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    assert!(plan.patch.is_empty(), "a preview changes nothing in the document");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

#[test]
fn a_preview_stands_in_the_opening_scene_by_default_and_the_same_call_gives_the_same_bytes() {
    // A job reads the document and its plan's patch is empty (checked in `run`), so the document stays as it was.
    let harness = Harness::new(TestDoc(project()));
    let args = json!({ "character": "mock", "size": [64, 96], "quality": "offline" });
    let first = run(&harness, args.clone()).unwrap();
    assert_eq!((first.scene.as_deref(), first.camera.as_str()), (Some("main"), "full"), "the opening scene and camera");
    let again = run(&harness, args).unwrap();
    assert_eq!(first.hash, again.hash, "the same document and arguments give the same bytes");

    let night = run(&harness, json!({ "character": "mock", "scene": "night", "size": [64, 96] })).unwrap();
    assert_eq!((night.scene.as_deref(), night.camera.as_str()), (Some("night"), "close"));
    assert_ne!(night.hash, first.hash, "another scene, another picture");
}

#[test]
fn a_project_without_scenes_previews_in_the_built_in_default_scene() {
    let harness = Harness::new(mock_doc());
    let result = run(&harness, json!({ "character": "mock", "size": [64, 96] })).unwrap();
    assert_eq!((result.scene, result.camera.as_str()), (None, "full"));
    let framed = run(&harness, json!({ "character": "mock", "framing": "bust", "size": [64, 96] })).unwrap();
    assert_eq!(framed.camera, "shot", "a framing without a camera is a fresh camera from the front");
}

#[test]
fn camera_framing_and_light_rig_are_checked_before_the_render() {
    let harness = Harness::new(TestDoc(project()));
    let refused = |args: Value| run(&harness, args).expect_err("refused");
    let e = refused(json!({ "character": "mock", "camera": "nope" }));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("camera")));
    assert!(e.message.contains("full"), "names the cameras it has: {}", e.message);
    let e = refused(json!({ "character": "mock", "framing": "free" }));
    assert_eq!(e.field.as_deref(), Some("framing"));
    let e = refused(json!({ "character": "mock", "light_rig": "disco" }));
    assert_eq!(e.field.as_deref(), Some("light_rig"));
    let e = refused(json!({ "character": "mock", "scene": "nowhere" }));
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = refused(json!({ "character": "nobody" }));
    assert_eq!(e.code, ErrorCode::NotFound);

    let rigged = run(
        &harness,
        json!({ "character": "mock", "light_rig": "viewer-moonlit", "camera": "bust", "size": [64, 96] }),
    )
    .unwrap();
    assert_eq!(rigged.camera, "bust");
}

#[test]
fn alone_takes_the_subjects_place_and_its_anchors() {
    let mut doc = project();
    apply::<omuzaki_stage::ops::scene::cast::PlaceCharacter>(
        &mut doc,
        json!({ "character": "mock", "id": "guest", "position": [1.5, 0.3], "yaw": 40.0 }),
    );
    apply::<omuzaki_stage::ops::scene::cast::SetSubject>(&mut doc, json!({ "member": "guest" }));
    apply::<omuzaki_stage::ops::scene::light::SetLight>(&mut doc, json!({ "light": "rim", "anchor": "member:mock" }));
    let (_, mut def) = scene_for(&doc, Some(&SceneId::new("main").unwrap())).unwrap();
    let member = cast_alone(&mut def, &CharId::new("mock").unwrap(), None).unwrap();
    assert_eq!(member, MemberId::new("guest").unwrap(), "the subject's id");
    assert_eq!(def.cast.len(), 1);
    let stands = &def.cast[&member];
    assert_eq!((stands.position, stands.yaw), ([1.5, 0.3], 40.0), "the subject's place");
    assert_eq!(def.lighting.lights[&"rim".parse().unwrap()].anchor, Anchor::Subject, "another member's light follows");
    def.check().expect("the copy keeps the rules");

    let mut choice = ShotChoice::default();
    choice.camera = Some(CameraId::new("bust").unwrap());
    choice.framing = Some(Framing::Face);
    choice.yaw = Some(15.0);
    let id = choice.apply(&mut def).unwrap();
    let camera = &def.cameras[&id];
    assert_eq!((camera.framing, camera.yaw), (Framing::Face, 15.0));
    assert_eq!(def.scene.camera.as_ref(), Some(&id));
}

/// On lavapipe: contact sheet 1 is the kit's four body templates at `full`, `bust` and `face`, each the light-rig path
/// `render.preview` took before (left) beside the default scene it stands in now (right); contact sheet 2 is every
/// library rig through `light_rig` on `soft-sd` and `standard`. Set `OMUZAKI_EVIDENCE_DIR` to keep the sheets.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
fn on_the_gpu_the_default_scene_and_every_rig() {
    use omuzaki_char::ops::character::create::CharacterCreate;
    use omuzaki_core::glam::UVec2;
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
    use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, RenderQuality, still_light, store_png};
    use omuzaki_stage::ops::render::still::{Shot, StillSpec, encode_png, render_body_stills};
    use omuzaki_stage::recipe::LightRig;
    use omuzaki_stage::scene::frame::frame;
    use omuzaki_stage_api::CameraDef;
    use omuzaki_store_api::Store;

    const SIZE: [u32; 2] = [192, 256];
    const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];
    let mut doc = MemDoc::from_value(json!({}));
    for template in TEMPLATES {
        let id = template.replace('-', "_");
        apply::<CharacterCreate>(&mut doc, json!({ "id": id, "name": template, "template": template, "seed": 7 }));
    }
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = Harness::on(TestDoc(doc), Box::new(renderer));
    let sheet = |uris: Vec<String>, columns: usize| -> ContactSheetResult {
        let op: ContactSheet = serde_json::from_value(json!({ "images": uris, "columns": columns })).unwrap();
        serde_json::from_value(harness.run(&op).expect("lays out").1).unwrap()
    };
    let keep = |name: &str, hash: &str| {
        if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
            let png = harness.store.get(&hash.parse().unwrap()).unwrap();
            std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), png).unwrap();
        }
    };

    // Sheet 1: before (the rig alone, the character at the origin, no ground) and after (the default scene).
    let clock = FakeClock::new(Seconds(0.0));
    let mut cells = Vec::new();
    for template in TEMPLATES {
        let id = template.replace('-', "_");
        for framing in ["full", "bust", "face"] {
            let before = harness.with_job(|job| {
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
                            &CharId::new(&id).unwrap(),
                            None,
                            RenderQuality::Offline.detail(),
                        )
                    })
                    .unwrap();
                let landmarks = body.landmarks().unwrap();
                let framing: Framing = serde_json::from_value(json!(framing)).unwrap();
                let camera = frame(framing, &landmarks, &CameraDef::preset(framing), 0.75).unwrap();
                let spec = StillSpec::new(
                    still_light(DEFAULT_LIGHT_RIG).unwrap(),
                    UVec2::from(SIZE),
                    RenderQuality::Offline.quality(),
                );
                let stills = omuzaki_stage::ops::render::job::with_renderer(job, |r| {
                    render_body_stills(r, body, &spec, &[Shot::new(camera)])
                })
                .unwrap();
                store_png(job, &encode_png(&stills[0].image).unwrap()).unwrap().uri
            });
            let after =
                run(&harness, json!({ "character": id, "framing": framing, "size": SIZE, "quality": "offline" }))
                    .unwrap();
            cells.push(before);
            cells.push(after.uri);
        }
    }
    let one = sheet(cells, 6);
    keep("preview-default-scene-sheet", &one.hash);

    // Sheet 2: every rig on soft-sd and standard, bust, with few clipped pixels on the character.
    let mut cells = Vec::new();
    for rig in LightRig::names() {
        for id in ["soft_sd", "standard"] {
            let shot = run(
                &harness,
                json!({ "character": id, "light_rig": rig, "framing": "bust", "size": SIZE, "quality": "offline" }),
            )
            .unwrap();
            let png = harness.store.get(&shot.hash.parse().unwrap()).unwrap();
            let image = image::load_from_memory(&png).unwrap().to_rgba8();
            let corner = *image.get_pixel(0, 0);
            let figure: Vec<_> =
                image.pixels().filter(|p| p.0.iter().zip(corner.0).any(|(a, b)| a.abs_diff(b) > 8)).collect();
            let clipped = figure.iter().filter(|p| p.0[..3].iter().all(|c| *c == 255)).count();
            let share = clipped as f64 / figure.len().max(1) as f64;
            println!("{rig} on {id}: {} figure pixels, {clipped} clipped white ({share:.4})", figure.len());
            assert!(figure.len() > (SIZE[0] * SIZE[1]) as usize / 10, "{rig} on {id}: the character shows");
            assert!(share < 0.01, "{rig} on {id}: {share:.4} of the character is clipped white");
            cells.push(shot.uri);
        }
    }
    let two = sheet(cells, 2);
    keep("preview-light-rigs-sheet", &two.hash);
}

/// On lavapipe (#931, LK-08): soft-sd and tall in the studio rig, then with fog, then with a sky environment as the
/// backdrop, then with a spot as well, full and bust. Fog and the spot are in subject units, so both statures take the
/// same share of fog and the same light. Set `OMUZAKI_EVIDENCE_DIR` to keep the sheet.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence image is written beside the test, not by an op")]
fn on_the_gpu_fog_a_sky_environment_and_a_spot_on_two_statures() {
    use omuzaki_char::ops::character::create::CharacterCreate;
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
    use omuzaki_stage::ops::scene::create::CreateScene;
    use omuzaki_stage::ops::scene::light::{AddLight, SetLightRig, SetLighting};
    use omuzaki_stage::ops::scene::look::SetBackdrop;
    use omuzaki_store_api::Store;

    const SIZE: [u32; 2] = [192, 256];
    const LOOKS: [&str; 4] = ["studio", "fog", "sky", "spot"];
    let mut doc = MemDoc::from_value(json!({}));
    for template in ["soft-sd", "tall"] {
        let id = template.replace('-', "_");
        apply::<CharacterCreate>(&mut doc, json!({ "id": id, "name": template, "template": template, "seed": 7 }));
    }
    // Each look adds one thing to the one before it.
    let fog = json!({ "fog": { "colour": "#2a2238", "near": 0.8, "far": 5.0 } });
    let sky = json!({ "environment": { "kind": "sky", "intensity": 0.35 } });
    let spot = json!({ "kind": "spot", "role": "accent", "position": [-1.0, 1.2, 0.3], "target": [0.0, 0.8, 0.0],
                       "cone": 25.0, "penumbra": 0.4, "intensity": 1.2, "colour": "#ffc890" });
    for (i, look) in LOOKS.iter().enumerate() {
        apply::<CreateScene>(&mut doc, json!({ "id": look, "cast": [] }));
        apply::<SetLightRig>(&mut doc, json!({ "scene": look, "rig": "viewer-studio" }));
        if i >= 1 {
            let mut args = fog.clone();
            args["scene"] = json!(look);
            apply::<SetLighting>(&mut doc, args);
        }
        if i >= 2 {
            let mut args = sky.clone();
            args["scene"] = json!(look);
            apply::<SetLighting>(&mut doc, args);
            apply::<SetBackdrop>(&mut doc, json!({ "scene": look, "kind": "environment" }));
        }
        if i >= 3 {
            let mut args = spot.clone();
            args["scene"] = json!(look);
            apply::<AddLight>(&mut doc, args);
        }
    }
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = Harness::on(TestDoc(doc), Box::new(renderer));
    let image = |hash: &str| {
        let png = harness.store.get(&hash.parse().unwrap()).unwrap();
        image::load_from_memory(&png).unwrap().to_rgba8()
    };

    let mut cells = Vec::new();
    for id in ["soft_sd", "tall"] {
        for framing in ["full", "bust"] {
            let shots: Vec<_> = LOOKS
                .iter()
                .map(|look| {
                    run(
                        &harness,
                        json!({ "character": id, "scene": look, "framing": framing, "size": SIZE, "quality": "offline" }),
                    )
                    .unwrap_or_else(|e| panic!("{id} {framing} in {look}: {e:?}"))
                })
                .collect();
            let [studio, fogged, sky, spot] = [0, 1, 2, 3].map(|i| image(&shots[i].hash));
            let differ = |a: &image::RgbaImage, b: &image::RgbaImage| {
                a.pixels().zip(b.pixels()).filter(|(p, q)| p.0.iter().zip(q.0).any(|(x, y)| x.abs_diff(y) > 6)).count()
            };
            let all = (SIZE[0] * SIZE[1]) as usize;
            let (by_fog, by_sky, by_spot) = (differ(&studio, &fogged), differ(&fogged, &sky), differ(&sky, &spot));
            println!("{id} {framing}: fog changes {by_fog}, the sky {by_sky}, the spot {by_spot} of {all} pixels");
            // Fog starts 0.8 subject heights from the camera, which a bust of a tall figure is nearer than.
            if framing == "full" {
                assert!(by_fog > all / 20, "{id} {framing}: the fog shows");
            }
            assert!(by_sky > all / 4, "{id} {framing}: the sky shows behind the character");
            assert!(by_spot > all / 200, "{id} {framing}: the spot lights the character");
            // The backdrop is opaque sky, not the rig's background colour.
            let (top, before) = (sky.get_pixel(SIZE[0] / 2, 0).0, fogged.get_pixel(SIZE[0] / 2, 0).0);
            assert!(top[3] == 255 && top != before, "{id} {framing}: the sky is behind, got {top:?} for {before:?}");
            cells.extend(shots.into_iter().map(|s| s.uri));
        }
    }
    let op: ContactSheet = serde_json::from_value(json!({ "images": cells, "columns": LOOKS.len() })).unwrap();
    let sheet: ContactSheetResult = serde_json::from_value(harness.run(&op).expect("lays out").1).unwrap();
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let png = harness.store.get(&sheet.hash.parse().unwrap()).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("scene-fog-sky-spot-sheet.png"), png).unwrap();
    }
}

/// A synthetic 4:3 room for backdrop tests: a wall darkening upward, a window of four panes with a sky gradient, and a
/// floor band; drawn here, so it is the project's own.
fn synthetic_room() -> Vec<u8> {
    let (width, height) = (400u32, 300u32);
    let picture = image::RgbaImage::from_fn(width, height, |column, row| {
        let (across, down) = (column as f32 / width as f32, row as f32 / height as f32);
        let mix =
            |from: [f32; 3], to: [f32; 3], at: f32| [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * at.clamp(0.0, 1.0));
        let window = (0.55..0.85).contains(&across) && (0.15..0.6).contains(&down);
        let mullion = window && ((across - 0.70).abs() < 0.008 || (down - 0.375).abs() < 0.01);
        let colour = if down > 0.78 {
            [120.0, 84.0, 60.0]
        } else if mullion {
            [235.0, 230.0, 220.0]
        } else if window {
            mix([110.0, 150.0, 220.0], [250.0, 210.0, 170.0], (down - 0.15) / 0.45)
        } else {
            mix([70.0, 64.0, 92.0], [182.0, 170.0, 190.0], down / 0.78)
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // every channel is within 0..=255
        let [red, green, blue] = colour.map(|channel| channel as u8);
        image::Rgba([red, green, blue, 255])
    });
    let mut bytes = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

/// On lavapipe (#930, scenes D2): soft-sd before (a colour backdrop) and after (a gradient, a picture covering the frame
/// and a picture contained in it with gradient bars), at 16:9 and 9:16. The backdrop shows as authored: its pixels
/// are not exposed or tone mapped. Set `OMUZAKI_EVIDENCE_DIR` to keep the sheet.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence image is written beside the test, not by an op")]
fn on_the_gpu_gradient_and_image_backdrops_at_two_aspects() {
    use omuzaki_char::ops::character::create::CharacterCreate;
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
    use omuzaki_stage::ops::scene::create::CreateScene;
    use omuzaki_stage::ops::scene::look::SetBackdrop;
    use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};

    const LOOKS: [&str; 4] = ["colour", "gradient", "cover", "contain"];
    let room = synthetic_room();
    let asset = ContentHash::of(&room);
    let mut doc = MemDoc::from_value(json!({}));
    apply::<CharacterCreate>(&mut doc, json!({ "id": "soft_sd", "name": "soft-sd", "template": "soft-sd", "seed": 7 }));
    let (top, bottom) = ("#1d2b4f", "#e8a87c");
    for look in LOOKS {
        apply::<CreateScene>(&mut doc, json!({ "id": look, "cast": [] }));
        let backdrop = match look {
            "colour" => json!({ "scene": look, "kind": "colour", "colour": "#2b2d42" }),
            "gradient" => json!({ "scene": look, "kind": "gradient", "top": top, "bottom": bottom }),
            fit => json!({ "scene": look, "kind": "image", "asset": asset, "fit": fit, "top": top, "bottom": bottom }),
        };
        apply::<SetBackdrop>(&mut doc, backdrop);
    }
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = Harness::on(TestDoc(doc), Box::new(renderer));
    let provenance = Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    };
    assert_eq!(harness.store.put(&room, provenance).unwrap(), asset);
    let image = |hash: &str| {
        let png = harness.store.get(&hash.parse().unwrap()).unwrap();
        image::load_from_memory(&png).unwrap().to_rgba8()
    };
    let decoded = image::load_from_memory(&room).unwrap().to_rgba8();

    let mut cells = Vec::new();
    for size in [[256u32, 144], [144, 256]] {
        for look in LOOKS {
            let shot = run(
                &harness,
                json!({ "character": "soft_sd", "scene": look, "framing": "full", "size": size, "quality": "offline" }),
            )
            .unwrap_or_else(|e| panic!("{look} at {size:?}: {e:?}"));
            let got = image(&shot.hash);
            let corner = got.get_pixel(1, 1).0;
            match look {
                // A colour is shown as authored, so it keys exactly: #2b2d42 under ACES at the rig's exposure.
                "colour" => assert!(
                    corner[..3].iter().zip([0x2b_u8, 0x2d, 0x42]).all(|(a, b)| a.abs_diff(b) <= 1),
                    "{size:?}: the colour is {corner:?}"
                ),
                // The gradient's top row is its top colour, exactly as authored: #1d2b4f.
                "gradient" => assert!(
                    corner[..3].iter().zip([0x1d_u8, 0x2b, 0x4f]).all(|(a, b)| a.abs_diff(b) <= 3),
                    "{size:?}: the gradient's top is {corner:?}"
                ),
                // A 4:3 picture covering the frame shows its own pixels at the corner, cropped to fit.
                "cover" => {
                    let frame = size[0] as f32 / size[1] as f32;
                    let uv = omuzaki_render_api::BackdropFit::Cover.uv(
                        omuzaki_core::glam::Vec2::new(1.5 / size[0] as f32, 1.5 / size[1] as f32),
                        frame,
                        4.0 / 3.0,
                    );
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a corner uv is in 0..1
                    let want = decoded.get_pixel((uv.x * 400.0) as u32, (uv.y * 300.0) as u32).0;
                    assert!(
                        corner[..3].iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 6),
                        "{size:?}: the cover's corner is {corner:?}, the picture there {want:?}"
                    );
                }
                _ => {}
            }
            assert_eq!(corner[3], 255, "{look} at {size:?}: the backdrop is opaque");
            cells.push(shot.uri);
        }
    }
    let op: ContactSheet = serde_json::from_value(json!({ "images": cells, "columns": LOOKS.len() })).unwrap();
    let sheet: ContactSheetResult = serde_json::from_value(harness.run(&op).expect("lays out").1).unwrap();
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let png = harness.store.get(&sheet.hash.parse().unwrap()).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("scene-backdrops-sheet.png"), png).unwrap();
    }
}
