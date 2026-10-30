//! Makeup (#2472) and the teeth and tongue's shape (#2473): every new knob is off by default and changes the face when
//! turned on, every makeup look paints and validates, and `face.makeup.preset` writes the whole look, leaves the rest
//! of the face alone and undoes. With `OMUZAKI_FACE_EVIDENCE_DIR` set, the evidence cases in `evidence/face-tools.toml`
//! render the looks on the kit's face and the mouth's new shapes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::makeup_presets::{preset_makeup, presets};
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    BlushKind, EyeParams, FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FacePainter, FaceProfile, FaceRasterizer,
    FaceView, MouthParams, PaintParams, Raster, Validate,
};
use omuzaki_face_mock::mock_projection;
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

/// The resting face with its eyes open.
fn rest() -> PaintParams {
    let open = EyeParams { open: 1.0, close: 0.0, arc: 0.0, lower_lid: 0.0 };
    PaintParams { left_eye: open, right_eye: open, ..FaceMapping::default().rest_params() }
}

/// An open mouth with the upper teeth and the tongue showing.
fn open_mouth() -> PaintParams {
    let mouth = MouthParams { open: 1.0, teeth_upper: 1.0, tongue: 0.6, ..rest().mouth };
    PaintParams { mouth, ..rest() }
}

/// Each section's drawing, in draw order (painting a section replaces the drawing's lists).
fn draw(profile: &FaceProfile, params: &PaintParams) -> Vec<FaceDrawing> {
    let mut painter = RealPainter::new(mock_projection(), omuzaki_face_api::DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    Section::ALL
        .iter()
        .map(|section| {
            let mut drawing = FaceDrawing::new();
            painter.paint_section(*section, &input, &mut drawing).unwrap();
            drawing
        })
        .collect()
}

fn fingerprint(profile: &FaceProfile, params: &PaintParams) -> Vec<u64> {
    draw(profile, params).iter().map(|d| d[FaceLayerId::Base].fingerprint()).collect()
}

/// The bare face with makeup preset `id` on it.
fn with_look(id: &str) -> FaceProfile {
    let mut face = serde_json::to_value(FaceProfile::default()).unwrap();
    for (path, value) in preset_makeup(id).unwrap() {
        let mut node = &mut face;
        for key in &path[..path.len() - 1] {
            node = &mut node[*key];
        }
        node[path[path.len() - 1]] = value;
    }
    serde_json::from_value(face).unwrap()
}

// ---- the knobs

#[test]
fn the_makeup_is_off_by_default_and_its_colours_alone_change_nothing() {
    for params in [rest(), open_mouth()] {
        let base = fingerprint(&FaceProfile::default(), &params);
        let mut p = FaceProfile::default();
        p.eyes.shadow.colour = omuzaki_face_api::ColourRef::rgb(0, 0, 255);
        p.eyes.shadow.wing = 1.0;
        p.eyes.shadow.lower = 1.0;
        p.cheeks.contour_colour = omuzaki_face_api::ColourRef::rgb(0, 255, 0);
        p.mouth.teeth.count = 12;
        p.mouth.teeth.gum_colour = omuzaki_face_api::ColourRef::rgb(0, 0, 255);
        assert_eq!(fingerprint(&p, &params), base, "knobs under a zero strength change nothing");
    }
}

#[test]
fn every_knob_changes_the_face_when_turned_on() {
    type Turn = fn(&mut FaceProfile);
    let knobs: [(&str, bool, Turn); 14] = [
        ("eyeshadow", false, |p| p.eyes.shadow.strength = 0.7),
        ("eyeshadow under", false, |p| (p.eyes.shadow.strength, p.eyes.shadow.lower) = (0.7, 0.8)),
        ("lip tint", false, |p| p.mouth.lips.tint = 0.7),
        ("lip tint, open", true, |p| p.mouth.lips.tint = 0.7),
        ("blush base", false, |p| p.cheeks.base = 0.6),
        ("sweep blush", false, |p| (p.cheeks.kind, p.cheeks.base) = (BlushKind::Sweep, 0.6)),
        ("bridge blush", false, |p| (p.cheeks.kind, p.cheeks.base) = (BlushKind::Bridge, 0.6)),
        ("contour", false, |p| p.cheeks.contour = 0.6),
        ("nose shadow", false, |p| p.nose.shadow = 0.6),
        ("nose highlight", false, |p| p.nose.highlight = 0.6),
        ("teeth height", true, |p| p.mouth.teeth.height = 1.5),
        ("teeth gaps", true, |p| p.mouth.teeth.gaps = 0.8),
        ("gum", true, |p| p.mouth.teeth.gum = 0.8),
        ("tongue groove", true, |p| (p.mouth.tongue.groove, p.mouth.tongue.width) = (0.8, 1.2)),
    ];
    for (name, open, turn) in knobs {
        let params = if open { open_mouth() } else { rest() };
        let base = fingerprint(&FaceProfile::default(), &params);
        let mut p = FaceProfile::default();
        turn(&mut p);
        p.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(fingerprint(&p, &params), base, "{name} changes nothing");
    }
}

#[test]
fn a_sweep_or_bridge_blush_still_follows_the_blush_channel() {
    for kind in [BlushKind::Sweep, BlushKind::Bridge] {
        let mut p = FaceProfile::default();
        p.cheeks.kind = kind;
        let calm = fingerprint(&p, &rest());
        let blushing = fingerprint(&p, &PaintParams { blush: 1.0, ..rest() });
        assert_ne!(calm, blushing, "{kind:?} ignores the channel");
    }
}

#[test]
fn every_look_validates_and_rasterises() {
    let mut rasterizer = SkiaRasterizer::new();
    let painter = RealPainter::new(mock_projection(), 2000.0);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    for preset in presets().unwrap() {
        let profile = with_look(&preset.id);
        profile.validate().unwrap_or_else(|e| panic!("{}: {e}", preset.id));
        for params in [rest(), open_mouth()] {
            for drawing in draw(&profile, &params) {
                let mut raster = Raster::new(1, 1);
                rasterizer.rasterize(&spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut raster).unwrap();
            }
        }
    }
}

// ---- the op

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn face_of(doc: &MemDoc) -> Value {
    doc.get(&DocPath::parse("characters/mira/face").unwrap()).cloned().unwrap_or(Value::Null)
}

fn doc() -> MemDoc {
    // A partial face, as a character file holds one: only what differs from the defaults.
    let face = json!({ "eyes": { "iris": { "mid": "#c03a3a" } }, "cheeks": { "strength": 0.6 } });
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

#[test]
fn the_makeup_op_writes_the_whole_look_leaves_the_rest_and_undoes() {
    let mut doc = doc();
    let before = face_of(&doc);
    let listed = run(&doc, "face.makeup.list_presets", json!({})).unwrap().output.unwrap();
    assert!(listed["presets"].as_array().unwrap().iter().any(|p| p["id"] == "soft-glam"), "{listed}");

    let plan = run(&doc, "face.makeup.preset", json!({ "character": "mira", "preset": "gothic" })).unwrap();
    doc.apply(&plan.patch).unwrap();
    let face = face_of(&doc);
    assert_eq!(face["eyes"]["iris"]["mid"], json!("#c03a3a"), "the rest of the face stays");
    assert_eq!(face["cheeks"]["strength"], json!(0.6), "the blush channel's strength is not makeup");
    assert_eq!(face["mouth"]["lips"]["colour"], json!("#7a2240"));
    assert_eq!(plan.output.unwrap()["preset"], "gothic");
    let profile: FaceProfile = serde_json::from_value(face).unwrap();
    profile.validate().unwrap();

    // Another look replaces the whole of this one: the gothic contour goes.
    let natural = run(&doc, "face.makeup.preset", json!({ "character": "mira", "preset": "natural" })).unwrap();
    doc.apply(&natural.patch).unwrap();
    let profile: FaceProfile = serde_json::from_value(face_of(&doc)).unwrap();
    assert!(profile.cheeks.contour.abs() < f64::EPSILON);
    assert!(profile.eyes.shadow.strength > 0.0);

    // Undo, in order, gives the bare face back.
    doc.apply(&natural.patch.inverse()).unwrap();
    doc.apply(&plan.patch.inverse()).unwrap();
    assert_eq!(face_of(&doc), before);

    let refused = run(&doc, "face.makeup.preset", json!({ "character": "mira", "preset": "clown" })).unwrap_err();
    assert!(refused.to_string().contains("natural"), "{refused}");
}

#[test]
fn none_takes_the_makeup_off() {
    let mut doc = doc();
    for preset in ["idol", "none"] {
        let plan = run(&doc, "face.makeup.preset", json!({ "character": "mira", "preset": preset })).unwrap();
        doc.apply(&plan.patch).unwrap();
    }
    let profile: FaceProfile = serde_json::from_value(face_of(&doc)).unwrap();
    let bare = FaceProfile::default();
    assert_eq!(profile.eyes.shadow, bare.eyes.shadow);
    assert_eq!(profile.cheeks.kind, bare.cheeks.kind);
    assert!(profile.nose.highlight.abs() < f64::EPSILON);
}

// ---- evidence

fn evidence_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from)
}

/// The kit's face from the starter character.
fn kit_face() -> Value {
    let starter: toml::Value = toml::from_str(include_str!("../../omuzaki-char/library/starter.toml")).unwrap();
    let face = serde_json::to_value(starter.get("face").cloned().unwrap()).unwrap();
    let mut whole = serde_json::to_value(FaceProfile::default()).unwrap();
    merge(&mut whole, &face);
    whole
}

fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// The kit's face wearing look `id`.
fn kit_with(id: &str) -> FaceProfile {
    let mut face = kit_face();
    for (path, value) in preset_makeup(id).unwrap() {
        let mut node = &mut face;
        for key in &path[..path.len() - 1] {
            node = &mut node[*key];
        }
        node[path[path.len() - 1]] = value;
    }
    serde_json::from_value(face).unwrap()
}

/// The whole face over a skin-coloured window at `px_per_m`, cropped to the face below the brows, or to the mouth when
/// `mouth`.
fn render(profile: &FaceProfile, params: &PaintParams, px_per_m: f32, mouth: bool) -> image::RgbaImage {
    let mut painter = RealPainter::new(mock_projection(), px_per_m);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    let (w, h) = spec.size_px();
    let mut cell = image::RgbaImage::from_pixel(w, h, image::Rgba([249, 222, 210, 255]));
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    let mut rasterizer = SkiaRasterizer::new();
    for section in Section::ALL {
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
    let to_px = |y: f64| ((r.y1 - y as f32) * px_per_m) as u32;
    if mouth {
        let my = to_px(profile.layout.mouth_y);
        let half = (0.05 * px_per_m) as u32;
        return image::imageops::crop_imm(&cell, w / 2 - half, my - half * 2 / 3, half * 2, half * 4 / 3).to_image();
    }
    let top = to_px(profile.layout.eye_y).saturating_sub((0.075 * px_per_m) as u32);
    let bottom = (to_px(profile.layout.mouth_y) + (0.04 * px_per_m) as u32).min(h);
    image::imageops::crop_imm(&cell, w / 8, top, w * 3 / 4, bottom - top).to_image()
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
fn render_the_makeup_looks_when_asked() {
    let Some(dir) = evidence_dir() else { return };
    let cells: Vec<_> = presets().unwrap().iter().map(|p| render(&kit_with(&p.id), &rest(), 2400.0, false)).collect();
    grid(&cells, 3).save(dir.join("makeup-looks.png")).unwrap();
}

#[test]
fn render_the_teeth_and_tongue_when_asked() {
    let Some(dir) = evidence_dir() else { return };
    let kit = kit_with("none");
    let mut shapes = Vec::new();
    for turn in [
        (|_: &mut FaceProfile| {}) as fn(&mut FaceProfile),
        |p| (p.mouth.teeth.gaps, p.mouth.teeth.count) = (0.7, 8),
        |p| (p.mouth.teeth.gaps, p.mouth.teeth.gum, p.mouth.teeth.height) = (0.6, 0.8, 1.4),
        |p| (p.mouth.tongue.groove, p.mouth.tongue.width, p.mouth.tongue.height) = (0.8, 1.2, 1.3),
    ] {
        let mut p = kit.clone();
        turn(&mut p);
        shapes.push(render(&p, &open_mouth(), 9000.0, true));
    }
    grid(&shapes, 4).save(dir.join("mouth-teeth-tongue.png")).unwrap();
}
