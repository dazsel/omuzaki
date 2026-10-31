//! Registry-wide op conformance (QG-11): every op the `omuzaki` binary registers has a valid schema and honest
//! metadata, and each example it declares decodes, dry-runs without touching anything, and, for `Undo::Auto`, applies
//! and undoes back to the exact prior document and files.
//!
//! Examples are declared on the args type (`OpRegistration::examples`). Ops with none are listed in
//! `op_conformance_allowlist.txt`, which may only shrink: an op that is missing from it and has no examples fails, and
//! so does a line for an op that now has examples. The suite prints the list's size.
//!
//! An edit or job with `Undo::None` is a live action (`automation.md` D7): it changes live state, never the document.
//! The executor refuses one whose plan changes the document (`checks::check_plan`), so its examples' dry-runs fail if
//! it does; it is applied by nothing here, since a live action has no inverse to check.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the test reads the project folder to prove an op left it alone")]

mod common;
mod window_host;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use omuzaki_anim_api::{Calibration, DeviceProfile, RetargetConfig};
use omuzaki_cli::project::open_in_memory;
use omuzaki_core::time::SystemClock;
use omuzaki_doc::Project;
use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value};
use omuzaki_face_api::ExpressionSet;
use omuzaki_history_api::Snapshots;
use omuzaki_history_mock::MemorySnapshots;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Grants, Mode, OpKind, OpRegistration, ProjectDir, Request, Runner, ServicesBuilder, Undo, registrations,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::guided::GuidedStatus;
use omuzaki_stage::perform::host::{
    CalibrationState, PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt,
};
use omuzaki_stage::perform::input::{
    CameraEntry, InputError, InputHost, InputState, Permission, PreviewState, SourceKind,
};
use omuzaki_track_api::camera::CameraId;

const ALLOWLIST: &str = include_str!("op_conformance_allowlist.txt");

/// A first-party item for the `library.*` examples.
fn library_item(text: &str) -> omuzaki_library_api::ItemManifest {
    let head = "schema = 1\nversion = \"1.0.0\"\ntarget = \"character\"\norigin = \"op conformance\"\ntags = [\"conformance\"]\n";
    let rights = "[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n";
    let text = format!("{head}{text}\n{rights}");
    omuzaki_library_api::ItemManifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

/// The voices the `voice.*` examples and the `voiced` fixture use: Kokoro's stock voices locally, and `Rachel` from the
/// hosted provider `tts-cloud` at a price; and voice commands with the shipped phrase list.
fn speech_services(builder: ServicesBuilder) -> ServicesBuilder {
    use omuzaki_performer_api::ids::VoiceId;
    use omuzaki_performer_api::speech::VoiceError;
    use omuzaki_performer_api::voice::{TtsPrice, VoiceCatalog, VoiceInfo, VoiceProvider};

    struct RigVoices;
    impl VoiceCatalog for RigVoices {
        fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
            let voice = |id: &str, lang: &str| {
                VoiceInfo::new(
                    VoiceId::new(id).expect("a voice id"),
                    vec![omuzaki_l10n_api::Locale::new(lang).expect("a locale")],
                )
            };
            Ok(match provider {
                VoiceProvider::Local => vec![
                    voice("kokoro-82m/af_heart", "en-US"),
                    voice("kokoro-82m/am_michael", "en-US"),
                    voice("kokoro-82m/bf_emma", "en-GB"),
                ],
                _ => vec![voice("tts-cloud/Rachel", "en-US")],
            })
        }
        fn price(&self, provider: &VoiceProvider) -> Option<TtsPrice> {
            matches!(provider, VoiceProvider::Hosted(_)).then(|| TtsPrice::new(15_000_000, "2026-10-01"))
        }
    }
    builder.insert::<dyn VoiceCatalog>(Arc::new(RigVoices)).insert::<omuzaki_speech::commands::VoiceCommands>(Arc::new(
        omuzaki_speech::commands::VoiceCommands::new(omuzaki_speech::commands::default_phrases()),
    ))
}

/// The library the `library.*` examples and fixtures use, with the person's own items in `folder`: the colourway
/// `night-iris` and the hairstyle `twintails` (first-party), and the person's `anon/hair/ribbon-bow`.
fn library_services(builder: ServicesBuilder, folder: &Path) -> ServicesBuilder {
    use omuzaki_library::HostLibrary;
    use omuzaki_library_api::{ItemScope, Library};

    let palette = |slot: &str, colour: &str| {
        format!(
            "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"{slot}\"\ncolour = \"{colour}\"\n"
        )
    };
    let night_iris = library_item(&format!(
        "id = \"omuzaki/colourways/night-iris\"\nsection = \"colourways\"\nkind = \"setting\"\nslot = \"colourway\"\n\
         title = \"Night iris\"\nsummary = \"Deep violet and ink blue.\"\n\
         params.hair = {{ type = \"colour\", default = \"#2a2346\", summary = \"The hair root\" }}\n{}{}{}",
        palette("hair_root", "$params.hair"),
        palette("hair_mid", "#463a78"),
        palette("outfit_a", "#1d2a4a"),
    ));
    let twintails = library_item(
        "id = \"omuzaki/hair/twintails\"\nsection = \"hair\"\nkind = \"part\"\nslot = \"hairstyle\"\n\
         title = \"Twintails\"\nsummary = \"Two high tails.\"\ncreates.hair = { kind = \"hair\", id = \"hair\", layer = 5 }\n\
         [[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"hair\"\nid = \"$parts.hair\"\nlayer = 5\n",
    );
    let bow = omuzaki_library_api::ItemManifest::parse(
        "schema = 1\nid = \"anon/hair/ribbon-bow\"\nversion = \"1.0.0\"\nsection = \"hair\"\nkind = \"part\"\n\
         target = \"character\"\ntitle = \"Ribbon bow\"\nsummary = \"A ribbon tied in a bow.\"\n\
         creates.bow = { kind = \"hairpin\", id = \"bow\", layer = 40 }\n[rights]\nauthor = \"Anon\"\nlicence = \"CC-BY-4.0\"\n\
         commercial = \"ok\"\n\n[[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"hairpin\"\n\
         id = \"$parts.bow\"\n",
    )
    .expect("the ribbon bow parses");
    let library = Arc::new(HostLibrary::open(Some(folder)));
    let refused = library.insert_all([
        (night_iris, ItemScope::FirstParty),
        (twintails, ItemScope::FirstParty),
        (bow, ItemScope::Mine),
    ]);
    assert!(refused.is_empty(), "the conformance items load: {refused:?}");
    builder.insert::<dyn Library>(Arc::clone(&library) as Arc<dyn Library>).insert(library)
}

/// The default fixture project: an empty project named `demo`.
const DEFAULT_FIXTURE: &str = "demo";

/// The fixture projects other than `demo`: one JSON file each, `<name>.json`, so a new fixture is a new file and this
/// test doesn't change. A file holds `about` (what the fixture is for), an optional `base` (the fixture it builds on)
/// and `steps`, each an `op` and its `args`. A fixture is made by running those real ops (never by writing the document
/// by hand), so what an example sees is what a person's own steps would leave.
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/op_fixtures");

/// The deepest chain of `base` fixtures followed before the chain is taken for a loop.
const MAX_FIXTURE_DEPTH: usize = 8;

/// One fixture file.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFile {
    /// What the fixture is for: the state it leaves and which ops need it.
    about: String,
    /// The fixture whose steps run first.
    #[serde(default)]
    base: Option<String>,
    /// The ops that make it, in order.
    steps: Vec<FixtureStep>,
}

/// One op run while building a fixture.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureStep {
    op: String,
    args: Value,
}

/// The names of the fixture files, sorted: what an op's `x-omuzaki-fixture` may name besides `demo`.
fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(FIXTURE_DIR)
        .map(|entries| {
            entries
                .filter_map(|entry| {
                    entry.ok()?.file_name().into_string().ok()?.strip_suffix(".json").map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The `(op, args)` steps that build the fixture `name` on top of an empty `demo` project, its bases' steps first.
fn fixture_steps(name: &str) -> Result<Vec<FixtureStep>, String> {
    let mut chain = Vec::new();
    let mut next = Some(name.to_owned());
    while let Some(name) = next {
        if chain.len() == MAX_FIXTURE_DEPTH {
            return Err(format!(
                "fixture `{name}`: its `base` chain is longer than {MAX_FIXTURE_DEPTH}; is it a loop?"
            ));
        }
        let path = Path::new(FIXTURE_DIR).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).map_err(|_| {
            let known = fixture_names().join("`, `");
            format!("names fixture `{name}`, and the conformance test knows only `{DEFAULT_FIXTURE}`, `{known}`")
        })?;
        let file: FixtureFile =
            serde_json::from_str(&text).map_err(|e| format!("fixture file `{name}.json` doesn't read: {e}"))?;
        if file.about.trim().is_empty() {
            return Err(format!("fixture file `{name}.json` says nothing in `about`"));
        }
        next = file.base;
        chain.push(file.steps);
    }
    Ok(chain.into_iter().rev().flatten().collect())
}

/// Builds the fixture `name` in a fresh project folder: the steps run through an executor that has every op registered
/// (the rig itself registers only the op under test), then the folder is closed so the rig reopens it from disk, as a
/// person's next session would.
fn fixture_project(name: &str) -> Result<tempfile::TempDir, String> {
    let dir = common::new_project();
    if name == DEFAULT_FIXTURE {
        return Ok(dir);
    }
    let steps = fixture_steps(name)?;
    let doc = Saved::open(dir.path()).map_err(|e| format!("opening the project for fixture `{name}`: {e}"))?;
    let project = ProjectDir::open(dir.path()).map_err(|e| format!("opening the folder for fixture `{name}`: {e}"))?;
    let library = tempfile::tempdir().map_err(|e| e.to_string())?;
    let setup = Executor::new(Box::new(doc), Arc::new(SystemClock::new()))
        .map_err(|e| e.to_string())?
        .with_services(speech_services(library_services(ServicesBuilder::new(), library.path())).build())
        .with_project_dir(project);
    for FixtureStep { op, args } in steps {
        let request = Request {
            op: op.clone(),
            args,
            mode: Mode::Apply,
            actor: Actor::Person { name: "conformance".to_owned() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        setup.run(request).map_err(|e| format!("fixture `{name}` step `{op}` failed: {}", e.message))?;
    }
    Ok(dir)
}

/// The project the rig runs ops on, written to its files after every patch, so the rules on files (a dry run writes
/// nothing, an undo leaves them byte for byte as they were) are checked the moment each run returns. The app writes
/// files from its keeper a moment later; for these rules only the files' content counts, not when they are written.
#[derive(Debug)]
struct Saved(Project);

impl Saved {
    fn open(dir: &Path) -> Result<Self, String> {
        open_in_memory(dir).map(Self).map_err(|e| e.to_string())
    }
}

impl DocRead for Saved {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        self.0.get(path)
    }

    fn version(&self) -> DocVersion {
        self.0.version()
    }
}

impl DocWrite for Saved {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        let version = self.0.apply(patch)?;
        self.0.save().map_err(|e| PatchError::Unreachable { path: DocPath::root(), reason: e.to_string() })?;
        Ok(version)
    }
}

/// What a run can leave behind: the document tree, every file under the project folder, and the op log's length.
#[derive(PartialEq, Debug)]
struct State {
    tree: Option<Value>,
    files: BTreeMap<String, Vec<u8>>,
    log: usize,
}

/// Stand-ins for the studio's `ui.scale` and `ui.theme` (declared in `omuzaki-ui`, which the CLI doesn't link), the
/// keys the `settings.*` examples name, declared as the studio does: taste, free for an agent, applied at once.
mod ui_settings {
    use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
    use omuzaki_settings_api::register_setting;
    use omuzaki_settings_api::setting::Setting;

    const fn taste(key: &'static str) -> SettingMeta {
        SettingMeta::new(key, "a stand-in", SettingScope::User, AgentWrite::Free, InProfile::Member, Apply::Live, 1)
    }

    pub struct UiScale;

    impl Setting for UiScale {
        type Value = u16;
        const META: SettingMeta = taste("ui.scale");

        fn default() -> u16 {
            100
        }
    }

    register_setting!(UiScale);

    pub struct UiTheme;

    impl Setting for UiTheme {
        type Value = String;
        const META: SettingMeta = taste("ui.theme");

        fn default() -> String {
            "dark".to_owned()
        }
    }

    register_setting!(UiTheme);
}

/// The performer an op's example runs against: none. It lists no triggers and refuses every request, which is all a
/// dry-run of the `trigger.*` examples reaches (`trigger.list` is the one that reads it). Its character has the starter
/// expressions, so a `trigger.bind` dry run finds the ones its examples name.
struct IdleHost;

/// The tracking input an op's example runs against: one camera, `fake:0`, a webcam source, the mirror off. It starts
/// nothing: every change answers as the dry run of a real input would and keeps no state.
struct IdleInput;

impl IdleInput {
    const CAMERA: &'static str = "fake:0";

    fn camera() -> CameraId {
        CameraId(Self::CAMERA.to_owned())
    }
}

impl InputHost for IdleInput {
    fn select(&self, _source: SourceKind, _mode: Mode) -> Result<SourceKind, InputError> {
        Ok(SourceKind::Webcam)
    }

    fn bind_camera(&self, camera: Option<CameraId>, _mode: Mode) -> Result<Option<CameraId>, InputError> {
        match camera {
            Some(camera) if camera != Self::camera() => Err(InputError::no_camera(camera)),
            _ => Ok(None),
        }
    }

    fn preview(&self, _on: bool, _mode: Mode) -> Result<bool, InputError> {
        Ok(false)
    }

    fn request_camera(&self) -> Permission {
        Permission::Unknown
    }

    fn state(&self) -> InputState {
        InputState {
            source: SourceKind::Webcam,
            camera: None,
            camera_present: false,
            preview: PreviewState::Off,
            permission: Permission::Unknown,
            cameras: vec![CameraEntry { id: Self::camera(), name: "Fake camera".to_owned(), bound: false }],
            tracking: Tracking::NoFrames,
        }
    }

    // A guided calibration that "starts" without running: the `input.calibrate` examples dry-run against it.
    fn calibrate_guided(&self, _mode: Mode) -> Result<GuidedStatus, InputError> {
        Ok(GuidedStatus::Idle)
    }

    // A pose output that "starts" without sending: the `input.vmc_out` examples dry-run against it.
    fn set_pose_out(
        &self,
        _target: Option<std::net::SocketAddr>,
        _mode: Mode,
    ) -> Result<omuzaki_track_api::pose_out::PoseOutStatus, InputError> {
        Ok(omuzaki_track_api::pose_out::PoseOutStatus::default())
    }

    // Body tracking that "starts" without tracking: the `input.track_body` examples dry-run against it.
    fn track_body(
        &self,
        _config: omuzaki_track_api::body::BodyConfig,
        _mode: Mode,
    ) -> Result<omuzaki_track_api::body::BodyConfig, InputError> {
        Ok(omuzaki_track_api::body::BodyConfig::OFF)
    }

    // A video reader that adds nothing: the `input.open_video` example dry-runs against it, whatever its path.
    fn add_video(
        &self,
        _path: &std::path::Path,
        _mode: Mode,
    ) -> Result<Option<omuzaki_track_api::camera::CameraInfo>, InputError> {
        Ok(None)
    }
}

impl PerformHost for IdleHost {
    fn calibrate(&self, _frames: usize) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_retarget(&self, _change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_calibration(&self, _calibration: Calibration) -> Result<(), PerformError> {
        Err(PerformError::Unsupported)
    }

    fn calibration(&self) -> Calibration {
        Calibration::identity(DeviceProfile::Arkit)
    }

    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::starter()
    }

    fn status(&self) -> PerformStatus {
        PerformStatus {
            tracking: Tracking::NoFrames,
            calibration: CalibrationState::Idle,
            retarget: RetargetConfig::default(),
            retarget_seq: 0,
            source_fps: 0.0,
        }
    }
}

/// The branches the `branch.*` examples read: `main` at `now`, and `aria/oval` forked from it with the project's
/// note set.
fn branches_at(now: &Value) -> Arc<omuzaki_history_mock::branch::MemoryBranchStore> {
    use omuzaki_history_api::branch::{
        BranchId, BranchInfo, BranchName, Contains, LaneState, Point, PointKind, Version,
    };
    use omuzaki_history_api::{Lsn, SnapshotId, WallTime};
    let store = Arc::new(omuzaki_history_mock::branch::MemoryBranchStore::new());
    let snapshot = SnapshotId(omuzaki_store_api::ContentHash::of(b"conformance"));
    let main_point =
        Point::new(BranchId::main(), 1, snapshot, Lsn(1), PointKind::Checkpoint, Actor::System, WallTime(1));
    let main_head = store.put_point(main_point, now.clone()).expect("a point encodes");
    let mut main = BranchInfo::new(BranchId::main(), BranchName::main(), main_head, 1, WallTime(1), Actor::System);
    main.lane = LaneState::open(true, 0);
    store.put_branch(main);
    let oval_id = BranchId::new("9c41d07a5e2b1f30").expect("a branch id");
    let mut theirs = now.clone();
    if let Some(project) = theirs.get_mut("project").and_then(Value::as_object_mut) {
        project.insert("note".to_owned(), Value::from("from aria/oval"));
    }
    let from_main = Contains::new().with(BranchId::main(), Version::new(1, main_head));
    let oval_point =
        Point::new(oval_id.clone(), 1, snapshot, Lsn(2), PointKind::Checkpoint, Actor::System, WallTime(2))
            .with_parents(vec![main_head])
            .with_contains(from_main.clone());
    let oval_head = store.put_point(oval_point, theirs).expect("a point encodes");
    let name = BranchName::new("aria/oval").expect("a branch name");
    let mut oval = BranchInfo::new(oval_id, name, oval_head, 1, WallTime(2), Actor::System);
    oval.inherits = from_main;
    store.put_branch(oval);
    store
}

struct Rig {
    runner: Executor,
    dir: tempfile::TempDir,
    /// The person's library folder for the `library.*` examples.
    _library: tempfile::TempDir,
    actor: Actor,
    grants: Grants,
}

impl Rig {
    fn new(reg: &'static OpRegistration, fixture: Option<&str>) -> Result<Self, String> {
        let dir = fixture_project(fixture.unwrap_or(DEFAULT_FIXTURE))?;
        let doc = Saved::open(dir.path()).map_err(|e| format!("opening the fixture failed: {e}"))?;
        // History with transactions 7 and 9 at the fixture's own state, so the `history.*` examples resolve.
        let now = doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null);
        let branches = branches_at(&now);
        let view = omuzaki_history_mock::branch::MemoryHistoryView::new(Arc::clone(&branches), now.clone());
        let log = omuzaki_history_mock::MemoryHistoryLog::default();
        for tx in [7, 9, 41] {
            view.record_tx(omuzaki_ops_api::TxId(tx), now.clone());
            log.push(omuzaki_history_api::TxSummary::new(
                omuzaki_ops_api::TxId(tx),
                "conformance",
                "test",
                Actor::System,
                omuzaki_history_api::Lsn(tx),
            ));
        }
        let project = ProjectDir::open(dir.path()).map_err(|e| format!("opening the fixture folder failed: {e}"))?;
        let library = tempfile::tempdir().map_err(|e| e.to_string())?;
        let runner = Executor::with_registrations(Box::new(doc), Arc::new(SystemClock::new()), [reg])
            .map_err(|e| e.to_string())?
            .with_services(
                speech_services(library_services(
                    window_host::with_window(ServicesBuilder::new(), (reg.meta)().name),
                    library.path(),
                ))
                // The stream service with nothing running: the `record.*` and `output.*` examples dry-run
                // against it as they would in the app.
                .insert::<omuzaki_stream::service::StreamService>(Arc::new(
                    omuzaki_stream::service::StreamService::new().expect("the stream service starts"),
                ))
                // The mock audio engine: the `audio.*` examples bind its default microphone.
                .insert::<dyn omuzaki_audio_api::engine::AudioEngine>(Arc::new(omuzaki_audio_mock::MockEngine::new()))
                // No OBS connection: the `obs.*` examples dry-run and say they would need one.
                .insert::<omuzaki_obs::service::ObsService>(Arc::new(omuzaki_obs::service::ObsService::new()))
                // No VTube Studio bridge: the `vts.*` examples dry-run and say they would need a connection.
                .insert::<omuzaki_vts::service::VtsService>(Arc::new(
                    omuzaki_vts::service::VtsService::new().expect("the built-in channel map parses"),
                ))
                .insert::<omuzaki_live::Automation>(Arc::new(omuzaki_live::Automation::new()))
                // A Twitch service on a transport that answers nothing: the `twitch.*` examples dry-run and
                // report that no token is stored.
                .insert::<omuzaki_twitch::TwitchService>(Arc::new(omuzaki_twitch::TwitchService::new(
                    Arc::new(omuzaki_http_mock::FakeHttp::new()),
                    Arc::new(omuzaki_live::Automation::new()),
                )))
                // Settings in memory: the `settings.*` examples read and write them as they would in the app,
                // with the profile `stream-0001` the `settings.profile.*` examples name.
                .insert::<dyn omuzaki_settings_api::service::Settings>(settings_with_a_profile())
                .insert::<dyn PerformHost>(Arc::new(IdleHost))
                .insert::<dyn InputHost>(Arc::new(IdleInput))
                // The stage's effect queue with no stage draining it: the `stage.effect.*` examples dry-run.
                .insert::<dyn omuzaki_stage_api::EffectService>(Arc::new(
                    omuzaki_stage::effects::service::StageEffects::new(0),
                ))
                // Snapshots with nothing to reload: `project.save` and `project.reload` run as they would on a
                // project whose files match the document.
                .insert::<dyn omuzaki_history_api::state::HistoryView>(Arc::new(view))
                // `main` (the folder branch, at the fixture's state) and `aria/oval`, forked from it with the note
                // changed: the `branch.*` examples merge, list and compare them.
                .insert::<dyn omuzaki_history_api::state::BranchStore>(branches)
                .insert::<dyn omuzaki_history_api::HistoryLog>(Arc::new(log))
                .insert::<dyn Snapshots>(Arc::new(MemorySnapshots::new(
                    Value::Null,
                    Arc::new(omuzaki_history_mock::SystemWallClock),
                )))
                .build(),
            )
            .with_project_dir(project);
        let actor = Actor::Person { name: "conformance".to_owned() };
        Ok(Self { runner, dir, _library: library, actor, grants: Grants::person() })
    }

    fn state(&self) -> State {
        State {
            tree: self.runner.get(&DocPath::root(), &self.grants).expect("the person may read"),
            files: files_under(self.dir.path()),
            log: self.runner.log(0, usize::MAX, &self.grants).expect("the person may read").len(),
        }
    }

    fn run(&self, op: &str, args: &Value, mode: Mode) -> Result<omuzaki_ops_api::Outcome, String> {
        let request = Request {
            op: op.to_owned(),
            args: args.clone(),
            mode,
            actor: self.actor.clone(),
            grants: self.grants.clone(),
            cause: None,
            tx: None,
            base: None,
        };
        self.runner.run(request).map_err(|e| e.message)
    }
}

fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("list the folder") {
            let path = entry.expect("read the entry").path();
            let name = path.strip_prefix(root).expect("under the root").to_string_lossy().into_owned();
            // The app's own bookkeeping (`.omuzaki/`, e.g. backups of replaced files) is not the user's files.
            if name == ".omuzaki" {
                continue;
            }
            if path.is_dir() {
                // A directory is recorded too (as a trailing-slash key with no bytes), so an empty one counts.
                into.insert(format!("{name}/"), Vec::new());
                walk(root, &path, into);
            } else {
                into.insert(name, std::fs::read(&path).expect("read the file"));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// Every way `reg` breaks the conformance rules; empty when it conforms.
fn problems(reg: &'static OpRegistration) -> Vec<String> {
    let meta = (reg.meta)();
    let name = meta.name;
    let mut found = Vec::new();
    let mut flag = |what: String| found.push(format!("{name}: {what}"));

    let schema = serde_json::to_value((reg.schema)()).expect("a schema serialises");
    if schema.get("type") != Some(&Value::from("object")) {
        flag("its schema is not an object schema".to_owned());
    }
    for (n, args) in reg.examples().iter().enumerate() {
        if let Err(why) = check_example(reg, meta.kind, meta.undo, args) {
            flag(format!("example {n} {why}"));
        }
    }
    found
}

fn check_example(reg: &'static OpRegistration, kind: OpKind, undo: Undo, args: &Value) -> Result<(), String> {
    (reg.decode)(args.clone()).map_err(|e| format!("doesn't decode: {}", e.message))?;
    let name = (reg.meta)().name;
    let rig = Rig::new(reg, reg.fixture().as_deref())?;

    let before = rig.state();
    let dry = rig.run(name, args, Mode::DryRun).map_err(|e| format!("fails its dry-run: {e}"))?;
    if dry.applied.is_some() || rig.state() != before {
        return Err("changed the document, its files or the log in a dry-run".to_owned());
    }
    // Jobs apply by running their work, and only `Auto` ops promise an exact inverse; the rest stop at the dry-run.
    if kind == OpKind::Job || undo != Undo::Auto {
        return Ok(());
    }
    rig.run(name, args, Mode::Apply).map_err(|e| format!("fails when applied: {e}"))?;
    if !dry.plan.patch.is_empty() {
        let label = rig.runner.undo(&rig.actor, &rig.grants).map_err(|e| format!("can't be undone: {}", e.message))?;
        if label.is_none() {
            return Err("left nothing to undo although its plan has changes".to_owned());
        }
    }
    let after = rig.state();
    if after.tree != before.tree || after.files != before.files {
        let moved: Vec<&String> = before
            .files
            .keys()
            .chain(after.files.keys())
            .filter(|k| before.files.get(*k) != after.files.get(*k))
            .collect();
        return Err(format!(
            "is not restored exactly by undo (tree changed: {}, files changed: {moved:?}{})",
            after.tree != before.tree,
            first_text_difference(&before.files, &after.files)
        ));
    }
    Ok(())
}

/// The first line that differs in the first text file that does, as `; line N was ... now ...`, or nothing when no text
/// file differs line-wise. It makes a failure say what moved, not only where.
fn first_text_difference(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> String {
    for (name, old) in before {
        let Some(new) = after.get(name).filter(|new| *new != old) else { continue };
        let (Ok(old), Ok(new)) = (std::str::from_utf8(old), std::str::from_utf8(new)) else { continue };
        let (mut old_lines, mut new_lines) = (old.lines(), new.lines());
        for n in 1.. {
            let (a, b) = (old_lines.next(), new_lines.next());
            if a != b {
                return format!("; {name} line {n} was {a:?}, now {b:?}");
            }
            if a.is_none() {
                break;
            }
        }
    }
    String::new()
}

/// The allowlist: ops without examples.
fn allowlist() -> BTreeSet<&'static str> {
    ALLOWLIST.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect()
}

/// Everything wrong with `registered` against `allowed`: ops with no examples that aren't listed, and listed names that
/// no longer need to be.
fn allowlist_problems(registered: &BTreeMap<&'static str, bool>, allowed: &BTreeSet<&str>) -> Vec<String> {
    let mut found = Vec::new();
    for (name, has_examples) in registered {
        match (has_examples, allowed.contains(name)) {
            (false, false) => found.push(format!("{name}: declares no examples and isn't allowlisted; add examples")),
            (true, true) => found.push(format!("{name}: has examples now; delete it from the allowlist")),
            _ => {}
        }
    }
    found.extend(
        allowed
            .iter()
            .filter(|name| !registered.contains_key(*name))
            .map(|name| format!("{name}: allowlisted but no op has that name")),
    );
    found
}

/// Every fixture file reads and builds, whether or not an op names it yet, so a broken fixture fails here by name
/// rather than as some op's example.
#[test]
fn every_fixture_file_builds() {
    omuzaki_cli::link();
    let names = fixture_names();
    assert!(!names.is_empty(), "no fixture files in {FIXTURE_DIR}");
    let broken: Vec<String> = names.iter().filter_map(|name| fixture_project(name).err()).collect();
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

#[test]
fn every_registered_op_conforms() {
    omuzaki_cli::link();
    let mut registered = BTreeMap::new();
    let mut found = Vec::new();
    for reg in registrations() {
        let meta = (reg.meta)();
        registered.insert(meta.name, !reg.examples().is_empty());
        found.extend(problems(reg));
    }
    let allowed = allowlist();
    found.extend(allowlist_problems(&registered, &allowed));
    println!(
        "op conformance: {} ops registered, {} with examples, {} on the allowlist",
        registered.len(),
        registered.values().filter(|&&has| has).count(),
        allowed.len()
    );
    // `registrations()` iterates in linker order; sort so the report is the same on every platform.
    assert!(found.is_empty(), "{}", sorted_report(found));
}

/// The failures one per line, in name order, so the report doesn't depend on the order the linker put registrations in.
fn sorted_report(mut found: Vec<String>) -> String {
    found.sort();
    found.join("\n")
}

/// Hostile arguments (QG-11, the agent surface): every example of every op, as it is and then with each value in it
/// deleted and replaced by each of a set of nasty values, dry-run on the example's fixture. An agent over MCP can send
/// any of these, and a plan runs on the caller's thread, where nothing catches a panic: the rule is that a dry-run
/// answers with a plan or an error, never a panic.
#[test]
fn every_op_answers_hostile_arguments_without_a_panic() {
    omuzaki_cli::link();
    let nasty = hostile::values();
    let (mut ops, mut tried) = (0_usize, 0_usize);
    let mut panics: Vec<String> = Vec::new();
    for reg in registrations() {
        let examples = reg.examples();
        if examples.is_empty() {
            continue;
        }
        let name = (reg.meta)().name;
        ops += 1;
        let mut rig = Rig::new(reg, reg.fixture().as_deref()).unwrap_or_else(|e| panic!("{name}: {e}"));
        for example in &examples {
            for args in hostile::variants(example, &nasty) {
                tried += 1;
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = rig.run(name, &args, Mode::DryRun);
                }));
                if let Err(panic) = ran {
                    let why = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    panics.push(format!("{name} {args}: {why}"));
                    // A panic may have left the executor half-way through a request: start the next on a fresh one.
                    rig = Rig::new(reg, reg.fixture().as_deref()).unwrap_or_else(|e| panic!("{name}: {e}"));
                }
            }
        }
    }
    println!("hostile arguments: {ops} ops, {tried} argument sets, {} panicked", panics.len());
    assert!(panics.is_empty(), "dry-runs panicked:\n{}", sorted_report(panics));
}

/// The argument sets [`every_op_answers_hostile_arguments_without_a_panic`] tries.
mod hostile {
    use serde_json::{Value, json};

    /// Values that break assumptions: signs, zero, huge and tiny numbers, wrong types, ids that exist in the fixtures,
    /// a path out of the project, a long string and control characters, and vectors of the wrong length or size.
    pub(super) fn values() -> Vec<Value> {
        vec![
            json!(-1),
            json!(0),
            json!(1),
            json!(-1e30),
            json!(1e30),
            json!(1e300),
            json!(-0.0),
            json!(0.5),
            json!(4_294_967_296_u64),
            json!(u64::MAX),
            json!(i64::MIN),
            json!(null),
            json!(true),
            json!(""),
            json!("main"),
            json!("kit"),
            json!("../outside"),
            json!("a".repeat(5000)),
            json!("\u{0}\u{202e}"),
            json!([]),
            json!([0.0, 0.0, 0.0]),
            json!([1e30, -1e30, 1e30]),
            json!({}),
        ]
    }

    /// `example` as it is, then with the value at each path in it (the whole value included) deleted and replaced by
    /// each of `nasty`.
    pub(super) fn variants(example: &Value, nasty: &[Value]) -> Vec<Value> {
        let mut paths = Vec::new();
        leaves(example, &mut Vec::new(), &mut paths);
        let mut out = vec![example.clone()];
        for path in &paths {
            out.extend(edited(example, path, None));
            out.extend(nasty.iter().filter_map(|v| edited(example, path, Some(v))));
        }
        out
    }

    /// The path of every value in `value`, as keys and indices, the empty path first.
    fn leaves(value: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        out.push(at.clone());
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    at.push(key.clone());
                    leaves(child, at, out);
                    at.pop();
                }
            }
            Value::Array(items) => {
                for (ix, child) in items.iter().enumerate() {
                    at.push(ix.to_string());
                    leaves(child, at, out);
                    at.pop();
                }
            }
            _ => {}
        }
    }

    fn at_path<'a>(value: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
        path.iter().try_fold(value, |v, key| match v {
            Value::Object(map) => map.get_mut(key),
            Value::Array(items) => items.get_mut(key.parse::<usize>().ok()?),
            _ => None,
        })
    }

    /// `example` with the value at `path` deleted (`None`) or replaced.
    fn edited(example: &Value, path: &[String], replacement: Option<&Value>) -> Option<Value> {
        let Some((last, parent)) = path.split_last() else {
            return replacement.cloned();
        };
        let mut out = example.clone();
        match (at_path(&mut out, parent)?, replacement) {
            (Value::Object(map), Some(v)) => {
                map.insert(last.clone(), v.clone());
            }
            (Value::Object(map), None) => {
                map.remove(last);
            }
            (Value::Array(items), Some(v)) => *items.get_mut(last.parse::<usize>().ok()?)? = v.clone(),
            (Value::Array(items), None) => {
                let ix = last.parse::<usize>().ok().filter(|&i| i < items.len())?;
                items.remove(ix);
            }
            _ => return None,
        }
        Some(out)
    }
}

/// Planted ops, never registered: each breaks exactly one rule, and the suite must say so.
mod planted {
    use omuzaki_doc_api::Patch;
    use omuzaki_ops_api::{FsMode, Op, OpCtx, OpError, OpMeta, Plan, Scope, WriteMode};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    fn note() -> DocPath {
        DocPath::root().join("project").join("note")
    }

    fn set_note(ctx: &OpCtx<'_>, text: &str) -> Plan {
        Plan::edit(Patch::new().with(note(), ctx.doc.get(&note()).cloned(), Some(json!(text))))
    }

    fn write_marker(ctx: &OpCtx<'_>) -> Result<(), OpError> {
        use std::io::Write;
        let mut file = ctx.fs_access("marker.txt", FsMode::Write)?.open_write(WriteMode::CreateOrTruncate)?;
        file.write_all(b"written").map_err(|e| OpError::internal(e.to_string()))
    }

    const fn meta(name: &'static str, undo: Undo) -> OpMeta {
        OpMeta { name, summary: "A planted op.", kind: OpKind::Edit, scope: Scope::EditScene, undo }
    }

    /// Conforms: sets the note.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct Honest {
        text: String,
    }

    impl Op for Honest {
        fn meta() -> OpMeta {
            meta("planted.honest", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Writes a file while planning a dry-run.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct WritesInDryRun {
        text: String,
    }

    impl Op for WritesInDryRun {
        fn meta() -> OpMeta {
            meta("planted.writes_in_dry_run", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            write_marker(ctx)?;
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Claims `Undo::Auto` but also writes a file when applied, which undoing the patch cannot take back.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct WrongInverse {
        text: String,
    }

    impl Op for WrongInverse {
        fn meta() -> OpMeta {
            meta("planted.wrong_inverse", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            if !ctx.dry_run {
                write_marker(ctx)?;
            }
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Creates an empty folder while planning a dry-run.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct MakesFolder {
        text: String,
    }

    impl Op for MakesFolder {
        fn meta() -> OpMeta {
            meta("planted.makes_folder", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            ctx.fs_access("scratch", FsMode::Write)?.create_dir_all()?;
            Ok(set_note(ctx, &self.text))
        }
    }

    /// Creates an empty folder only when applied, which undoing the patch cannot take back.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct MakesFolderWhenApplied {
        text: String,
    }

    impl Op for MakesFolderWhenApplied {
        fn meta() -> OpMeta {
            meta("planted.makes_folder_when_applied", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            if !ctx.dry_run {
                ctx.fs_access("scratch", FsMode::Write)?.create_dir_all()?;
            }
            Ok(set_note(ctx, &self.text))
        }
    }

    /// A live action: an edit with nothing to undo, which changes live state and never the document.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{}]))]
    pub struct LiveAction {}

    impl Op for LiveAction {
        fn meta() -> OpMeta {
            meta("planted.live_action", Undo::None)
        }

        fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(Plan::answer(json!({ "played": true })))
        }
    }

    /// Says there is nothing to undo, and changes the document.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": "hello" }]))]
    pub struct EditWithoutUndo {
        text: String,
    }

    impl Op for EditWithoutUndo {
        fn meta() -> OpMeta {
            meta("planted.edit_without_undo", Undo::None)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(set_note(ctx, &self.text))
        }
    }

    /// An example its own args type refuses.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "text": 7 }]))]
    pub struct BadExample {
        text: String,
    }

    impl Op for BadExample {
        fn meta() -> OpMeta {
            meta("planted.bad_example", Undo::Auto)
        }

        fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(set_note(ctx, &self.text))
        }
    }
}

macro_rules! planted_reg {
    ($ty:ty) => {{
        static REG: OpRegistration = OpRegistration::of::<$ty>();
        &REG
    }};
}

fn assert_flagged(reg: &'static OpRegistration, needle: &str) {
    let found = problems(reg);
    assert!(found.iter().any(|p| p.contains(needle)), "expected a problem containing {needle:?}, got {found:?}");
}

#[test]
fn an_honest_op_conforms() {
    assert_eq!(problems(planted_reg!(planted::Honest)), Vec::<String>::new());
}

#[test]
fn a_dry_run_that_writes_fails() {
    assert_flagged(planted_reg!(planted::WritesInDryRun), "changed the document, its files or the log in a dry-run");
}

#[test]
fn a_wrong_inverse_fails() {
    assert_flagged(planted_reg!(planted::WrongInverse), "is not restored exactly by undo");
}

#[test]
fn an_empty_folder_made_in_a_dry_run_fails() {
    assert_flagged(planted_reg!(planted::MakesFolder), "changed the document, its files or the log in a dry-run");
}

#[test]
fn an_empty_folder_left_after_undo_fails() {
    assert_flagged(planted_reg!(planted::MakesFolderWhenApplied), "is not restored exactly by undo");
}

#[test]
fn failures_are_reported_in_name_order() {
    let found = vec!["b.op: second".to_owned(), "c.op: third".to_owned(), "a.op: first".to_owned()];
    assert_eq!(sorted_report(found), "a.op: first\nb.op: second\nc.op: third");
}

#[test]
fn a_live_action_conforms_and_an_edit_without_undo_that_changes_the_document_fails() {
    assert_eq!(problems(planted_reg!(planted::LiveAction)), Vec::<String>::new());
    assert_flagged(planted_reg!(planted::EditWithoutUndo), "is a live action (`Undo::None`) but planned a change");
}

#[test]
fn an_example_that_does_not_decode_fails() {
    assert_flagged(planted_reg!(planted::BadExample), "doesn't decode");
}

#[test]
fn the_allowlist_may_only_shrink() {
    let registered = BTreeMap::from([("a.new", false), ("a.done", true), ("a.todo", false)]);
    let allowed = BTreeSet::from(["a.done", "a.todo", "a.gone"]);
    let found = allowlist_problems(&registered, &allowed);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().any(|p| p.starts_with("a.new:")));
    assert!(found.iter().any(|p| p.starts_with("a.done:")));
    assert!(found.iter().any(|p| p.starts_with("a.gone:")));
}

#[test]
fn the_snapshot_leaves_out_the_apps_own_folder_only() {
    let dir = tempfile::tempdir().expect("make a temp folder");
    std::fs::create_dir_all(dir.path().join(".omuzaki/replaced")).expect("make the bookkeeping folder");
    std::fs::write(dir.path().join(".omuzaki/replaced/a.kept"), b"old").expect("write a backup");
    std::fs::write(dir.path().join("marker.txt"), b"stray").expect("write a stray file");
    let files: Vec<String> = files_under(dir.path()).into_keys().collect();
    assert_eq!(files, vec!["marker.txt".to_string()]);
}

/// Settings in memory holding one profile, "Stream" (`stream-0001`, the mock's first id).
/// The settings mock with the profile "Stream" saved (the profile ops' examples name it) and first-run setup on its
/// Go-live check (where `setup.finish` is honoured).
fn settings_with_a_profile() -> Arc<dyn omuzaki_settings_api::service::Settings> {
    use omuzaki_settings_api::service::Settings as _;
    let settings = omuzaki_settings_mock::MemorySettings::default();
    let person = Actor::Person { name: "conformance".into() };
    let current = omuzaki_settings_api::profile::ProfileSource::Current;
    settings.save_profile("Stream", current, false, Mode::Apply, &person).expect("the mock saves a profile");
    let go_live = omuzaki_settings_api::session::SetupProgress::new("go_live", Vec::new(), false);
    settings.session().set_setup(Some(go_live)).expect("the mock records setup");
    Arc::new(settings)
}
