//! The likeness measures on real renders of kit characters (`docs/design/likeness-measures.md` 11.2, task T11): on
//! lavapipe, `render.preview {preset: "measure"}` of both kit templates measures without a refusal, the snapped eyes
//! and mouth agree with the points the render reports within 2% of the eye distance, the measured chin with the
//! render's, and blending the head from `round-u` toward `egg-oval` moves `aspect` and the outline distance one way at
//! every step. Set `OMUZAKI_EVIDENCE_DIR` to keep the renders.
//!
//! Both wait on #2386 (run them with `--include-ignored`). The `measure` render's line is closed now, under the chin
//! too (#2319: edge lines at every depth jump), and `standard` measures; but its eyes don't snap where the bangs cross
//! the lids, and a quarter of the way to `egg-oval` the profile leaks at both ends of the eye line, where hair hangs in
//! front of the temples.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "wgpu")]

#[path = "render/support.rs"]
mod support;

use omuzaki_char::head::presets::ShapeProfiles;
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_measure::face::likeness::{Compare, Side, Tolerances, likeness};
use omuzaki_measure::face::spacing::{FeatureSpacing, spacing};
use omuzaki_measure::face::{FaceLandmarks, FaceMeasure, LandmarkSource, MeasureInput, measure_face};
use omuzaki_measure::image::decode;
use omuzaki_ops_api::{Actor, Op, OpCtx};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult, RenderedFace};
use omuzaki_store_api::Store;
use serde_json::{Value, json};
use support::{Harness, TestDoc};

const SIDE: u32 = 1024;

fn kit_doc() -> MemDoc {
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Person { name: "test".to_owned() };
    let mut doc = MemDoc::from_value(json!({}));
    for template in ["standard", "mini"] {
        let op: CharacterCreate =
            serde_json::from_value(json!({ "id": template, "name": template, "template": template, "seed": 7 }))
                .unwrap();
        let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
        doc.apply(&plan.patch).unwrap();
    }
    doc
}

fn lavapipe(doc: MemDoc) -> Harness {
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    Harness::on(TestDoc(doc), Box::new(renderer))
}

/// A measurement of one render, with the points the render reported.
struct Measured {
    face: RenderedFace,
    m: FaceMeasure,
    s: FeatureSpacing,
}

impl Measured {
    fn side(&self) -> Side<'_> {
        Side {
            outline: &self.m.outline,
            spacing: &self.s,
            frame: Some(&self.m.frame),
            mouth_height: self.m.landmarks.mouth_height,
        }
    }

    fn iod(&self) -> f64 {
        (self.face.eye_left[0] - self.face.eye_right[0]).hypot(self.face.eye_left[1] - self.face.eye_right[1])
    }
}

/// Renders `character` with the `measure` preset and measures it, keeping the render as `evidence`.
fn render_and_measure(harness: &Harness, character: &str, evidence: &str) -> Measured {
    let op: Preview =
        serde_json::from_value(json!({ "character": character, "preset": "measure", "size": [SIDE, SIDE] }))
            .expect("the arguments read");
    let (_, value): (_, Value) = harness.run(&op).unwrap();
    let result: PreviewResult = serde_json::from_value(value).unwrap();
    let [face] = result.face_landmarks.as_slice() else { panic!("{character}: one face, got {result:?}") };
    let hash = result.hash.parse().unwrap();
    let png = harness.store.get(&hash).unwrap();
    keep(evidence, &png);
    let image = decode(&png, hash, "image").unwrap();
    let landmarks = FaceLandmarks {
        eye_right: face.eye_right,
        eye_left: face.eye_left,
        mouth: Some(face.mouth),
        nose: None,
        source: LandmarkSource::Render,
    };
    let m = measure_face(&image, &MeasureInput { landmarks, refine: true, exclude: Vec::new(), skin: None })
        .unwrap_or_else(|e| panic!("{character}: the render measures: {e:?}"));
    let s = spacing(&m);
    Measured { face: face.clone(), m, s }
}

/// Writes `png` to `OMUZAKI_EVIDENCE_DIR` as `name`, when it is set.
#[allow(clippy::disallowed_methods, reason = "evidence is written beside the test, not by an op")]
fn keep(name: &str, png: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), png).unwrap();
    }
}

#[test]
#[ignore = "#2386: the eyes don't snap where the bangs cross the lids"]
fn kit_renders_measure_and_the_snap_agrees_with_the_rendered_points() {
    let harness = lavapipe(kit_doc());
    for character in ["standard", "mini"] {
        let measured = render_and_measure(&harness, character, &format!("measure-kit-{character}.png"));
        let (iod, lm) = (measured.iod(), &measured.m.landmarks);
        let off = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]) / iod;
        let eyes = [off(lm.eye_right.used, measured.face.eye_right), off(lm.eye_left.used, measured.face.eye_left)];
        let mouth = lm.mouth.map(|m| off(m.used, measured.face.mouth)).unwrap();
        let (_, chin) = measured.m.chin();
        let chin = off(chin, measured.face.chin);
        let warnings: Vec<&str> = measured.m.warnings.iter().map(|w| w.code).collect();
        println!(
            "{character}: IOD {iod:.1} px; snapped eyes {:.2}% and {:.2}% of IOD from the render's points, mouth {:.2}%, \
             chin {:.2}%; coverage {:.3}, asymmetry {:?}, aspect {:.3}; warnings {warnings:?}",
            eyes[0] * 100.0,
            eyes[1] * 100.0,
            mouth * 100.0,
            chin * 100.0,
            measured.m.outline.coverage(),
            measured.m.outline.asymmetry(),
            measured.s.aspect,
        );
        assert!(eyes.iter().all(|e| *e <= 0.02), "{character}: design 11.2, the eyes within 2% of IOD: {eyes:?}");
        assert!(lm.eye_right.snapped && lm.eye_left.snapped, "{character}: both eyes snapped");
        // The render's mouth point is the mouth line's centre and the snap's is its top edge: the line's half width
        // apart at most.
        assert!(mouth <= 0.03, "{character}: the mouth within 3% of IOD: {mouth}");
        assert!(chin <= 0.02, "{character}: the measured chin within 2% of IOD of the render's: {chin}");
        assert!(measured.m.outline.coverage() >= 0.6, "{character}: enough of the outline reads");
        assert!(measured.m.outline.asymmetry().is_some_and(|a| a < 0.02), "{character}: a front view is symmetric");
        // The same character compared with itself is identical; under same_look it is within.
        let again = render_and_measure(&harness, character, &format!("measure-kit-{character}-again.png"));
        let l = likeness(measured.side(), again.side(), Tolerances::preset(Compare::SameLook)).unwrap();
        assert!(l.within && l.score == 0.0, "{character}: the same render measures the same: {:?}", l.terms);
    }
}

/// Design 11.2: `face.set_profile` between `round-u` and `egg-oval` moves `aspect` and `O` monotonically with the
/// blend.
#[test]
#[ignore = "#2386: a quarter of the way to egg-oval the profile leaks at the temples"]
fn blending_round_u_toward_egg_oval_moves_aspect_and_the_outline_one_way() {
    let mut measured = Vec::new();
    for step in 0..5 {
        let profile = ShapeProfiles::blend("round-u", "egg-oval", f64::from(step) / 4.0).unwrap();
        let mut root = kit_doc().root().clone();
        let node = &mut root["characters"]["standard"];
        node["parts"]["head"]["params"]["outline"] = serde_json::to_value(profile.outline).unwrap();
        node["face"]["layout"] = serde_json::to_value(profile.layout).unwrap();
        let one =
            render_and_measure(&lavapipe(MemDoc::from_value(root)), "standard", &format!("measure-blend-{step}.png"));
        measured.push(one);
    }
    let aspects: Vec<f64> = measured.iter().map(|m| m.s.aspect).collect();
    let outline: Vec<f64> = measured
        .iter()
        .map(|m| likeness(measured[0].side(), m.side(), Tolerances::preset(Compare::SameLook)).unwrap().terms[0].value)
        .collect();
    println!("round-u to egg-oval: aspect {aspects:.4?}; O from round-u {outline:.4?}");
    let rising = |v: &[f64]| v.windows(2).all(|w| w[1] > w[0]);
    let falling = |v: &[f64]| v.windows(2).all(|w| w[1] < w[0]);
    assert!(rising(&aspects) || falling(&aspects), "aspect moves one way: {aspects:?}");
    assert!(rising(&outline), "O grows with the blend: {outline:?}");
    // The blend's last step is a different head: outside same_look, so the gate would catch it.
    let far = likeness(measured[0].side(), measured[4].side(), Tolerances::preset(Compare::SameLook)).unwrap();
    assert!(!far.within, "{:?}", far.terms);
}
