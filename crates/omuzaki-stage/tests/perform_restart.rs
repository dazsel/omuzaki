//! The V0 settings survive a restart (V0 exit 9, #2073): every key is set through the ops, saved to the real settings
//! store in a folder, the whole app is dropped, and a new one on the same folder restores them exactly.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]
#![allow(clippy::disallowed_methods, reason = "the test edits the settings files by hand, as a person would")]

#[path = "live/support.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;

use omuzaki_anim::MAX_TRIGGERS;
use omuzaki_anim_api::{Calibration, ChannelBuf, DeviceProfile, HumanoidPose, RetargetConfig};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::{Actor, Mode, OpCtx, OpError, Plan, Services, ServicesBuilder, find};
use omuzaki_settings::{Paths, Store};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingScope};
use omuzaki_settings_api::registry;
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt};
use omuzaki_settings_api::setting::Setting;
use omuzaki_stage::perform::Performer;
use omuzaki_stage::perform::host::{CalibrationState, PerformControl, PerformHandle, PerformHost, perform_channel};
use omuzaki_stage::perform::input::{InputCamera, InputHost, InputSource, LiveInput, NoPrompt, SourceKind, SourceSlot};
use omuzaki_stage::perform::settings::{
    PerformRetarget, PerformTriggers, Restored, RetargetPrefs, restore, save, save_calibration,
};
use omuzaki_track::calibration::{CalibrationError, CalibrationStore, calibration_file};
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::TrackerConfig;
use serde_json::{Value, json};
use support::{Cameras, face_for, mid_step, performer};

const CHARACTER: &str = "kiri";

/// One run of the app: the performer with its handle, the input over scripted cameras, and the settings store.
struct App {
    handle: PerformHandle,
    control: PerformControl,
    performer: Performer,
    input: Arc<LiveInput>,
    slot: Arc<SourceSlot>,
    store: Arc<Store>,
    /// The calibration state, in its own file beside the settings (ADR 0026).
    calibrations: CalibrationStore,
    step: u64,
}

impl App {
    /// Starts the app on `dir` with the cameras named `cameras` plugged in.
    fn start(dir: &Path, cameras: &[&str]) -> Self {
        let paths = Paths::new(dir.join("config"), dir.join("data"));
        omuzaki_stream::link(); // registers `output.window`
        let store = Arc::new(Store::open(&paths));
        let calibrations = CalibrationStore::open(calibration_file(paths.data()));
        let mut performer = performer(&HumanoidPose::REST);
        let (handle, mut control) = perform_channel(&performer);
        control.advance(&mut performer, mid_step(0));
        let scripts = cameras.iter().enumerate().map(|(i, id)| (*id, face_for(20.0, i as u64 + 1))).collect();
        let (tracker, _log) = Cameras::new(scripts);
        let slot = Arc::new(SourceSlot::new());
        let perform: Arc<dyn PerformHost> = Arc::new(handle.clone());
        let input = Arc::new(LiveInput::new(
            Box::new(tracker),
            Box::new(NoPrompt),
            TrackerConfig::default(),
            Arc::clone(&slot),
            perform,
        ));
        handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
        let mut app = Self { handle, control, performer, input, slot, store, calibrations, step: 1 };
        app.steps(2);
        app
    }

    /// Runs `n` performer steps, handing over any posted source first, as the live loop does.
    fn steps(&mut self, n: u64) {
        for _ in 0..n {
            self.slot.swap_into(&mut self.performer);
            self.control.advance(&mut self.performer, mid_step(self.step));
            self.step += 1;
        }
    }

    fn services(&self) -> Services {
        let perform: Arc<dyn PerformHost> = Arc::new(self.handle.clone());
        let settings: Arc<dyn Settings> = self.store.clone();
        let input: Arc<dyn InputHost> = self.input.clone();
        ServicesBuilder::new()
            .insert::<dyn PerformHost>(perform)
            .insert::<dyn InputHost>(input)
            .insert::<dyn Settings>(settings)
            .build()
    }

    /// Runs `op` as the person.
    fn op(&self, op: &str, args: Value) -> Result<Plan, OpError> {
        let op = (find(op).expect("the op registers itself").decode)(args)?;
        let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
        let actor = Actor::Person { name: "person".into() };
        let services = self.services();
        op.plan(&OpCtx::new(&doc, &actor, &clock).with_services(&services))
    }

    fn person() -> Actor {
        Actor::Person { name: "person".into() }
    }

    /// What `restore` gives back on this app, after letting the performer take the requests.
    fn restore(&mut self, character: &str) -> Restored {
        let restored =
            restore(&self.handle, &*self.input, &*self.store, &self.calibrations, character, DeviceProfile::Webcam);
        self.steps(8);
        restored
    }

    /// Everything the person can see of the live state, to compare across a restart.
    fn view(&self) -> View {
        let state = self.input.state();
        View {
            retarget: self.handle.status().retarget,
            triggers: self
                .handle
                .triggers()
                .into_iter()
                .map(|t| (t.binding.key, t.binding.expression.as_str().to_owned()))
                .collect(),
            source: state.source,
            camera: state.camera,
            calibration: self.handle.calibration(),
        }
    }

    /// Ends the app: the store writes its files and every thread stops.
    fn quit(self) {
        self.store.flush().unwrap();
    }
}

#[derive(Debug, PartialEq)]
struct View {
    retarget: RetargetConfig,
    triggers: Vec<(String, String)>,
    source: SourceKind,
    camera: Option<CameraId>,
    calibration: Calibration,
}

/// The first run: the person sets every V0 key through the ops and the app saves them.
fn first_run(dir: &Path) -> View {
    let mut app = App::start(dir, &["a", "b"]);
    app.op("perform.set_retarget", json!({ "smoothing": 0.7, "mirror": false })).unwrap();
    app.op("trigger.bind", json!({ "key": "F1", "expression": "happy" })).unwrap();
    app.op("trigger.bind", json!({ "key": "F2", "expression": "angry", "mode": "hold" })).unwrap();
    // Capture a calibration from the camera: the source is the webcam while it runs.
    app.op("input.select", json!({ "source": "webcam" })).unwrap();
    app.op("input.bind_camera", json!({ "camera": "b" })).unwrap();
    app.op("input.preview", json!({ "on": true })).unwrap();
    app.steps(40);
    app.handle.calibrate(30).unwrap();
    for _ in 0..40 {
        app.steps(10);
        if matches!(app.handle.status().calibration, CalibrationState::Done { .. }) {
            break;
        }
    }
    assert!(matches!(app.handle.status().calibration, CalibrationState::Done { .. }), "the calibration finished");
    app.op("input.preview", json!({ "on": false })).unwrap();
    app.op("input.select", json!({ "source": "life_layer" })).unwrap();
    app.steps(8);
    save(&app.handle, &*app.store, &App::person()).unwrap();
    save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
    let view = app.view();
    app.quit();
    view
}

#[test]
fn every_v0_setting_comes_back_exactly_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let saved = first_run(&path);
    assert_eq!(saved.retarget.smoothing.to_bits(), 0.7_f32.to_bits());
    assert!(!saved.retarget.mirror);
    assert_ne!(saved.calibration, Calibration::identity(DeviceProfile::Webcam), "the capture changed the calibration");

    // A new run on the same folders: nothing is set again, only restored.
    let mut app = App::start(&path, &["a", "b"]);
    let fresh = app.view();
    assert_ne!(fresh, saved, "before the restore the app is at its defaults");
    let restored = app.restore(CHARACTER);
    assert_eq!(restored.findings, vec![], "nothing in the files was refused");
    assert!(restored.calibration && restored.missing_camera.is_none() && restored.problems.is_empty(), "{restored:?}");
    assert_eq!(restored.triggers, 2);
    assert_eq!(app.view(), saved, "source, camera, calibration, smoothing and hotkeys are exactly as saved");
}

#[test]
fn a_bad_stored_value_names_its_key_and_the_default_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    first_run(&path);
    let file = path.join("config").join("settings.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    // The saved number is an f32 widened to f64 (0.699999988...): replace whatever it is.
    let at = text.find("smoothing = ").unwrap_or_else(|| panic!("the saved file is:\n{text}")) + "smoothing = ".len();
    let end = at + text[at..].find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap();
    std::fs::write(&file, format!("{}4.0{}", &text[..at], &text[end..])).unwrap();

    let mut app = App::start(&path, &["a", "b"]);
    let restored = app.restore(CHARACTER);
    let bad: Vec<&str> = restored.findings.iter().map(|f| f.error.key.as_str()).collect();
    assert_eq!(bad, ["perform.retarget"], "{restored:?}");
    assert!(restored.findings[0].error.to_string().contains("smoothing"), "{}", restored.findings[0].error);
    assert_eq!(app.view().retarget.smoothing.to_bits(), RetargetPrefs::default().smoothing.to_bits());
    assert!(app.view().retarget.mirror, "the whole key fell back, so mirror is its default too");
    assert_eq!(restored.triggers, 2, "the other keys still came back");
}

#[test]
fn garbage_in_a_file_never_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    first_run(&path);
    for name in ["config/settings.toml", "data/machine.toml", "data/perform/calibration.toml"] {
        std::fs::write(path.join(name), "this is [not toml").unwrap();
    }
    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    assert!(!restored.calibration && restored.triggers == 0, "{restored:?}");
    assert_eq!(app.view().retarget.smoothing.to_bits(), RetargetPrefs::default().smoothing.to_bits());
    // The person is told why nothing came back: a file that does not parse names no key, so it is found by its file.
    let mut files: Vec<&str> = restored.findings.iter().map(|f| f.from.as_str()).collect();
    files.sort_unstable();
    assert_eq!(files, ["machine.toml", "settings.toml"], "{:?}", restored.findings);
    assert!(restored.findings.iter().all(|f| f.error.key.is_empty() && f.error.to_string().contains("does not parse")));
    assert_eq!(restored.calibration_findings.len(), 1, "{:?}", restored.calibration_findings);
    assert!(app.calibrations.locked().is_some());
}

#[test]
fn a_calibration_for_another_character_is_not_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let saved = first_run(&path);
    let mut app = App::start(&path, &["a", "b"]);
    let default = app.handle.calibration();
    let restored = app.restore("mira");
    assert!(!restored.calibration);
    assert_ne!(app.handle.calibration(), saved.calibration);
    assert_eq!(app.handle.calibration(), default, "the device's default is still in use");
    // And a character saved beside it keeps its own.
    save_calibration(&app.handle, &mut app.calibrations, "mira").unwrap();
    assert_eq!(app.calibrations.len(), 2);
    assert_eq!(app.calibrations.for_character(CHARACTER, DeviceProfile::Webcam), Some(saved.calibration));
}

#[test]
fn an_unplugged_camera_is_reported_and_its_binding_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    first_run(&path);
    let mut app = App::start(&path, &["a"]); // `b` is not connected today
    let restored = app.restore(CHARACTER);
    assert_eq!(restored.missing_camera, Some(CameraId("b".into())));
    assert_eq!(app.store.get::<InputCamera>().unwrap().id, Some(CameraId("b".into())), "the binding is still saved");
    assert_eq!(restored.triggers, 2, "the rest came back");
}

#[test]
fn each_v0_key_declares_who_may_write_it_and_where_it_lives() {
    let table = [
        (InputSource::META, SettingScope::Machine, AgentWrite::Confirm, InProfile::Excluded),
        (InputCamera::META, SettingScope::Machine, AgentWrite::Never, InProfile::Excluded),
        (PerformRetarget::META, SettingScope::User, AgentWrite::Free, InProfile::Member),
        (PerformTriggers::META, SettingScope::User, AgentWrite::Free, InProfile::Member),
    ];
    for (meta, scope, agent, profile) in table {
        assert_eq!(
            (meta.scope, meta.agent, meta.profile, meta.apply),
            (scope, agent, profile, Apply::Live),
            "{}",
            meta.key
        );
        assert!(registry::find(meta.key).is_some(), "{} is registered", meta.key);
    }
    // ADR 0026: a calibration is state with its own file, not a setting.
    assert!(registry::find("perform.calibrations").is_none());
}

#[test]
fn the_hotkey_list_is_bounded_and_the_keys_distinct() {
    let binding = |key: &str| omuzaki_stage::perform::triggers::TriggerBinding {
        key: key.to_owned(),
        expression: omuzaki_face_api::ExpressionId::new("happy").unwrap(),
        mode: omuzaki_anim::TriggerMode::Toggle,
    };
    let too_many: Vec<_> = (0..=MAX_TRIGGERS).map(|i| binding(&format!("F{i}"))).collect();
    assert!(PerformTriggers::check(&too_many).is_err());
    assert!(PerformTriggers::check(&vec![binding("F1"), binding("f1")]).is_err(), "one key, twice");
    assert!(PerformTriggers::check(&vec![binding("F1"), binding("F2")]).is_ok());
}

#[test]
fn unbinding_the_camera_is_saved_and_comes_back_unbound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let app = App::start(&path, &["a"]);
        app.op("input.select", json!({ "source": "webcam" })).unwrap();
        app.op("input.bind_camera", json!({ "camera": "a" })).unwrap();
        app.op("input.bind_camera", json!({ "camera": null })).unwrap();
        app.quit();
    }
    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    assert_eq!(restored.findings, vec![], "an unbound camera is a value the files can hold");
    assert_eq!((app.view().source, app.view().camera), (SourceKind::Webcam, None));
    assert_eq!(restored.missing_camera, None);
}

#[test]
fn the_output_window_settings_come_back_exactly_and_a_bad_one_names_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let saved =
        json!({ "backdrop": "key", "key": "#ff00ff", "scale": 0.5, "title": "My Output", "always_on_top": true });
    {
        // What `output.window.open` saves when a settings service exists.
        let app = App::start(&path, &["a"]);
        app.store.set_raw("output.window", saved.clone(), Layer::Base, Mode::Apply, &App::person()).unwrap();
        app.quit();
    }
    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    assert_eq!(restored.findings, vec![]);
    assert_eq!(restored.output_window, Some(saved), "the open window's choices are exactly as saved");
    drop(app);

    // A scale outside the allowed range: the key reads as its default, a finding names it, the start goes on.
    let file = path.join("data").join("machine.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    let at = text.find("scale = ").unwrap_or_else(|| panic!("the saved file is:\n{text}")) + "scale = ".len();
    let end = at + text[at..].find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap();
    std::fs::write(&file, format!("{}9.0{}", &text[..at], &text[end..])).unwrap();
    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    let named: Vec<&str> = restored.findings.iter().map(|f| f.error.key.as_str()).collect();
    assert_eq!(named, ["output.window"], "{restored:?}");
    assert_eq!(restored.output_window, None, "nothing usable to open the window with");
}

#[test]
fn a_full_set_of_hotkeys_the_dials_and_the_calibration_all_restore_before_the_first_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let names = ["happy", "angry", "surprised", "sad"];
    let saved = {
        let mut app = App::start(&path, &["a"]);
        for i in 0..MAX_TRIGGERS {
            let mode = if i % 2 == 0 { "toggle" } else { "hold" };
            app.op("trigger.bind", json!({ "key": format!("F{}", i + 1), "expression": names[i % 4], "mode": mode }))
                .unwrap();
            app.steps(1);
        }
        app.op("perform.set_retarget", json!({ "smoothing": 0.65, "head_gain": 1.5 })).unwrap();
        app.steps(2);
        app.handle.set_calibration(calibration_of(0.3)).unwrap();
        app.steps(2);
        save(&app.handle, &*app.store, &App::person()).unwrap();
        save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
        let view = app.view();
        app.quit();
        view
    };
    assert_eq!(saved.triggers.len(), MAX_TRIGGERS);

    // A new run restores with no performer step in between: 16 hotkeys, the dials and the calibration, one queue.
    let mut app = App::start(&path, &["a"]);
    let restored = restore(&app.handle, &*app.input, &*app.store, &app.calibrations, CHARACTER, DeviceProfile::Webcam);
    assert!(restored.problems.is_empty(), "{:?}", restored.problems);
    assert_eq!(restored.triggers, MAX_TRIGGERS);
    assert!(restored.calibration);
    app.steps(8);
    assert_eq!(app.view(), saved, "everything was applied, none of it dropped by a full queue");
}

#[test]
fn saving_right_after_an_op_saves_what_the_op_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let app = App::start(&path, &["a"]);
    // The op only queues its change; no performer step has run when `save` is called.
    app.op("perform.set_retarget", json!({ "smoothing": 0.9, "mirror": false })).unwrap();
    assert_ne!(app.handle.status().retarget.smoothing.to_bits(), 0.9_f32.to_bits(), "not applied yet");
    save(&app.handle, &*app.store, &App::person()).unwrap();
    let saved = app.store.get::<PerformRetarget>().unwrap();
    assert_eq!(saved.smoothing.to_bits(), 0.9_f32.to_bits());
    assert!(!saved.mirror);
}

#[test]
fn one_corrupt_calibration_neither_hides_the_others_nor_is_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let mut app = App::start(&path, &["a"]);
        app.handle.set_calibration(calibration_of(0.3)).unwrap();
        app.steps(2);
        save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
        app.quit();
    }
    // Beside the good entry: one character whose whole value is the wrong type, and one with garbage inside (a hand
    // edit, or a newer build's shape).
    let file = path.join("data").join("perform").join("calibration.toml");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n[[calibration]]\ncharacter = \"mira\"\nprofile = \"webcam\"\ncalibration = \"garbage\"\n");
    text.push_str("\n[[calibration]]\ncharacter = \"ola\"\nprofile = \"webcam\"\n[calibration.calibration]\nprofile = \"webcam\"\nranges = \"garbage\"\n");
    std::fs::write(&file, text).unwrap();

    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    assert!(restored.calibration, "the good calibration still comes back: {restored:?}");
    let named: Vec<_> = restored.calibration_findings.iter().map(|f| f.character.as_deref()).collect();
    assert_eq!(named, [Some("mira"), Some("ola")]);
    // Saving a third character's calibration keeps both: the good one and the corrupt ones as they were.
    save_calibration(&app.handle, &mut app.calibrations, "noor").unwrap();
    app.quit();
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written.matches("garbage").count(), 2, "the unreadable entries were not rewritten:\n{written}");
    let again = App::start(&path, &["a"]);
    assert!(again.calibrations.for_character(CHARACTER, DeviceProfile::Webcam).is_some());
    assert!(again.calibrations.for_character("noor", DeviceProfile::Webcam).is_some());
    assert_eq!(again.calibrations.findings().len(), 2);
}

#[test]
fn a_calibration_file_that_cannot_be_read_is_reported_and_never_saved_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let file = path.join("data").join("perform").join("calibration.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "version = 7\n").unwrap();
    let mut app = App::start(&path, &["a"]);
    let restored = app.restore(CHARACTER);
    assert!(!restored.calibration);
    assert_eq!(restored.calibration_findings.len(), 1, "{restored:?}");
    let refused = save_calibration(&app.handle, &mut app.calibrations, CHARACTER);
    assert!(matches!(refused, Err(CalibrationError::Locked { .. })), "{refused:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "version = 7\n", "the file is as it was");
}

#[test]
fn calibrations_live_in_their_own_file_and_not_in_the_settings_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    first_run(&path);
    let own = std::fs::read_to_string(path.join("data").join("perform").join("calibration.toml")).unwrap();
    assert!(own.contains("character = \"kiri\"") && own.contains("version = 1"), "{own}");
    for name in ["config/settings.toml", "data/machine.toml"] {
        let text = std::fs::read_to_string(path.join(name)).unwrap_or_default();
        assert!(!text.contains("kiri") && !text.contains("calibration"), "{name} holds no calibration:\n{text}");
    }
}

#[test]
fn saving_a_calibration_right_after_setting_it_saves_what_was_set() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let mut app = App::start(&path, &["a"]);
        // `set_calibration` only queues; no performer step has run when the calibration is saved.
        app.handle.set_calibration(calibration_of(0.4)).unwrap();
        assert_ne!(app.handle.calibration(), calibration_of(0.4), "not applied yet");
        assert_eq!(app.handle.requested_calibration(), calibration_of(0.4), "but it is what was asked for");
        save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
        app.steps(2);
        assert_eq!(app.handle.calibration(), calibration_of(0.4), "applied now");
        assert_eq!(app.handle.requested_calibration(), app.handle.calibration(), "and the two views agree again");
        app.quit();
    }
    let app = App::start(&path, &["a"]);
    assert_eq!(app.calibrations.for_character(CHARACTER, DeviceProfile::Webcam), Some(calibration_of(0.4)));
}

#[test]
fn a_restored_calibration_saved_before_the_first_step_is_not_replaced_by_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let mut app = App::start(&path, &["a"]);
        app.handle.set_calibration(calibration_of(0.3)).unwrap();
        app.steps(2);
        save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
        app.quit();
    }
    // The app restores and saves again at once (an exit right after start): the queued restore is what is saved.
    let mut app = App::start(&path, &["a"]);
    let restored = restore(&app.handle, &*app.input, &*app.store, &app.calibrations, CHARACTER, DeviceProfile::Webcam);
    assert!(restored.calibration);
    save_calibration(&app.handle, &mut app.calibrations, CHARACTER).unwrap();
    assert_eq!(app.calibrations.for_character(CHARACTER, DeviceProfile::Webcam), Some(calibration_of(0.3)));
}

/// A calibration every range of which depends on `seed`.
fn calibration_of(seed: f32) -> Calibration {
    use omuzaki_anim_api::{Channel, ChannelRange};
    let mut calibration = Calibration::identity(DeviceProfile::Webcam);
    for (i, info) in Channel::arkit().iter().enumerate() {
        let lo = seed * 0.1 + i as f32 * 0.001;
        calibration.set_range(info.channel, ChannelRange { lo, hi: lo + 0.5 + seed });
    }
    calibration
}
