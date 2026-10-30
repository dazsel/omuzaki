//! The eye's fine detail (#2465 to #2468) and the eye presets and random eye (#2469): every new knob is off by default
//! and changes the picture when turned on, an eye with everything on stays inside the painter's command budget, the
//! ops write a whole, valid eye that undo takes back, and the same seed gives the same eye. With
//! `OMUZAKI_FACE_EVIDENCE_DIR` set, the evidence cases in `evidence/face-tools.toml` render the presets, a sheet of
//! random eyes and a before/after close-up of the kit's eye.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::eye_presets::{DEFAULT_STYLE, preset_eyes, presets, randomise};
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Catchlight, CatchlightShape, Cmd, DEFAULT_PX_PER_M, EyeParams, FaceDrawing, FaceInput, FaceLayerId, FaceMapping,
    FacePainter, FaceProfile, FaceRasterizer, FaceView, PaintParams, Raster, Validate,
};
use omuzaki_face_mock::mock_projection;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// The most commands one eye may take with everything on (the budget `eyes_iris.rs` keeps).
const COMMANDS_PER_EYE: usize = 600;

fn open_params() -> PaintParams {
    let open = EyeParams { open: 1.0, close: 0.0, arc: 0.0, lower_lid: 0.0 };
    PaintParams { left_eye: open, right_eye: open, ..FaceMapping::default().rest_params() }
}

fn draw(profile: &FaceProfile) -> FaceDrawing {
    let mut drawing = FaceDrawing::new();
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let params = open_params();
    let input = FaceInput { profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
    drawing
}

fn fingerprint(profile: &FaceProfile) -> u64 {
    draw(profile)[FaceLayerId::Base].fingerprint()
}

fn face_with_eyes(eyes: Value) -> FaceProfile {
    let mut face = serde_json::to_value(FaceProfile::default()).unwrap();
    face["eyes"] = eyes;
    serde_json::from_value(face).unwrap()
}

/// Everything turned all the way up.
fn everything() -> FaceProfile {
    let mut p = FaceProfile::default();
    let d = &mut p.eyes.iris.detail;
    d.fibres.strength = 1.0;
    d.fibres.count = 256;
    d.collarette.strength = 1.0;
    d.collarette.teeth = 64;
    d.flecks.strength = 1.0;
    d.flecks.count = 64;
    d.limbal.strength = 1.0;
    d.pupil_glow = 1.0;
    let h = &mut p.eyes.highlights;
    h.caustic = 1.0;
    h.shimmer = 1.0;
    h.extra = [CatchlightShape::Blob, CatchlightShape::Window, CatchlightShape::Ring, CatchlightShape::Crescent]
        .into_iter()
        .cycle()
        .take(omuzaki_face_api::MAX_CATCHLIGHTS)
        .map(|shape| Catchlight { shape, ..Catchlight::default() })
        .collect();
    let s = &mut p.eyes.sclera;
    (s.corner_shade, s.vessels, s.caruncle, s.waterline) = (1.0, 1.0, 1.0, 1.0);
    p.eyes.lash.strands.upper.count = 200;
    p.eyes.lash.strands.lower.count = 200;
    p.validate().unwrap();
    p
}

// ---- the knobs

#[test]
fn the_detail_is_off_by_default_and_a_zero_strength_draws_nothing_whatever_the_other_knobs() {
    let base = fingerprint(&FaceProfile::default());
    let mut p = FaceProfile::default();
    p.eyes.iris.detail.fibres.count = 200;
    p.eyes.iris.detail.collarette.teeth = 40;
    p.eyes.iris.detail.flecks.count = 50;
    p.eyes.lash.strands.upper.length = 0.8;
    p.eyes.sclera.vessel_colour = omuzaki_face_api::ColourRef::rgb(0, 0, 255);
    p.eyes.seed = 99;
    assert_eq!(fingerprint(&p), base, "knobs under a zero strength or count change nothing");
}

#[test]
fn every_knob_changes_the_eye_when_turned_on() {
    type Turn = fn(&mut FaceProfile);
    let base = fingerprint(&FaceProfile::default());
    let knobs: [(&str, Turn); 14] = [
        ("fibres", |p| p.eyes.iris.detail.fibres.strength = 0.7),
        ("collarette", |p| p.eyes.iris.detail.collarette.strength = 0.7),
        ("flecks", |p| p.eyes.iris.detail.flecks.strength = 0.7),
        ("limbal", |p| p.eyes.iris.detail.limbal.strength = 0.7),
        ("pupil glow", |p| p.eyes.iris.detail.pupil_glow = 0.7),
        ("caustic", |p| p.eyes.highlights.caustic = 0.7),
        ("window", |p| p.eyes.highlights.shape = CatchlightShape::Window),
        ("ring", |p| {
            p.eyes.highlights.extra = vec![Catchlight { shape: CatchlightShape::Ring, ..Catchlight::default() }];
        }),
        ("crescent", |p| {
            p.eyes.highlights.extra = vec![Catchlight { shape: CatchlightShape::Crescent, ..Catchlight::default() }];
        }),
        ("corner shade", |p| p.eyes.sclera.corner_shade = 0.7),
        ("vessels", |p| p.eyes.sclera.vessels = 0.7),
        ("caruncle", |p| p.eyes.sclera.caruncle = 0.7),
        ("waterline", |p| p.eyes.sclera.waterline = 0.7),
        ("strands", |p| p.eyes.lash.strands.upper.count = 20),
    ];
    for (name, turn) in knobs {
        let mut p = FaceProfile::default();
        turn(&mut p);
        p.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(fingerprint(&p), base, "{name} changes nothing");
    }
}

#[test]
fn the_seed_moves_the_pattern_and_the_same_seed_repeats_it() {
    let mut a = everything();
    a.eyes.seed = 1;
    let mut b = a.clone();
    b.eyes.seed = 2;
    assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
    assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn an_eye_with_everything_on_stays_inside_the_command_budget() {
    let drawing = draw(&everything());
    let cmds = drawing[FaceLayerId::Base].cmds().len();
    assert!(cmds <= 2 * COMMANDS_PER_EYE, "{cmds} commands for two eyes");
    // Clips and transforms pair up, or the rasterisers refuse the list.
    let (mut clips, mut transforms) = (0_i32, 0_i32);
    for cmd in drawing[FaceLayerId::Base].cmds() {
        match cmd {
            Cmd::PushClip(_) => clips += 1,
            Cmd::PopClip => clips -= 1,
            Cmd::PushTransform(_) => transforms += 1,
            Cmd::PopTransform => transforms -= 1,
            _ => {}
        }
        assert!(clips >= 0 && transforms >= 0);
    }
    assert_eq!((clips, transforms), (0, 0));
}

#[test]
fn an_eye_with_everything_on_rasterises_and_stays_inside_the_eye() {
    let profile = everything();
    let drawing = draw(&profile);
    let painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut raster).unwrap();
    // Nothing the eyes paint reaches the bottom third of the face window (the mouth's place).
    let (w, h) = (raster.width as usize, raster.height as usize);
    let low = raster.pixels[(w * 4) * (h * 2 / 3)..].chunks(4).filter(|px| px[3] > 0).count();
    assert_eq!(low, 0, "{low} pixels painted below the eyes");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_eye_with_everything_on_encodes_for_the_gpu() {
    let drawing = draw(&everything());
    let painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    let mut scene = vello::Scene::new();
    omuzaki_face::raster::gpu::SceneEncoder::new()
        .encode(&spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut scene)
        .unwrap();
}

// ---- the presets and the random eye

#[test]
fn every_preset_and_every_random_eye_paints() {
    for preset in presets().unwrap() {
        let profile = face_with_eyes(preset_eyes(&preset.id).unwrap());
        profile.validate().unwrap_or_else(|e| panic!("{}: {e}", preset.id));
        assert!(draw(&profile)[FaceLayerId::Base].cmds().len() <= 2 * COMMANDS_PER_EYE, "{}", preset.id);
    }
    let base = preset_eyes(DEFAULT_STYLE).unwrap();
    for seed in 0..64 {
        let profile = face_with_eyes(randomise(&base, seed, &[]));
        profile.validate().unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        draw(&profile);
    }
}

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn apply(doc: &mut MemDoc, name: &str, args: Value) -> Plan {
    let plan = run(doc, name, args).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    doc.apply(&plan.patch).unwrap();
    plan
}

fn eyes_of(doc: &MemDoc) -> Value {
    doc.get(&DocPath::parse("characters/mira/face/eyes").unwrap()).cloned().unwrap_or(Value::Null)
}

fn doc() -> MemDoc {
    let mut face = serde_json::to_value(FaceProfile::default()).unwrap();
    face["eyes"]["iris"]["mid"] = json!("#c03a3a");
    face["eyes"]["right_iris"] = json!({ "mid": "#3a3ac0" });
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

#[test]
fn the_preset_op_writes_the_whole_eye_keeps_the_colours_and_undoes() {
    let mut doc = doc();
    let before = eyes_of(&doc);
    let listed = run(&doc, "face.eyes.list_presets", json!({})).unwrap().output.unwrap();
    assert!(listed["presets"].as_array().unwrap().iter().any(|p| p["id"] == "realistic"), "{listed}");

    let plan = apply(&mut doc, "face.eyes.preset", json!({ "character": "mira", "preset": "realistic" }));
    let eyes = eyes_of(&doc);
    assert_eq!(eyes["iris"]["mid"], json!("#c03a3a"), "the character's colours stay");
    assert_eq!(eyes["right_iris"]["mid"], json!("#3a3ac0"));
    assert_eq!(eyes["iris"]["width"], preset_eyes("realistic").unwrap()["iris"]["width"]);
    assert_eq!(plan.output.unwrap()["preset"], "realistic");

    // The preset's colours too, and the right eye's own iris gone with them.
    apply(&mut doc, "face.eyes.preset", json!({ "character": "mira", "preset": "cat", "keep_colours": false }));
    let eyes = eyes_of(&doc);
    assert_eq!(eyes["iris"]["mid"], preset_eyes("cat").unwrap()["iris"]["mid"]);
    assert!(eyes.get("right_iris").is_none());

    // Undo: the first edit's inverse, applied after it alone, gives the old eye back.
    let mut fresh = self::doc();
    let first = run(&fresh, "face.eyes.preset", json!({ "character": "mira", "preset": "doll" })).unwrap();
    fresh.apply(&first.patch).unwrap();
    fresh.apply(&first.patch.inverse()).unwrap();
    assert_eq!(eyes_of(&fresh), before, "undo restores the old eye");

    let refused = run(&doc, "face.eyes.preset", json!({ "character": "mira", "preset": "nope" })).unwrap_err();
    assert!(refused.to_string().contains("kit"), "{refused}");
}

#[test]
fn the_random_op_repeats_its_seed_and_keeps_what_it_is_told() {
    let mut a = doc();
    let mut b = doc();
    apply(&mut a, "face.eyes.randomise", json!({ "character": "mira", "seed": 5 }));
    apply(&mut b, "face.eyes.randomise", json!({ "character": "mira", "seed": 5 }));
    assert_eq!(eyes_of(&a), eyes_of(&b));
    let first = eyes_of(&a);
    apply(&mut a, "face.eyes.randomise", json!({ "character": "mira", "seed": 6, "keep": ["colours", "lashes"] }));
    let second = eyes_of(&a);
    assert_eq!(first["iris"]["mid"], second["iris"]["mid"]);
    assert_eq!(first["lash"], second["lash"]);
    assert_ne!(first["aperture"], second["aperture"]);
    let face: FaceProfile =
        serde_json::from_value(a.get(&DocPath::parse("characters/mira/face").unwrap()).cloned().unwrap()).unwrap();
    face.validate().unwrap();
}

// ---- evidence

fn evidence_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from)
}

/// The eye sections over a skin-coloured face window at `px_per_m`, cropped round the left eye when `close`, or round
/// both eyes otherwise.
fn render(profile: &FaceProfile, px_per_m: f32, close: bool) -> image::RgbaImage {
    let mut painter = RealPainter::new(mock_projection(), px_per_m);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    let (w, h) = spec.size_px();
    let mut cell = image::RgbaImage::from_pixel(w, h, image::Rgba([249, 222, 210, 255]));
    let params = open_params();
    let input = FaceInput { profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut rasterizer = SkiaRasterizer::new();
    for section in [Section::Eyes, Section::Brows] {
        let mut drawing = FaceDrawing::default();
        painter.paint_section(section, &input, &mut drawing).unwrap();
        let mut raster = Raster::new(1, 1);
        rasterizer.rasterize(&spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut raster).unwrap();
        for (dst, src) in cell.pixels_mut().zip(raster.pixels.as_chunks::<4>().0.iter()) {
            // Premultiplied over opaque: src + dst * (1 - alpha).
            let keep = 255 - u32::from(src[3]);
            let under = dst.0;
            for c in 0..3 {
                dst.0[c] = u8::try_from((u32::from(src[c]) + u32::from(under[c]) * keep / 255).min(255)).unwrap();
            }
        }
    }
    let r = mock_projection().rect;
    let ey = ((r.y1 - profile.layout.eye_y as f32) * px_per_m) as u32;
    if close {
        let ex = ((profile.layout.eye_spacing as f32 - r.x0) * px_per_m) as u32;
        let half = (0.085 * px_per_m) as u32;
        return image::imageops::crop_imm(&cell, ex - half, ey - half * 3 / 4, half * 2, half * 3 / 2).to_image();
    }
    let top = ey.saturating_sub((0.07 * px_per_m) as u32);
    image::imageops::crop_imm(&cell, w / 8, top, w * 3 / 4, (0.13 * px_per_m) as u32).to_image()
}

fn grid(cells: &[image::RgbaImage], cols: usize) -> image::RgbaImage {
    let (cw, ch) = (cells[0].width(), cells[0].height());
    let rows = cells.len().div_ceil(cols);
    let mut sheet = image::RgbaImage::from_pixel(cw * cols as u32, ch * rows as u32, image::Rgba([40, 40, 40, 255]));
    for (i, c) in cells.iter().enumerate() {
        image::imageops::replace(&mut sheet, c, i64::from((i % cols) as u32 * cw), i64::from((i / cols) as u32 * ch));
    }
    sheet
}

#[test]
fn render_the_eye_presets_when_asked() {
    let Some(dir) = evidence_dir() else { return };
    let profiles: Vec<FaceProfile> =
        presets().unwrap().iter().map(|p| face_with_eyes(preset_eyes(&p.id).unwrap())).collect();
    let close: Vec<_> = profiles.iter().map(|p| render(p, 6000.0, true)).collect();
    grid(&close, 4).save(dir.join("eye-presets-close.png")).unwrap();
    let pairs: Vec<_> = profiles.iter().map(|p| render(p, 2400.0, false)).collect();
    grid(&pairs, 2).save(dir.join("eye-presets.png")).unwrap();
}

#[test]
fn render_random_eyes_when_asked() {
    let Some(dir) = evidence_dir() else { return };
    let base = preset_eyes(DEFAULT_STYLE).unwrap();
    let cells: Vec<_> =
        (0..24).map(|seed| render(&face_with_eyes(randomise(&base, seed, &[])), 2400.0, false)).collect();
    grid(&cells, 4).save(dir.join("eye-random.png")).unwrap();
}

#[test]
fn render_the_kit_eye_before_and_after_when_asked() {
    let Some(dir) = evidence_dir() else { return };
    // Before: the kit's eye shape on the old eye; after: the kit preset.
    let mut before = preset_eyes("kit").unwrap();
    let old = serde_json::to_value(omuzaki_face_api::EyeStyle::default()).unwrap();
    for key in ["iris", "highlights", "sclera", "lash"] {
        before[key] = old[key].clone();
    }
    before["iris"]["drop"] = json!(0.0);
    before["iris"]["height"] = json!(0.9);
    let cells = [
        render(&face_with_eyes(before), 9000.0, true),
        render(&face_with_eyes(preset_eyes("kit").unwrap()), 9000.0, true),
    ];
    grid(&cells, 2).save(dir.join("eye-kit-before-after.png")).unwrap();
}
