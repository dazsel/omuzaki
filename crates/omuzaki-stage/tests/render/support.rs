//! The render-op test harness: a document with the mock character, the services a render job needs, and a way to run a
//! job op without an executor. The render-op tests share it through a `#[path]` attribute.

// Every test file that includes this module uses a different part of it.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use omuzaki_char_api::{BuildServices, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::{CHARACTER_JSON, mock_variants};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, CancelFlag, JobCtx, JobDoc, JobId, JobSignal, Op, OpCtx, OpError, Plan, Services, ServicesBuilder,
};
use omuzaki_render_api::Renderer;
use omuzaki_render_mock::MockRenderer;
use omuzaki_store_mock::MemStore;

/// A document as a job's worker sees it: a short read under the lock, here with no lock at all.
pub struct TestDoc(pub MemDoc);

impl JobDoc for TestDoc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

/// A job signal that never reports, is never cancelled and accepts all spending.
pub struct TestSignal;

impl JobSignal for TestSignal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancel_flag(&self) -> CancelFlag {
        CancelFlag::new()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

/// The document with the mock character at `characters/mock`, its variants under `characters/mock/variants`.
pub fn mock_doc() -> TestDoc {
    let mut character: Value = serde_json::from_str(CHARACTER_JSON).expect("the fixture parses");
    let variants = serde_json::to_value(mock_variants().expect("the variants parse")).expect("variants serialise");
    character.as_object_mut().expect("the character is a table").insert("variants".to_owned(), variants);
    TestDoc(MemDoc::from_value(serde_json::json!({ "characters": { "mock": character } })))
}

/// A service the harness can leave out, to test what an op does without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The renderer (`Mutex<Box<dyn Renderer>>`).
    Renderer,
    /// The character builder (`dyn CharacterBuilder`).
    Builder,
    /// The build services (`BuildServices`).
    BuildServices,
    /// The asset store (`dyn Store`).
    Store,
}

/// The services a render job runs with, and handles on the renderer and the store so a test can read them back.
pub struct Harness {
    services: Services,
    /// The renderer the services hand out.
    pub renderer: Arc<Mutex<Box<dyn Renderer>>>,
    /// The store the services hand out.
    pub store: Arc<MemStore>,
    doc: TestDoc,
}

impl Harness {
    /// A harness over `doc` with every service injected.
    pub fn new(doc: TestDoc) -> Self {
        Self::without(doc, None)
    }

    /// A harness over `doc` that leaves out `missing`, if given.
    pub fn without(doc: TestDoc, missing: Option<Missing>) -> Self {
        Self::build(doc, missing, Box::new(MockRenderer::new()))
    }

    /// A harness over `doc` with every service, rendering on `renderer` (the wgpu one, for the GPU tests).
    pub fn on(doc: TestDoc, renderer: Box<dyn Renderer>) -> Self {
        Self::build(doc, None, renderer)
    }

    /// A harness over `doc` with every service, whose past states and other branches `history` reads (#400).
    pub fn with_history(doc: TestDoc, history: Arc<dyn omuzaki_history_api::state::HistoryView>) -> Self {
        Self::build_with(doc, None, Box::new(MockRenderer::new()), ServicesBuilder::new().insert(history))
    }

    fn build(doc: TestDoc, missing: Option<Missing>, renderer: Box<dyn Renderer>) -> Self {
        Self::build_with(doc, missing, renderer, ServicesBuilder::new())
    }

    fn build_with(
        doc: TestDoc,
        missing: Option<Missing>,
        renderer: Box<dyn Renderer>,
        builder: ServicesBuilder,
    ) -> Self {
        let renderer: Arc<Mutex<Box<dyn Renderer>>> = Arc::new(Mutex::new(renderer));
        let store = Arc::new(MemStore::new());
        let mut builder = builder;
        if missing != Some(Missing::Renderer) {
            builder = builder.insert::<Mutex<Box<dyn Renderer>>>(renderer.clone());
        }
        if missing != Some(Missing::Builder) {
            builder = builder.insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder));
        }
        if missing != Some(Missing::BuildServices) {
            builder = builder.insert::<BuildServices>(Arc::new(BuildServices::default()));
        }
        if missing != Some(Missing::Store) {
            builder = builder.insert::<dyn omuzaki_store_api::Store>(store.clone());
        }
        Self { services: builder.build(), renderer, store, doc }
    }

    /// Runs `f` with a job context over the harness's document and services.
    pub fn with_job<T>(&self, f: impl FnOnce(&JobCtx<'_>) -> T) -> T {
        let actor = Actor::Person { name: "test".to_owned() };
        let signal = TestSignal;
        let job = JobCtx::new(JobId(1), &actor, &self.doc, &signal).with_services(&self.services);
        f(&job)
    }

    /// The document the jobs read.
    pub fn doc(&self) -> &MemDoc {
        &self.doc.0
    }

    /// The document, to edit between jobs.
    pub fn doc_mut(&mut self) -> &mut MemDoc {
        &mut self.doc.0
    }

    /// Runs `op` to the end as the executor does: plans it as a dry run, runs its work, finishes it against the
    /// document and applies the finished patch. Returns the dry run's plan and the finished one.
    ///
    /// # Errors
    /// Whatever planning, the work or finishing fails with.
    pub fn run_to_end<O: Op>(&mut self, op: &O) -> Result<(Plan, Plan), OpError> {
        let (plan, value) = self.run(op)?;
        let actor = Actor::Person { name: "test".to_owned() };
        let clock = FakeClock::new(Seconds(0.0));
        let ctx = OpCtx::new(&self.doc.0, &actor, &clock).with_services(&self.services);
        let finished = op.finish(&ctx, value)?;
        omuzaki_doc_api::DocWrite::apply(&mut self.doc.0, &finished.patch)
            .map_err(|e| OpError::internal(format!("applying the finished patch: {e}")))?;
        Ok((plan, finished))
    }

    /// Plans `op` as a dry run, then runs its work, as a job would. Returns the plan and the work's result.
    ///
    /// # Errors
    /// Whatever planning or the work fails with.
    pub fn run<O: Op>(&self, op: &O) -> Result<(Plan, Value), OpError> {
        let actor = Actor::Person { name: "test".to_owned() };
        let clock = FakeClock::new(Seconds(0.0));
        let ctx = OpCtx::new(&self.doc.0, &actor, &clock).with_services(&self.services).with_dry_run(true);
        let plan = op.plan(&ctx)?;
        let value = self.with_job(|job| op.work(job))?;
        Ok((plan, value))
    }
}
