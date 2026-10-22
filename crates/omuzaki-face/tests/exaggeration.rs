//! Exaggeration profiles (#383, FE-06): the dial gives the defaults bit for bit at 0 and never shrinks a coefficient as
//! it turns up, every preset keeps a valid face whose paint stays in range, `face.set_exaggeration` applies and undoes
//! exactly, and, with `OMUZAKI_FACE_EVIDENCE_DIR` set, the contact sheet of three presets, five expressions and two
//! strengths (full, and the subtle movement a webcam reads from a calm face, with and without the expander).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{CHANNELS, ChannelBuf};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::exaggeration::{Preset, at, ends, gains, parse};
use omuzaki_face::preview::metrics::downsample;
use omuzaki_face::preview::sheet::{Cell, compose};
use omuzaki_face::preview::{FaceSubject, Preview};
use omuzaki_face_api::{
    Amplify, ExpressionId, ExpressionSet, FaceMapping, FaceProfile, PaintParams, Palette, Validate,
};
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

#[test]
fn amount_zero_is_the_defaults_bit_for_bit() {
    let defaults = FaceMapping::default();
    let mapping = at(&defaults, 0.0).unwrap();
    assert_eq!(mapping, defaults);
    let json = serde_json::to_value(&defaults).unwrap();
    for (path, value) in gains(0.0).unwrap() {
        let (section, key) = path.split_once('/').unwrap();
        assert_eq!(json[section][key].as_f64().unwrap().to_bits(), value.to_bits(), "{path}");
    }
}

#[test]
fn the_dial_is_monotone_and_its_ends_are_the_presets() {
    let ends = ends().unwrap();
    assert!(!ends.realistic.is_empty() && !ends.chibi.is_empty());
    let mut last = gains(-1.0).unwrap();
    for step in -19..=20 {
        let amount = f64::from(step) / 20.0;
        let now = gains(amount).unwrap();
        for (path, value) in &now {
            assert!(*value >= last[path], "{path} shrank from {} to {value} at amount {amount}", last[path]);
        }
        last = now;
    }
    let defaults = serde_json::to_value(FaceMapping::default()).unwrap();
    let chibi = gains(1.0).unwrap();
    let realistic = gains(-1.0).unwrap();
    for (path, factor) in &ends.chibi {
        let (section, key) = path.split_once('/').unwrap();
        let default = defaults[section][key].as_f64().unwrap();
        assert!((chibi[path] - default * factor).abs() < 1e-4, "{path} at +1");
    }
    for (path, factor) in &ends.realistic {
        let (section, key) = path.split_once('/').unwrap();
        let default = defaults[section][key].as_f64().unwrap();
        assert!((realistic[path] - default * factor).abs() < 1e-4, "{path} at -1");
    }
    assert_eq!(gains(5.0).unwrap(), chibi, "the dial stops at its ends");
}

#[test]
fn a_broken_end_table_is_refused_by_name() {
    assert!(parse("t", "[eyes]\nwide = 0.5\n", true).unwrap_err().contains("at least 1"));
    assert!(parse("t", "[eyes]\nwide = 1.5\n", false).unwrap_err().contains("at most 1"));
    assert!(parse("t", "[eyes]\nwyde = 1.5\n", true).unwrap_err().contains("eyes/wyde"));
    assert!(parse("t", "[rest]\neye_open = 1.5\n", true).unwrap_err().contains("rest"));
}

/// Twenty bus inputs: neutral, everything full, everything at a tracker's subtle 0.15, and seventeen spread by a fixed
/// generator over each channel's whole range.
fn twenty_buses() -> Vec<ChannelBuf> {
    let fill = |v: f32| {
        let mut b = ChannelBuf::NEUTRAL;
        for info in CHANNELS {
            b.set(info.channel, v.clamp(info.min, info.max));
        }
        b
    };
    let mut state: u32 = 0x9e37_79b9;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        #[allow(clippy::cast_precision_loss)] // a 24-bit fraction is exact in f32
        let unit = (state >> 8) as f32 / (1 << 24) as f32;
        unit
    };
    let mut out = vec![ChannelBuf::NEUTRAL, fill(1.0), fill(0.15)];
    for _ in 0..17 {
        let mut b = ChannelBuf::NEUTRAL;
        for info in CHANNELS {
            b.set(info.channel, info.min + (info.max - info.min) * next());
        }
        out.push(b);
    }
    out
}

/// Every output range the paint parameters' docs promise.
fn assert_in_range(p: &PaintParams, what: &str) {
    let within = |x: f32, lo: f32, hi: f32| (lo..=hi).contains(&x);
    for eye in [p.left_eye, p.right_eye] {
        assert!(within(eye.open, 0.0, 1.9) && within(eye.lower_lid, 0.0, 1.0), "{what}: {eye:?}");
    }
    assert!(within(p.eye_tilt, -2.0, 2.0) && within(p.pupil, 0.4, 1.6), "{what}: {p:?}");
    assert!(within(p.gaze.x, -1.0, 1.0) && within(p.gaze.y, -1.0, 1.0) && within(p.vergence, -1.0, 1.0), "{what}");
    for brow in [p.left_brow, p.right_brow] {
        assert!(within(brow.height, -1.5, 1.5) && within(brow.tilt, -2.5, 2.5), "{what}: {brow:?}");
    }
    let m = p.mouth;
    assert!(within(m.open, 0.0, 1.5) && within(m.wide, 0.3, 2.0) && within(m.shift, -1.0, 1.0), "{what}: {m:?}");
    assert!(within(m.smile_left, -2.0, 2.0) && within(m.smile_right, -2.0, 2.0), "{what}: {m:?}");
    for v in [m.round, m.press, m.pucker, m.teeth_upper, m.teeth_lower] {
        assert!(within(v, 0.0, 1.0), "{what}: {m:?}");
    }
    assert!(within(p.blush, 0.0, 1.6) && within(p.brow_arch, -1.0, 1.0), "{what}: {p:?}");
}

#[test]
fn every_preset_is_a_valid_face_whose_paint_stays_in_range() {
    let buses = twenty_buses();
    assert_eq!(buses.len(), 20);
    for preset in Preset::ALL {
        for amplify in [Amplify::default(), Amplify { knee: 0.25, gain: 4.0 }] {
            let mut mapping = at(&FaceMapping::default(), preset.amount()).unwrap();
            mapping.amplify = amplify;
            mapping.validate().unwrap_or_else(|e| panic!("{}: {e:?}", preset.name()));
            for (i, bus) in buses.iter().enumerate() {
                assert_in_range(&mapping.apply(bus), &format!("{} bus {i}", preset.name()));
            }
        }
    }
    // The ends really differ: a full smile lifts the corners less on a realistic face, more on a chibi one.
    let smile = |preset: Preset| {
        let mut b = ChannelBuf::NEUTRAL;
        b.set(omuzaki_anim_api::Channel::MOUTH_SMILE_LEFT, 0.6);
        at(&FaceMapping::default(), preset.amount()).unwrap().apply(&b).mouth.smile_left
    };
    assert!(smile(Preset::Realistic) < smile(Preset::Anime) && smile(Preset::Anime) < smile(Preset::ChibiExtreme));
}

fn run(doc: &MemDoc, args: Value) -> Result<Plan, OpError> {
    let op = (find("face.set_exaggeration").expect("registered").decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn doc() -> MemDoc {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

fn mapping_of(doc: &MemDoc) -> FaceMapping {
    let face = doc.get(&DocPath::parse("characters/mira/face").unwrap()).cloned().unwrap();
    serde_json::from_value::<FaceProfile>(face).unwrap().mapping
}

#[test]
fn set_exaggeration_applies_and_undoes_exactly() {
    for args in [
        json!({ "character": "mira", "preset": "chibi-extreme" }),
        json!({ "character": "mira", "preset": "realistic" }),
        json!({ "character": "mira", "amount": 0.4, "amplify_subtle": { "knee": 0.2, "gain": 2.5 } }),
        json!({ "character": "mira", "amplify_subtle": {} }),
    ] {
        let mut document = doc();
        let before = document.root().clone();
        let plan = run(&document, args.clone()).unwrap_or_else(|e| panic!("{args}: {e}"));
        assert_eq!(plan, run(&document, args.clone()).unwrap(), "a dry run repeats");
        assert!(!plan.patch.changes.is_empty(), "{args}");
        document.apply(&plan.patch).unwrap();
        let mapping = mapping_of(&document);
        mapping.validate().unwrap();
        if let Some(amount) = args.get("amount").and_then(Value::as_f64) {
            assert_eq!(
                mapping,
                at(&FaceMapping { amplify: Amplify { knee: 0.2, gain: 2.5 }, ..FaceMapping::default() }, amount)
                    .unwrap()
            );
        }
        if args.get("amplify_subtle") == Some(&json!({})) {
            assert_eq!(mapping.amplify, Amplify { knee: 0.2, gain: 2.0 }, "the expander's defaults");
        }
        document.apply(&plan.patch.inverse()).unwrap();
        assert_eq!(document.root(), &before, "{args}: undo restores the document exactly");
    }
    // Turning the dial twice doesn't compound: chibi then anime is the defaults again.
    let mut document = doc();
    document.apply(&run(&document, json!({ "character": "mira", "preset": "chibi-extreme" })).unwrap().patch).unwrap();
    document.apply(&run(&document, json!({ "character": "mira", "amount": 0 })).unwrap().patch).unwrap();
    assert_eq!(mapping_of(&document), FaceMapping::default());
}

#[test]
fn set_exaggeration_refuses_what_it_cant_do() {
    let document = doc();
    let refused = |args: Value| run(&document, args.clone()).expect_err(&args.to_string()).message;
    assert!(refused(json!({ "character": "mira", "preset": "anime", "amount": 0.5 })).contains("not both"));
    assert!(refused(json!({ "character": "mira", "amount": 1.5 })).contains("out of range"));
    assert!(refused(json!({ "character": "mira" })).contains("nothing to set"));
    let knee = refused(json!({ "character": "mira", "amplify_subtle": { "knee": 0.5, "gain": 3.0 } }));
    assert!(knee.contains("knee"), "{knee}");
    assert!(run(&document, json!({ "character": "mira", "preset": "cartoon" })).is_err());
}

#[test]
fn render_the_exaggeration_sheet_when_asked() {
    let Some(dir) = std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let expressions = ["happy", "angry", "sad", "surprised", "smug"];
    let library = ExpressionSet::library().unwrap();
    let mut cells = Vec::new();
    // Rows: each strength (full, subtle, subtle with the expander) under each preset; columns: the five expressions.
    for (strength, weight, amplify) in [
        ("full", 1.0, Amplify::default()),
        ("subtle", 0.3, Amplify::default()),
        ("subtle+amp", 0.3, Amplify { knee: 0.3, gain: 2.5 }),
    ] {
        for preset in Preset::ALL {
            let mut mapping = at(&FaceMapping::default(), preset.amount()).unwrap();
            mapping.amplify = amplify.clone();
            let profile = FaceProfile { expressions: library.clone(), mapping, ..FaceProfile::default() };
            let subject = FaceSubject {
                profile: profile.resolve_colours(&Palette::default()).unwrap(),
                projection: HeadParams::default().face_projection(),
                skin: Colour::rgb(0xf9, 0xd3, 0xc5),
            };
            let rest = subject.profile.rest_buf();
            let mut preview = Preview::new(subject.clone(), 387);
            for name in expressions {
                let id = ExpressionId::new(name).unwrap();
                let bus = subject.profile.expressions.blend(&rest, &[(&id, weight)]).unwrap();
                preview.paint(&bus).unwrap();
                let picture = downsample(&preview.picture(), 160);
                cells.push(Cell { picture, label: format!("{} {strength} {name}", preset.name()), flagged: false });
            }
        }
    }
    compose(&cells, 5).save(dir.join("exaggeration.png")).unwrap();
}
