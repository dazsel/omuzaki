//! The Live workspace (#305): its op calls go through the real executor and the stage's own `perform.*` ops over a
//! fake `PerformHost`, so the names and arguments are the ones the ops on main declare; controls whose ops aren't on
//! main are disabled and say why; the panel is keyboard-reachable in reading order, labelled for AccessKit, speaks only
//! through its message table, and has at least four hotkey rows.
//!
//! Set `OMUZAKI_LIVE_EVIDENCE_DIR=/some/dir` to write the evidence pictures (idle, calibrating and tracking lost at
//! 1366 × 768 and 1920 × 1080).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, ServicesBuilder, TxId, find,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{
    all_strings, harness_state, press_for, reading_order_violations, tab_through, unlabelled_interactive,
};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::live::model::{CALIBRATE_FRAMES, HOTKEY_ROWS, LiveModel, OP_IMPORT, Ops};
use omuzaki_ui::live::panel::{LivePanel, SharedOutput};
use omuzaki_ui::live::story::{LiveState, StoryRunner};
use omuzaki_ui::output_window::{DEFAULT_KEY, OutputBackdrop, OutputPlan, OutputWindow};

/// The performer as the ops see it, recording what they ask.
#[derive(Default)]
struct FakeHost {
    status: Mutex<Option<PerformStatus>>,
    calibrations: Mutex<Vec<usize>>,
    changes: Mutex<Vec<RetargetChange>>,
}

impl FakeHost {
    fn set(&self, state: LiveState) {
        *self.status.lock().unwrap() = Some(state.status());
    }
}

impl PerformHost for FakeHost {
    fn calibrate(&self, frames: usize) -> Result<(), PerformError> {
        self.calibrations.lock().unwrap().push(frames);
        Ok(())
    }

    fn set_retarget(&self, change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        self.changes.lock().unwrap().push(change);
        Ok(RetargetReceipt { seq: 1, requested: RetargetConfig::default() })
    }

    fn set_calibration(&self, _calibration: Calibration) -> Result<(), PerformError> {
        Ok(())
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Webcam)
    }

    fn status(&self) -> PerformStatus {
        self.status.lock().unwrap().clone().unwrap_or_else(|| LiveState::Idle.status())
    }
}

/// The real executor with the fake host in its services, recording every request on its way through.
struct Recording {
    inner: Executor,
    requests: Mutex<Vec<(String, Value)>>,
}

impl Recording {
    fn new(host: Arc<FakeHost>) -> Self {
        omuzaki_ui::link();
        let ops = ["perform.calibrate", "perform.set_retarget", "perform.status"].map(|op| find(op).expect("linked"));
        let services = ServicesBuilder::new().insert::<dyn PerformHost>(host).build();
        let inner = Executor::with_registrations(Box::new(MemDoc::new()), Arc::new(FakeClock::new(Seconds(0.0))), ops)
            .expect("an executor")
            .with_services(services);
        Self { inner, requests: Mutex::new(Vec::new()) }
    }

    fn ops_run(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
    }

    fn last(&self, op: &str) -> Option<Value> {
        self.requests.lock().unwrap().iter().rev().find(|(name, _)| name == op).map(|(_, args)| args.clone())
    }
}

impl Runner for Recording {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.requests.lock().unwrap().push((req.op.clone(), req.args.clone()));
        self.inner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }
    fn subscribe(&self, grants: &Grants) -> Result<std::sync::mpsc::Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

struct World<R: Runner> {
    runner: R,
    panel: LivePanel,
    actor: Actor,
}

fn person() -> Actor {
    Actor::Person { name: "performer".into() }
}

/// A 1366 × 768 harness drawing the workspace over `runner`.
fn workspace<R: Runner + 'static>(runner: R, panel: LivePanel, size: egui::Vec2) -> Harness<'static, World<R>> {
    harness_state(Settings::default(), size, World { runner, panel, actor: person() }, |ui, world| {
        world.panel.ui(ui, &world.runner, &world.actor);
    })
}

fn live(host: &Arc<FakeHost>) -> Harness<'static, World<Recording>> {
    let runner = Recording::new(Arc::clone(host));
    let panel = LivePanel::new(&runner, None);
    workspace(runner, panel, vec2(1366.0, 1400.0))
}

/// The node named `name` (exactly) with `role`.
fn named<'h, S>(h: &'h Harness<'_, S>, role: Role, name: &str) -> egui_kittest::Node<'h> {
    let found = h.get_all_by_role(role).find(|n| n.accesskit_node().label().as_deref() == Some(name));
    found.unwrap_or_else(|| panic!("no {role:?} named {name}"))
}

fn is_disabled<S>(h: &Harness<'_, S>, role: Role, name: &str) -> bool {
    named(h, role, name).accesskit_node().is_disabled()
}

#[test]
fn it_reads_perform_status_and_shows_the_calibration_in_progress() {
    let host = Arc::new(FakeHost::default());
    host.set(LiveState::Calibrating);
    let h = live(&host);
    let runner = &h.state().runner;
    assert_eq!(runner.ops_run().first().map(String::as_str), Some("perform.status"));
    assert_eq!(runner.last("perform.status"), Some(object(&[])));
    assert!(all_strings(&h).iter().any(|s| s == "Calibrating: 112 of 300 frames"), "the progress line shows");
    assert!(is_disabled(&h, Role::Button, "Calibrate"), "no second capture while one runs");
    let model = h.state().panel.model();
    assert_eq!(model.tracking(), Some(Tracking::Tracking));
    assert!((model.smoothing() - 0.35).abs() < 1e-6, "the slider follows the applied smoothing");
}

#[test]
fn calibrate_runs_perform_calibrate_with_the_frames_it_declares() {
    let host = Arc::new(FakeHost::default());
    host.set(LiveState::TrackingLost);
    let mut h = live(&host);
    named(&h, Role::Button, "Calibrate").click();
    h.run_steps(2);
    let args = h.state().runner.last("perform.calibrate").expect("calibrate ran");
    assert_eq!(args, object(&[("frames", Value::from(CALIBRATE_FRAMES))]));
    // The op on main decoded those arguments and reached the performer.
    assert_eq!(*host.calibrations.lock().unwrap(), vec![300]);
    assert!((find("perform.calibrate").expect("registered").decode)(args).is_ok());
}

#[test]
fn the_smoothing_slider_commits_through_perform_set_retarget() {
    let host = Arc::new(FakeHost::default());
    host.set(LiveState::TrackingLost);
    let mut h = live(&host);
    named(&h, Role::Slider, "Smoothing").focus();
    h.run_steps(1);
    press_for(&mut h, Modifiers::NONE, Key::ArrowRight, 3);
    let args = h.state().runner.last("perform.set_retarget").expect("set_retarget ran");
    let smoothing = args["smoothing"].as_f64().expect("a number");
    assert!((smoothing - 0.40).abs() < 1e-6, "one step up from 0.35: {args}");
    assert_eq!(args.as_object().into_iter().flat_map(|m| m.keys()).count(), 1, "only the smoothing changes: {args}");
    let changes = host.changes.lock().unwrap();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].smoothing.is_some_and(|s| (s - 0.4).abs() < 1e-6));
}

#[test]
fn controls_whose_ops_are_not_on_main_are_disabled_with_not_available_yet() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    for (role, name) in [
        (Role::Button, "Choose a camera"),
        (Role::ComboBox, "Camera"),
        (Role::CheckBox, "Mirrored preview"),
        (Role::Button, "Add hotkey"),
        (Role::Button, "Remove Happy"),
    ] {
        assert!(is_disabled(&h, role, name), "{name} is disabled");
    }
    // The reason shows on keyboard focus: the tooltip says "Not available yet".
    named(&h, Role::Button, "Add hotkey").focus();
    h.run_steps(4);
    assert!(
        all_strings(&h).iter().any(|s| s.starts_with("Not available yet: binding hotkeys")),
        "the tooltip says why"
    );
    named(&h, Role::Button, "Choose a camera").focus();
    h.run_steps(4);
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Not available yet: choosing a camera")));
    // Nothing but the status read ran: no op was called under a guessed name.
    assert!(h.state().runner.ops_run().iter().all(|op| op == "perform.status"));
}

#[test]
fn a_build_without_the_perform_ops_says_so_and_has_no_drop_target_without_io_import() {
    let h = workspace(NoOps, LivePanel::new(&NoOps, None), vec2(1366.0, 1400.0));
    assert!(is_disabled(&h, Role::Button, "Calibrate"));
    assert!(is_disabled(&h, Role::Slider, "Smoothing"));
    assert!(!all_strings(&h).iter().any(|s| s.contains("Drop a VRM")), "absent, not broken");
    assert!(all_strings(&h).iter().any(|s| s == "Waiting for the performer"));
}

#[test]
fn the_drop_target_is_there_when_io_import_is_and_a_drop_runs_it() {
    let ops = Ops { import: true, ..Ops::default() };
    let mut h = workspace(NoOps, LivePanel::with_model(LiveModel::with_ops(ops), None), vec2(1366.0, 1400.0));
    assert!(all_strings(&h).iter().any(|s| s == "Drop a VRM here to import it"));
    // The model runs `io.import` with the dropped path (the op lives in `omuzaki-io-ops`, which the app links).
    let recorder = Recorder::default();
    let mut model = LiveModel::with_ops(ops);
    model.import(&recorder, &person(), "/kit/avatar.vrm");
    assert_eq!(
        recorder.0.lock().unwrap().clone(),
        vec![(OP_IMPORT.to_owned(), object(&[("path", Value::from("/kit/avatar.vrm"))]))]
    );
    assert_eq!(model.imported(), Some("avatar.vrm"));
    h.run_steps(1);
}

#[test]
fn the_workspace_is_keyboard_reachable_in_reading_order_and_every_control_is_labelled() {
    let host = Arc::new(FakeHost::default());
    host.set(LiveState::TrackingLost);
    let mut h = live(&host);
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
    let stops = tab_through(&mut h, 80);
    let labels: Vec<&str> = stops.iter().map(|s| s.label.as_str()).collect();
    let violations = reading_order_violations(&stops);
    assert!(violations.is_empty(), "{violations:#?}");
    let at = |name: &str| {
        labels.iter().position(|l| *l == name).unwrap_or_else(|| panic!("{name} is not a stop: {labels:?}"))
    };
    assert!(at("Output window") < at("Character"), "the output toggle sits under the health strip");
    assert!(at("Character") < at("Camera"));
    assert!(at("Camera") < at("Calibrate"));
    assert!(at("Calibrate") < at("Smoothing"));
    assert!(at("Smoothing") < at("Add hotkey"));
}

#[test]
fn the_hotkey_table_has_at_least_four_rows() {
    let host = Arc::new(FakeHost::default());
    let h = live(&host);
    let rows = h
        .get_all_by_role(Role::Button)
        .filter(|n| n.accesskit_node().label().is_some_and(|l| l.contains("trigger: Not bound")))
        .count();
    assert!(rows >= 4 && HOTKEY_ROWS >= 4, "{rows} hotkey rows");
}

#[test]
fn the_workspace_speaks_only_through_its_message_table() {
    let host = Arc::new(FakeHost::default());
    host.set(LiveState::Calibrating);
    let mut h = live(&host);
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    let strings = all_strings(&h);
    assert!(strings.len() > 20, "the scan saw only {strings:?}");
    let english = plain_english(strings.iter().map(String::as_str));
    assert!(english.is_empty(), "plain English in the Live workspace: {english:?}");
}

#[test]
fn the_output_toggle_opens_the_window_and_shows_its_size_and_fallback() {
    let plan = OutputPlan { backdrop: OutputBackdrop::Transparent { fallback: DEFAULT_KEY }, ..OutputPlan::default() };
    let output: SharedOutput =
        Arc::new(Mutex::new(OutputWindow::new(plan, omuzaki_ui::aux_window::Transparency::Fallback)));
    let panel = LivePanel::with_model(LiveState::Idle.panel(&StoryRunner).model().clone(), Some(Arc::clone(&output)));
    let mut h = workspace(StoryRunner, panel, vec2(1366.0, 1400.0));
    let strings = all_strings(&h);
    assert!(strings.iter().any(|s| s == "1920 × 1080 px"), "the size shows");
    assert!(strings.iter().any(|s| s.starts_with("fallback")), "the fallback shows");
    named(&h, Role::Switch, "Output window").click();
    h.run_steps(2);
    assert!(output.lock().unwrap().status().open, "the toggle opened the window");
}

#[test]
fn a_build_without_an_output_window_disables_the_toggle() {
    let model = LiveState::Idle.panel(&StoryRunner).model().clone();
    let h = workspace(StoryRunner, LivePanel::with_model(model, None), vec2(1366.0, 1400.0));
    assert!(is_disabled(&h, Role::Switch, "Output window"));
}

#[test]
fn evidence_pictures_of_idle_calibrating_and_tracking_lost() {
    let dir = std::env::var_os("OMUZAKI_LIVE_EVIDENCE_DIR");
    for state in LiveState::ALL {
        for (w, h) in [(1366.0, 768.0), (1920.0, 1080.0)] {
            let panel = state.panel(&StoryRunner);
            let mut harness = Harness::builder().with_size(vec2(w, h)).wgpu().build_ui_state(
                |ui, world: &mut World<StoryRunner>| {
                    omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
                    let fill = omuzaki_ui::ds::Theme::of(omuzaki_ui::ds::ThemeKind::Dark).bg1;
                    egui::Frame::new().fill(fill).inner_margin(16.0).show(ui, |ui| {
                        world.panel.ui(ui, &world.runner, &world.actor);
                    });
                },
                World { runner: StoryRunner, panel, actor: person() },
            );
            harness.run_steps(4);
            let image = harness.render().expect("a software or hardware adapter renders");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // whole sizes
            let want = (w as u32, h as u32);
            assert_eq!(image.dimensions(), want);
            if let Some(dir) = &dir {
                let name = format!("live-{}-{}x{}.png", state.slug(), want.0, want.1);
                image.save(std::path::Path::new(dir).join(name)).expect("the picture saves");
            }
        }
    }
}

/// A runner with no ops at all: a build that links none of the perform ops.
struct NoOps;

/// A runner that records what it is asked and does nothing.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Value)>>);

macro_rules! empty_runner {
    ($t:ty, $record:expr, $answer:expr) => {
        impl Runner for $t {
            fn ops(&self) -> Vec<OpInfo> {
                Vec::new()
            }
            fn run(&self, req: Request) -> Result<Outcome, OpError> {
                let record: fn(&Self, &Request) = $record;
                record(self, &req);
                $answer
            }
            fn begin(&self, _: &str, _: &Actor) -> TxId {
                TxId(0)
            }
            fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
                Ok(())
            }
            fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
                Ok(())
            }
            fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
                Ok(None)
            }
            fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
                Ok(None)
            }
            fn subscribe(&self, _: &Grants) -> Result<std::sync::mpsc::Receiver<Event>, OpError> {
                Ok(std::sync::mpsc::channel().1)
            }
            fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
                Ok(None)
            }
            fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
                Ok(Vec::new())
            }
        }
    };
}

empty_runner!(NoOps, |_, _| {}, Err(OpError::unavailable("document")));
empty_runner!(
    Recorder,
    |this, req| this.0.lock().unwrap().push((req.op.clone(), req.args.clone())),
    Ok(Outcome::new(omuzaki_ops_api::Plan::default()))
);

fn object(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
}

#[test]
fn a_status_that_fails_to_read_clears_and_reports() {
    let mut model = LiveModel::new(&Recorder::default());
    // A runner without `perform.status` is never asked for it.
    model.refresh(&Recorder::default(), &person());
    assert!(model.status().is_none() && model.failure().is_none());
    let mut with_status = LiveModel::with_ops(Ops { status: true, ..Ops::default() });
    with_status.refresh(&Recorder::default(), &person());
    assert_eq!(with_status.failure().map(|f| f.op), Some("perform.status"));
    assert_eq!(with_status.calibration(), None::<CalibrationState>);
}
