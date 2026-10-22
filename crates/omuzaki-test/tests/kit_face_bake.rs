//! The face bake on real kit characters (#2057, FE-09 #33): `character.create` of each template, then `io.export` to
//! VRM 1.0 with the 52 perfect-sync channels, through the real kit, the real face baker and the real VRM exporter, all
//! linked here through the CLI (no implementation crate may depend on the others). The file is read back with the real
//! importer. Set `OMUZAKI_EVIDENCE_DIR` to keep each bake report and the face atlases.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the file the op wrote and keeps evidence beside it")]

use std::sync::Arc;

use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{
    ARKIT_52, BakeReport, ExpressionName, ImageMime, ImportCtx, ImportInput, ImportOptions, PresetExpression,
    Realisation, run_import,
};
use omuzaki_io_ops::ops::export::{Export, ExportResult};
use omuzaki_io_vrm::Vrm1Importer;
use omuzaki_ops_api::{
    Actor, CancelFlag, Grants, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, Plan, ProjectDir, Services,
    ServicesBuilder, find,
};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

struct Signal(CancelFlag);

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.0)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.0.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc<'a>(&'a MemDoc);

impl JobDoc for Doc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(self.0);
    }
}

/// A project holding the kit character `template`, with the licence record `licence.derive` proposes stored.
struct Project {
    doc: MemDoc,
    services: Services,
    dir: tempfile::TempDir,
}

impl Project {
    fn new(template: &str) -> Self {
        omuzaki_test::link();
        let mut project = Self {
            doc: MemDoc::from_value(json!({})),
            services: ServicesBuilder::new().insert::<dyn Store>(Arc::new(MemStore::new())).build(),
            dir: tempfile::tempdir().unwrap(),
        };
        let create = json!({ "id": template, "name": template, "template": template, "seed": 7 });
        project.apply("character.create", create);
        let derived = project.plan("licence.derive", json!({ "character": template }), true).output.unwrap();
        let mut record = derived["record"].clone();
        record["meta"]["authors"] = json!(["Omuzaki kit"]);
        project.apply("licence.set", json!({ "character": template, "record": record }));
        project
    }

    fn plan(&self, op: &str, args: Value, dry: bool) -> Plan {
        let decoded = (find(op).unwrap_or_else(|| panic!("{op} is registered")).decode)(args).unwrap();
        let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
        let ctx = OpCtx::new(&self.doc, &actor, &clock).with_services(&self.services).with_dry_run(dry);
        decoded.plan(&ctx).unwrap_or_else(|e| panic!("{op}: {e:?}"))
    }

    fn apply(&mut self, op: &str, args: Value) {
        let plan = self.plan(op, args, false);
        self.doc.apply(&plan.patch).unwrap();
    }

    /// `io.export` as the executor runs a job.
    fn export(&self, args: Value) -> ExportResult {
        let op: Export = serde_json::from_value(args).unwrap();
        let (actor, clock, grants) = (Actor::Person { name: "tester".into() }, FakeClock::default(), Grants::person());
        let project = ProjectDir::open(self.dir.path()).unwrap();
        let ctx = OpCtx::new(&self.doc, &actor, &clock)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        op.plan(&ctx).unwrap();
        let (doc, signal) = (Doc(&self.doc), Signal(CancelFlag::new()));
        let job = JobCtx::new(JobId(1), &actor, &doc, &signal)
            .with_grants(&grants)
            .with_project_dir(&project)
            .with_services(&self.services);
        serde_json::from_value(op.work(&job).unwrap()).unwrap()
    }
}

/// Writes `bytes` to `OMUZAKI_EVIDENCE_DIR` as `name`, when it is set.
fn keep(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

fn kind(realisation: &Realisation) -> &'static str {
    match realisation {
        Realisation::Morph { .. } => "morph",
        Realisation::Flipbook { .. } => "flipbook",
        Realisation::Colour => "colour",
        Realisation::Mixed => "mixed",
        Realisation::Inert { .. } => "inert",
    }
}

fn bake(template: &str) -> (BakeReport, Vec<u8>) {
    let project = Project::new(template);
    let path = format!("out/{template}.vrm");
    let result = project.export(json!({ "character": template, "format": "vrm1", "path": path, "purpose": "private" }));
    let bytes = std::fs::read(project.dir.path().join(&path)).unwrap();
    (result.bake.expect("the kit character's face is baked"), bytes)
}

#[test]
fn every_kit_template_bakes_its_face_where_the_renderer_draws_it() {
    for template in ["standard", "mini"] {
        let (report, bytes) = bake(template);
        let mut counts = std::collections::BTreeMap::new();
        for channel in &report.channels {
            *counts.entry(kind(&channel.realisation)).or_insert(0) += 1;
        }
        let warnings: Vec<String> = report
            .findings
            .findings
            .iter()
            .filter(|f| f.severity == omuzaki_io_api::Severity::Warning)
            .map(|f| format!("{}: {}", f.rule, f.message))
            .collect();
        println!("{template}: {} channels {counts:?}", report.channels.len());
        for finding in &report.findings.findings {
            println!("  {:?} {}: {}", finding.severity, finding.rule, finding.message);
        }
        keep(&format!("bake-{template}.json"), serde_json::to_string_pretty(&report).unwrap().as_bytes());

        // Every feature finds the head under it, and every morph moves some of it: the face sits where the renderer
        // draws it, not at the head bone (the head's base), which put the eyes and the mouth below the head.
        for rule in ["face.bake_no_patch", "face.bake_morph_empty"] {
            assert!(!warnings.iter().any(|w| w.starts_with(rule)), "{template}: {warnings:#?}");
        }
        // The only channels with no form are the gaze channels, which the look-at block turns.
        let inert: Vec<&str> = report
            .channels
            .iter()
            .filter(|c| matches!(c.realisation, Realisation::Inert { .. }))
            .map(|c| c.channel.as_str())
            .collect();
        assert!(inert.iter().all(|c| c.starts_with("eyeLook")), "{template}: inert {inert:?}");
        assert_eq!(inert.len(), 8, "{template}: inert {inert:?}");

        // The file reads back with every channel, every preset and the atlases.
        let scene = run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ImportCtx::new(&ImportOptions::default()))
            .expect("the file reads back")
            .scene;
        let avatar = scene.avatar.as_ref().expect("a VRM has an avatar");
        for channel in ARKIT_52 {
            assert!(avatar.expressions.contains_key(&ExpressionName::parse(channel).unwrap()), "{template}: {channel}");
        }
        for preset in PresetExpression::ALL {
            assert!(avatar.expressions.contains_key(&ExpressionName::Preset(preset)), "{template}: {preset:?}");
        }
        let atlases: Vec<_> = scene.images.iter().filter(|i| i.name.starts_with("face.")).collect();
        for name in ["face.eye-left", "face.eye-right", "face.mouth", "face.brow-left", "face.brow-right"] {
            assert!(atlases.iter().any(|i| i.name == name), "{template}: {name} is in the file");
        }
        for image in atlases {
            assert_eq!(image.mime, ImageMime::Png);
            keep(&format!("{template}-{}.png", image.name), image.bytes.as_slice());
        }
    }
}
