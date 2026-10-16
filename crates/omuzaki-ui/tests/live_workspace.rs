//! The Live workspace (#305): its op calls go through the real executor and the stage's own `perform.*` ops over a
//! fake `PerformHost`, so the names and arguments are the ones the ops on main declare; controls whose ops aren't on
//! main are disabled and say why; the panel is keyboard-reachable in reading order, labelled for AccessKit, speaks only
//! through its message table, and has at least four hotkey rows, which bind and unbind through the trigger ops.
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
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Event, Grants, Mode, OpError, OpInfo, OpRecord, Outcome, Request, Runner, ServicesBuilder, TxId, find,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use omuzaki_stage::perform::input::{
    CameraEntry, InputError, InputHost, InputState, Permission, PreviewState, SourceKind,
};
use omuzaki_stage::perform::triggers::{BindOutcome, TriggerBinding, TriggerInfo};
use omuzaki_stream_api::window::WindowHost;
use omuzaki_track_api::camera::CameraId;
use omuzaki_ui::aux_window::{PhysicalSize, Transparency};
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{
    all_strings, harness_state, press_for, reading_order_violations, tab_through, unlabelled_interactive,
};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::live::model::{
    CALIBRATE_FRAMES, HOTKEY_ROWS, LiveModel, OP_IMPORT, OP_INPUT_BIND_CAMERA, OP_INPUT_PREVIEW,
    OP_INPUT_REQUEST_CAMERA, OP_INPUT_SELECT, OP_INPUT_STATE, Ops,
};
use omuzaki_ui::live::panel::LivePanel;
use omuzaki_ui::live::story::{LiveState, StoryRunner};
use omuzaki_ui::output_window::{OutputHost, OutputPlan, OutputWindow};

/// The performer as the ops see it, recording what they ask.
#[derive(Default)]
struct FakeHost {
    status: Mutex<Option<PerformStatus>>,
    calibrations: Mutex<Vec<usize>>,
    changes: Mutex<Vec<RetargetChange>>,
    triggers: Mutex<Vec<TriggerBinding>>,
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

    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::starter()
    }

    fn bind_trigger(&self, binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        let mut book = self.triggers.lock().unwrap();
        let replaced = book.iter().position(|b| b.key.eq_ignore_ascii_case(&binding.key));
        let slot = if let Some(slot) = replaced {
            book[slot] = binding;
            slot
        } else {
            book.push(binding);
            book.len() - 1
        };
        Ok(BindOutcome { slot, replaced: replaced.is_some() })
    }

    fn bind_trigger_checked(&self, binding: TriggerBinding) -> Result<BindOutcome, PerformError> {
        if !self.expressions().0.contains_key(&binding.expression) {
            return Err(PerformError::UnknownExpression);
        }
        self.bind_trigger(binding)
    }

    fn unbind_trigger(&self, key: &str) -> Result<bool, PerformError> {
        let mut book = self.triggers.lock().unwrap();
        let before = book.len();
        book.retain(|b| !b.key.eq_ignore_ascii_case(key));
        Ok(book.len() < before)
    }

    fn triggers(&self) -> Vec<TriggerInfo> {
        let book = self.triggers.lock().unwrap();
        book.iter().map(|binding| TriggerInfo { binding: binding.clone(), active: false, missing: false }).collect()
    }
}

/// The tracking input as the stage's `input.*` ops see it, kept in memory: two cameras, and the same refusals as the
/// stage's `LiveInput` (a camera it doesn't list, a mirror with nothing to capture). A dry run changes nothing.
struct FakeInput(Mutex<InputState>);

impl FakeInput {
    /// Two cameras, none bound, the life layer driving the face, the permission as given.
    fn new(permission: Permission) -> Arc<Self> {
        let camera =
            |id: &str, name: &str| CameraEntry { id: CameraId(id.to_owned()), name: name.to_owned(), bound: false };
        Arc::new(Self(Mutex::new(InputState {
            source: SourceKind::LifeLayer,
            camera: None,
            camera_present: false,
            preview: PreviewState::Off,
            permission,
            cameras: vec![camera("usb:0", "Logitech C920"), camera("usb:1", "Integrated Camera")],
            tracking: Tracking::NoFrames,
        })))
    }

    fn view(&self) -> InputState {
        self.0.lock().unwrap().clone()
    }
}

impl InputHost for FakeInput {
    fn select(&self, source: SourceKind, mode: Mode) -> Result<SourceKind, InputError> {
        let mut view = self.0.lock().unwrap();
        let before = view.source;
        if mode == Mode::Apply {
            view.source = source;
        }
        Ok(before)
    }

    fn bind_camera(&self, camera: Option<CameraId>, mode: Mode) -> Result<Option<CameraId>, InputError> {
        let mut view = self.0.lock().unwrap();
        if let Some(id) = camera.as_ref().filter(|id| !view.cameras.iter().any(|c| &c.id == *id)) {
            return Err(InputError::no_camera(id.clone()));
        }
        let before = view.camera.clone();
        if mode == Mode::Apply {
            for entry in &mut view.cameras {
                entry.bound = camera.as_ref() == Some(&entry.id);
            }
            view.camera_present = camera.is_some();
            view.camera = camera;
        }
        Ok(before)
    }

    fn preview(&self, on: bool, mode: Mode) -> Result<bool, InputError> {
        let mut view = self.0.lock().unwrap();
        let before = view.preview.is_on();
        let next = match (on, view.camera.clone()) {
            (false, _) => PreviewState::Off,
            (true, Some(camera)) if view.source == SourceKind::Webcam => PreviewState::On { camera },
            (true, None) => return Err(InputError::NoCameraBound),
            (true, Some(_)) => return Err(InputError::NothingToCapture),
        };
        if mode == Mode::Apply {
            view.preview = next;
        }
        Ok(before)
    }

    fn request_camera(&self) -> Permission {
        let mut view = self.0.lock().unwrap();
        view.permission = Permission::Granted;
        view.permission
    }

    fn state(&self) -> InputState {
        self.view()
    }
}

/// The real executor with the fake performer and the app's real output window host in its services, plus (when given)
/// the `input.*` surface, recording every request on its way through.
struct Recording {
    inner: Executor,
    requests: Mutex<Vec<(String, Value)>>,
    window: OutputHost,
}

impl Recording {
    /// The perform, trigger and output window ops, without the `input.*` ops: a build that lacks them.
    fn new(host: Arc<FakeHost>) -> Self {
        Self::build(host, None)
    }

    /// The same, with the stage's `input.*` ops over `input`.
    fn with_input(host: Arc<FakeHost>, input: &Arc<FakeInput>) -> Self {
        Self::build(host, Some(input))
    }

    fn build(host: Arc<FakeHost>, input: Option<&Arc<FakeInput>>) -> Self {
        omuzaki_ui::link();
        let mut names = vec![
            "perform.calibrate",
            "perform.set_retarget",
            "perform.status",
            "trigger.bind",
            "trigger.unbind",
            "trigger.list",
            "output.window.open",
            "output.window.close",
            "output.window.status",
        ];
        if input.is_some() {
            names.extend([
                OP_INPUT_STATE,
                OP_INPUT_SELECT,
                OP_INPUT_BIND_CAMERA,
                OP_INPUT_PREVIEW,
                OP_INPUT_REQUEST_CAMERA,
            ]);
        }
        let ops: Vec<_> = names.into_iter().map(|op| find(op).expect("linked")).collect();
        let window = OutputHost::new(Transparency::Fallback, PhysicalSize::FULL_HD);
        let mut services =
            ServicesBuilder::new().insert::<dyn PerformHost>(host).insert::<dyn WindowHost>(Arc::new(window.clone()));
        if let Some(input) = input {
            services = services.insert::<dyn InputHost>(Arc::clone(input) as Arc<dyn InputHost>);
        }
        let inner = Executor::with_registrations(Box::new(MemDoc::new()), Arc::new(FakeClock::new(Seconds(0.0))), ops)
            .expect("an executor")
            .with_services(services.build());
        Self { inner, requests: Mutex::new(Vec::new()), window }
    }

    /// One frame of the app's UI thread: the window the ops asked for opens or closes, and says what it is.
    fn ui_frame(&self, window: &mut OutputWindow) {
        self.window.take_requests(window);
        self.window.publish(window, 1.0);
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
    live_on(Recording::new(Arc::clone(host)))
}

fn live_on(runner: Recording) -> Harness<'static, World<Recording>> {
    let panel = LivePanel::new(&runner);
    workspace(runner, panel, vec2(1366.0, 1600.0))
}

/// Reads the status again now, as the panel does every quarter second.
fn reread(h: &mut Harness<'_, World<Recording>>) {
    let world = h.state_mut();
    world.panel.model_mut().refresh(&world.runner, &world.actor);
}

/// Chooses `option` in the dropdown named `name`, as a screen reader (or an agent driving the UI) does.
fn choose(h: &mut Harness<'_, World<Recording>>, name: &str, option: &str) {
    let (target_node, target_tree) = named(h, Role::ComboBox, name).accesskit_node().locate();
    let data = Some(egui::accesskit::ActionData::Value(option.into()));
    let action = egui::accesskit::Action::SetValue;
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action,
        target_tree,
        target_node,
        data,
    }));
    h.run_steps(3);
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
fn controls_whose_ops_this_build_lacks_are_disabled_and_say_which_op() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    for (role, name) in [(Role::ComboBox, "Camera"), (Role::Switch, "Track my face")] {
        assert!(is_disabled(&h, role, name), "{name} is disabled");
    }
    // The reason shows on keyboard focus: the tooltip names the missing op.
    named(&h, Role::Switch, "Track my face").focus();
    h.run_steps(4);
    assert!(all_strings(&h).iter().any(|s| s == "Not available: this build has no input.state op"));
    // Nothing but the reads ran: no op was called under a name this build doesn't have.
    let reads = ["perform.status", "trigger.list", "output.window.status"];
    assert!(
        h.state().runner.ops_run().iter().all(|op| reads.contains(&op.as_str())),
        "{:?}",
        h.state().runner.ops_run()
    );
}

#[test]
fn a_build_without_the_perform_ops_says_so_and_has_no_drop_target_without_io_import() {
    let h = workspace(NoOps, LivePanel::new(&NoOps), vec2(1366.0, 1400.0));
    assert!(is_disabled(&h, Role::Button, "Calibrate"));
    assert!(is_disabled(&h, Role::Slider, "Smoothing"));
    assert!(is_disabled(&h, Role::Button, "Happy trigger: Not bound"), "no trigger ops, no binding");
    assert!(is_disabled(&h, Role::Button, "Add hotkey"));
    assert!(is_disabled(&h, Role::Switch, "Output window"), "no window ops, no output window");
    assert!(is_disabled(&h, Role::Switch, "Keep on top"));
    assert!(!all_strings(&h).iter().any(|s| s.contains("Drop a VRM")), "absent, not broken");
    assert!(all_strings(&h).iter().any(|s| s == "Waiting for the performer"));
}

#[test]
fn the_drop_target_is_there_when_io_import_is_and_a_drop_runs_it() {
    let ops = Ops { import: true, ..Ops::default() };
    let mut h = workspace(NoOps, LivePanel::with_model(LiveModel::with_ops(ops)), vec2(1366.0, 1400.0));
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
fn the_output_switch_opens_and_closes_the_window_through_its_ops() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
    assert!(
        all_strings(&h).iter().any(|s| s == "Capture it as the window “Omuzaki Output”"),
        "closed: where to find it"
    );
    named(&h, Role::Switch, "Output window").click();
    h.run_steps(2);
    let args = h.state().runner.last("output.window.open").expect("the switch ran the op");
    assert_eq!(args, object(&[("backdrop", Value::from("key")), ("always_on_top", Value::from(false))]));
    assert!((find("output.window.open").expect("registered").decode)(args).is_ok(), "the op decodes what the UI sends");
    // The app's UI thread opens it on its next frame; the panel's next read shows it.
    h.state().runner.ui_frame(&mut window);
    assert!(window.status().open && !window.plan().always_on_top, "open, and not on top unless asked");
    reread(&mut h);
    h.run_steps(2);
    assert!(all_strings(&h).iter().any(|s| s == "1920 × 1080 px over #00b140"), "{:?}", all_strings(&h));
    assert_eq!(
        named(&h, Role::Switch, "Output window").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "the switch shows the window open"
    );

    named(&h, Role::Switch, "Output window").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last("output.window.close"), Some(object(&[])));
    h.state().runner.ui_frame(&mut window);
    assert!(!window.status().open, "the switch closed it");
}

#[test]
fn backdrop_and_keep_on_top_reopen_an_open_window_and_a_fallback_says_so() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Fallback);
    named(&h, Role::Switch, "Output window").click();
    h.run_steps(2);
    h.state().runner.ui_frame(&mut window);
    reread(&mut h);
    h.run_steps(2);

    named(&h, Role::RadioButton, "Transparent").click();
    h.run_steps(2);
    let args = h.state().runner.last("output.window.open").expect("re-opened");
    assert_eq!(args["backdrop"], "transparent");
    h.state().runner.ui_frame(&mut window);
    reread(&mut h);
    h.run_steps(2);
    // This host has no verified transparent surface: the window took the key, and the panel says so.
    assert!(all_strings(&h).iter().any(|s| s.starts_with("fallback")), "{:?}", all_strings(&h));

    named(&h, Role::Switch, "Keep on top").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last("output.window.open").expect("re-opened")["always_on_top"], true);
    h.state().runner.ui_frame(&mut window);
    assert!(window.plan().always_on_top, "the setting reached the window");
}

#[test]
fn a_minimised_window_says_it_cannot_be_captured() {
    let h = workspace(
        StoryRunner(LiveState::TrackingLost),
        LiveState::TrackingLost.panel(&StoryRunner(LiveState::TrackingLost)),
        vec2(1366.0, 1600.0),
    );
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Minimised")), "{:?}", all_strings(&h));
}

#[test]
fn the_camera_section_binds_a_camera_and_tracks_through_the_input_ops() {
    let host = Arc::new(FakeHost::default());
    let input = FakeInput::new(Permission::Granted);
    let mut h = live_on(Recording::with_input(Arc::clone(&host), &input));
    assert_eq!(h.state().runner.last(OP_INPUT_STATE), Some(object(&[])), "it reads the input");
    assert!(is_disabled(&h, Role::Switch, "Track my face"), "nothing to track from until a camera is chosen");

    choose(&mut h, "Camera", "Logitech C920");
    assert_eq!(h.state().runner.last(OP_INPUT_BIND_CAMERA), Some(object(&[("camera", Value::from("usb:0"))])));
    assert_eq!(input.view().camera, Some(CameraId("usb:0".to_owned())));

    named(&h, Role::Switch, "Track my face").click();
    h.run_steps(2);
    let ops = h.state().runner.ops_run();
    let reads = ["input.state", "perform.status", "trigger.list", "output.window.status"];
    let actions: Vec<&str> = ops.iter().map(String::as_str).filter(|op| !reads.contains(op)).collect();
    assert_eq!(actions, ["input.bind_camera", "input.select", "input.preview"], "the webcam source, then the capture");
    assert_eq!(h.state().runner.last(OP_INPUT_SELECT), Some(object(&[("source", Value::from("webcam"))])));
    assert_eq!(h.state().runner.last(OP_INPUT_PREVIEW), Some(object(&[("on", Value::from(true))])));
    assert!(all_strings(&h).iter().any(|s| s == "Tracking from Logitech C920"), "{:?}", all_strings(&h));

    named(&h, Role::Switch, "Track my face").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last(OP_INPUT_PREVIEW), Some(object(&[("on", Value::from(false))])));
    assert_eq!(input.view().preview, PreviewState::Off);
}

#[test]
fn an_unplugged_camera_says_so_and_retry_starts_it_again() {
    let host = Arc::new(FakeHost::default());
    let input = FakeInput::new(Permission::Granted);
    {
        let mut view = input.0.lock().unwrap();
        view.source = SourceKind::Webcam;
        view.camera = Some(CameraId("usb:0".to_owned()));
        view.camera_present = false;
        view.preview = PreviewState::On { camera: CameraId("usb:0".to_owned()) };
    }
    let mut h = live_on(Recording::with_input(host, &input));
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Camera unplugged")));
    named(&h, Role::Button, "Retry").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last(OP_INPUT_PREVIEW), Some(object(&[("on", Value::from(true))])));
}

#[test]
fn the_camera_permission_is_asked_through_its_op_and_a_refusal_says_where_to_allow_it() {
    let host = Arc::new(FakeHost::default());
    let input = FakeInput::new(Permission::Unknown);
    let mut h = live_on(Recording::with_input(Arc::clone(&host), &input));
    named(&h, Role::Button, "Allow the camera").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last(OP_INPUT_REQUEST_CAMERA), Some(object(&[])));
    assert_eq!(input.view().permission, Permission::Granted);

    let denied = FakeInput::new(Permission::Denied);
    let h = live_on(Recording::with_input(host, &denied));
    assert!(all_strings(&h).iter().any(|s| s.starts_with("Camera access is off")));
}

#[test]
fn evidence_pictures_of_idle_calibrating_and_tracking_lost() {
    let dir = std::env::var_os("OMUZAKI_LIVE_EVIDENCE_DIR");
    let dock = live_dock_share();
    for state in LiveState::ALL {
        for (w, h) in [(1366.0, 768.0), (1920.0, 1080.0)] {
            let panel = state.panel(&StoryRunner(state));
            let mut harness = Harness::builder().with_size(vec2(w, h)).wgpu().build_ui_state(
                move |ui, world: &mut World<StoryRunner>| {
                    omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
                    let theme = omuzaki_ui::ds::Theme::of(omuzaki_ui::ds::ThemeKind::Dark);
                    // The Perform workspace's default layout: the Live panel docked right at its registered share,
                    // beside the viewport's place (the viewport itself is the edit panels' change).
                    let panel = egui::Frame::new().fill(theme.bg1).inner_margin(12.0);
                    egui::Panel::right("live.dock")
                        .exact_size((w * dock).round())
                        .frame(panel)
                        .show(ui, |ui| world.panel.ui(ui, &world.runner, &world.actor));
                    egui::CentralPanel::default().frame(egui::Frame::new().fill(theme.bg0)).show(ui, |_| {});
                },
                World { runner: StoryRunner(state), panel, actor: person() },
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

/// The Live panel's share of the width in the Perform workspace's registered default layout.
fn live_dock_share() -> f32 {
    use omuzaki_ui_api::layout::Node;
    omuzaki_ui::link();
    let host = omuzaki_ui_mock::MockHost::new();
    let perform = host.registry().workspace("perform").expect("registered");
    let Node::Split { children, shares, .. } = (perform.default_layout)() else { panic!("a split") };
    let live = children.iter().position(|child| *child == Node::pane("live")).expect("the Live panel is in it");
    shares[live] / shares.iter().sum::<f32>()
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

#[test]
fn a_trigger_button_binds_the_next_key_press_and_remove_unbinds_it() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    assert!(is_disabled(&h, Role::Button, "Remove Happy"), "nothing to remove yet");
    named(&h, Role::Button, "Happy trigger: Not bound").click();
    h.run_steps(2);
    named(&h, Role::Button, "Press a key for Happy; Escape cancels").focus();
    h.run_steps(1);
    press_for(&mut h, Modifiers::NONE, Key::F1, 1);
    h.run_steps(2);
    let args = h.state().runner.last("trigger.bind").expect("trigger.bind ran");
    assert_eq!(args, object(&[("key", Value::from("F1")), ("expression", Value::from("happy"))]));
    assert!((find("trigger.bind").expect("registered").decode)(args).is_ok(), "the op decodes what the UI sends");
    assert_eq!(host.triggers().len(), 1, "the binding reached the performer");
    named(&h, Role::Button, "Happy trigger: F1");

    // A chord, on another row.
    named(&h, Role::Button, "Angry trigger: Not bound").click();
    h.run_steps(2);
    press_for(&mut h, Modifiers::CTRL, Key::F2, 1);
    h.run_steps(2);
    named(&h, Role::Button, "Angry trigger: Ctrl+F2");

    // Rebinding a row moves its key: the old one is let go.
    named(&h, Role::Button, "Happy trigger: F1").click();
    h.run_steps(2);
    press_for(&mut h, Modifiers::NONE, Key::F3, 1);
    h.run_steps(2);
    assert_eq!(h.state().runner.last("trigger.unbind"), Some(object(&[("key", Value::from("F1"))])));
    named(&h, Role::Button, "Happy trigger: F3");

    // Escape ends the wait without binding anything.
    let binds = h.state().runner.ops_run().iter().filter(|op| *op == "trigger.bind").count();
    named(&h, Role::Button, "Sad trigger: Not bound").click();
    h.run_steps(2);
    press_for(&mut h, Modifiers::NONE, Key::Escape, 1);
    h.run_steps(2);
    named(&h, Role::Button, "Sad trigger: Not bound");
    assert_eq!(h.state().runner.ops_run().iter().filter(|op| *op == "trigger.bind").count(), binds);

    named(&h, Role::Button, "Remove Happy").click();
    h.run_steps(2);
    assert_eq!(h.state().runner.last("trigger.unbind"), Some(object(&[("key", Value::from("F3"))])));
    named(&h, Role::Button, "Happy trigger: Not bound");
    assert_eq!(host.triggers().len(), 1, "only Angry's key is left");
}

#[test]
fn add_hotkey_shows_the_next_mood_until_every_mood_has_a_row() {
    let host = Arc::new(FakeHost::default());
    let mut h = live(&host);
    assert!(!all_strings(&h).iter().any(|s| s == "Relaxed"));
    named(&h, Role::Button, "Add hotkey").click();
    h.run_steps(2);
    named(&h, Role::Button, "Relaxed trigger: Not bound");
    assert!(is_disabled(&h, Role::Button, "Add hotkey"), "every mood has a row");
}

#[test]
fn a_binding_made_elsewhere_gets_its_own_row() {
    let host = Arc::new(FakeHost::default());
    let binding = TriggerBinding {
        key: "F9".into(),
        expression: omuzaki_face_api::ExpressionId::new("ee").unwrap(),
        mode: omuzaki_anim::TriggerMode::Hold,
    };
    host.bind_trigger(binding).unwrap();
    let h = live(&host);
    named(&h, Role::Button, "ee trigger: F9");
}
