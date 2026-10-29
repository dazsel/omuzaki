//! Live control and drivers over a real `Performer` (`docs/design/drivers.md`): the `act.*` ops move the live face,
//! eyes and body through the score layer; a webcam wins the channels it reads until the member's drive map gives them
//! to the score; `drive.set` is saved with the scene and its undo reaches the live performer; each cast member
//! performs on its own; and a step with acts playing allocates nothing.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::float_cmp)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

#[path = "live/support.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim::{BusAnimator, BusAnimatorParams};
use omuzaki_anim_api::{
    Bus, Channel, ChannelBuf, Cues, DeviceProfile, FrameSource, HumanoidBone, HumanoidPose, LifeConfig, LifeLayer,
    Ownership, Source, Tick,
};
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ErrorCode, Grants, Mode, OpCtx, OpError, Request, Runner, Services, ServicesBuilder, find,
};
use omuzaki_stage::perform::cast::{CastBook, LiveCast};
use omuzaki_stage::perform::host::{ActRequest, PerformControl, PerformHandle, PerformHost, perform_channel};
use omuzaki_stage::perform::input::NoSource;
use omuzaki_stage::perform::score::Envelope;
use omuzaki_stage::perform::{Performer, PerformerParts};
use omuzaki_stage_api::drive::{DriveGroup, DriveMap, DriverKind};
use omuzaki_stage_api::{MemberId, SceneDef, SceneId, paths};
use serde_json::{Value, json};
use support::{mid_step, performer};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// What one step left on the bus and the body.
struct Step {
    values: ChannelBuf,
    owners: Ownership,
    pose: HumanoidPose,
}

/// A life layer that leaves the bus alone, so a test reads exactly what the drivers wrote.
struct Still;

impl LifeLayer for Still {
    fn step(&mut self, _tick: Tick, _cues: &Cues, _bus: &mut Bus) {}
    fn config(&self) -> &LifeConfig {
        static CONFIG: std::sync::OnceLock<LifeConfig> = std::sync::OnceLock::new();
        CONFIG.get_or_init(|| LifeConfig::new(0))
    }
    fn set_config(&mut self, _config: LifeConfig) {}
    fn reset(&mut self) {}
}

/// A performer with the starter expressions, the two ends of its channel, and the next step to run.
struct Rig {
    handle: PerformHandle,
    control: PerformControl,
    performer: Performer,
    next: u64,
}

impl Rig {
    /// A performer reading `source` (a webcam's frames), or nothing, with the real life layer.
    fn alive(source: Option<Box<dyn FrameSource>>) -> Self {
        Self::with(performer(&HumanoidPose::REST), source)
    }

    /// A performer reading `source`, or nothing, whose life layer leaves the bus alone.
    fn new(source: Option<Box<dyn FrameSource>>) -> Self {
        let parts = PerformerParts {
            source: Box::new(NoSource),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
            life: Box::new(Still),
            animator: Box::new(BusAnimator::new(BusAnimatorParams::default())),
        };
        Self::with(Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST), source)
    }

    fn with(mut performer: Performer, source: Option<Box<dyn FrameSource>>) -> Self {
        if let Some(source) = source {
            performer.set_source(source);
        }
        let (handle, mut control) = perform_channel(&performer);
        handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
        control.advance(&mut performer, mid_step(0));
        Self { handle, control, performer, next: 1 }
    }

    /// A performer on a webcam that sees a face the whole time.
    fn tracked() -> Self {
        let track = synthetic_track(&SyntheticSpec { seconds: 60.0, ..SyntheticSpec::default() });
        Self::new(Some(Box::new(TrackReplay::new(track))))
    }

    fn step(&mut self) -> Step {
        let now = mid_step(self.next);
        self.next += 1;
        let out = self.control.advance(&mut self.performer, now).expect("one step is due");
        Step { values: out.bus.values, owners: out.bus.owners, pose: *out.pose }
    }

    /// Runs `seconds` of steps and returns the last.
    fn run_for(&mut self, seconds: f32) -> Step {
        let steps = (seconds * 60.0).round().max(1.0) as u64;
        let mut last = self.step();
        for _ in 1..steps {
            last = self.step();
        }
        last
    }

    fn services(&self) -> Services {
        let host: Arc<dyn PerformHost> = Arc::new(self.handle.clone());
        ServicesBuilder::new().insert::<dyn PerformHost>(host).build()
    }
}

/// Runs `op` against `services` over an empty document, as a person would, and returns its answer.
fn run(services: &Services, op: &str, args: Value) -> Result<Value, OpError> {
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Person { name: "performer".into() };
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services);
    let op = (find(op).expect("the op registers itself").decode)(args)?;
    Ok(op.plan(&ctx)?.output.unwrap_or(Value::Null))
}

/// The value `happy` shows on the left smile at full strength over the neutral rest face.
fn happy_smile() -> f32 {
    let set = ExpressionSet::starter();
    let happy = ExpressionId::new("happy").unwrap();
    set.blend(&ChannelBuf::NEUTRAL, &[(&happy, 1.0)]).unwrap()[Channel::MOUTH_SMILE_LEFT]
}

#[test]
fn an_agent_s_expression_fades_in_holds_and_fades_out_on_the_live_face() {
    omuzaki_stage::link();
    let mut rig = Rig::new(None);
    let services = rig.services();
    let answer =
        run(&services, "act.expression", json!({ "expression": "happy", "hold_s": 0.5, "fade_s": 0.1 })).unwrap();
    assert_eq!(answer["act"], 1, "{answer}");
    let held = rig.run_for(0.3);
    assert!((held.values[Channel::MOUTH_SMILE_LEFT] - happy_smile()).abs() < 1e-5, "the face shows happy");
    assert_eq!(held.owners.of(Channel::MOUTH_SMILE_LEFT), Source::Score, "the score drives the smile");
    let listed = run(&services, "act.list", json!({})).unwrap();
    assert_eq!(listed["acts"][0]["act"]["expression"], "happy", "{listed}");
    let after = rig.run_for(0.5);
    assert!(after.values[Channel::MOUTH_SMILE_LEFT].abs() < 1e-5, "it has faded back to the rest face");
    assert_eq!(after.owners.of(Channel::MOUTH_SMILE_LEFT), Source::Rest);
    assert_eq!(run(&services, "act.list", json!({})).unwrap()["acts"], json!([]), "and is gone");
}

#[test]
fn the_life_layer_keeps_an_acted_face_alive_without_losing_it() {
    omuzaki_stage::link();
    let mut rig = Rig::alive(None);
    run(&rig.services(), "act.expression", json!({ "expression": "happy", "held": true })).unwrap();
    // Life's springs chase the acted face and its noise rides on it: a smile, moving.
    let smiles: Vec<f32> = (0..120).map(|_| rig.step().values[Channel::MOUTH_SMILE_LEFT]).collect();
    let (lo, hi) = smiles[60..].iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    assert!(lo > happy_smile() - 0.15 && hi < happy_smile() + 0.15, "still happy: {lo}..{hi}");
    assert!(hi - lo > 1e-4, "and alive: {lo}..{hi}");
}

#[test]
fn an_unknown_name_is_refused_with_the_nearest_and_a_dry_run_plays_nothing() {
    omuzaki_stage::link();
    let mut rig = Rig::new(None);
    let services = rig.services();
    let error = run(&services, "act.expression", json!({ "expression": "hapy" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.hint.as_deref().unwrap_or_default().contains("happy"), "{:?}", error.hint);
    let error = run(&services, "act.channels", json!({ "channels": { "jawOpn": 0.5 } })).unwrap_err();
    assert!(error.hint.as_deref().unwrap_or_default().contains("jawOpen"), "{:?}", error.hint);
    let error = run(&services, "act.clip", json!({ "clip": "wav" })).unwrap_err();
    assert!(error.hint.as_deref().unwrap_or_default().contains("wave"), "{:?}", error.hint);
    let error = run(&services, "act.look", json!({ "x": 3.0 })).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);

    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Person { name: "performer".into() };
    let ctx = OpCtx::new(&doc, &actor, &clock).with_dry_run(true).with_services(&services);
    let op = (find("act.channels").unwrap().decode)(json!({ "channels": { "jawOpen": 0.5 } })).unwrap();
    assert_eq!(op.plan(&ctx).unwrap().output.unwrap()["dry_run"], true);
    assert_eq!(rig.run_for(0.2).values[Channel::JAW_OPEN], 0.0, "a dry run moves nothing");
}

#[test]
fn channels_are_held_until_released_and_then_fade_from_where_they_are() {
    omuzaki_stage::link();
    let mut rig = Rig::new(None);
    let services = rig.services();
    let id = run(&services, "act.channels", json!({ "channels": { "jawOpen": 0.4, "headRoll": 9.0 }, "held": true }))
        .unwrap()["act"]
        .as_u64()
        .unwrap();
    let held = rig.run_for(5.0);
    assert!((held.values[Channel::JAW_OPEN] - 0.4).abs() < 1e-5, "held well past the default hold");
    assert!((held.values[Channel::HEAD_ROLL] - Channel::HEAD_ROLL.range().1).abs() < 1e-5, "held to its range");
    run(&services, "act.release", json!({ "act": id })).unwrap();
    assert!(rig.run_for(0.5).values[Channel::JAW_OPEN].abs() < 1e-5, "released, it fades out");
    let error = run(&services, "act.release", json!({ "act": id })).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "an act that ended can't be released again");
}

#[test]
fn a_webcam_wins_the_eyes_until_the_drive_map_gives_them_to_the_score() {
    omuzaki_stage::link();
    let mut rig = Rig::tracked();
    let services = rig.services();
    let yaw = Channel::HEAD_YAW;
    rig.run_for(0.5);
    assert_eq!(rig.step().owners.of(yaw), Source::Tracker, "the webcam drives the head");
    run(&services, "act.look", json!({ "at": "right", "head": 1.0, "held": true, "fade_s": 0.0 })).unwrap();
    let looked = rig.run_for(0.2);
    assert_eq!(looked.owners.of(yaw), Source::Tracker, "a look under a webcam: the webcam still wins (PF-23)");
    assert_eq!(looked.owners.of(Channel::EYE_LOOK_OUT_LEFT), Source::Score, "where the webcam reads no gaze, it shows");

    let mut map = DriveMap::open();
    map.set(DriveGroup::Head, DriverKind::Tracker, 0.0);
    rig.handle.set_drive(map.clone()).unwrap();
    let given = rig.run_for(0.2);
    assert_eq!(given.owners.of(yaw), Source::Score, "with the head gated off the webcam, the look turns it");
    let turned = 0.8 * omuzaki_stage::perform::host::LOOK_HEAD_YAW_RAD;
    assert!((given.values[yaw] - turned).abs() < 1e-5, "x 0.8 turns the head toward the picture's right");
    assert_eq!(given.owners.of(Channel::JAW_OPEN), Source::Tracker, "the webcam still drives the mouth");
    assert_eq!(rig.handle.drive(), map);
}

#[test]
fn a_clip_moves_the_body_under_the_tracked_head_and_ends_with_itself() {
    omuzaki_stage::link();
    let mut rig = Rig::new(None);
    let services = rig.services();
    let rest = rig.step().pose;
    let answer = run(&services, "act.clip", json!({ "clip": "wave" })).unwrap();
    let length = answer["envelope"]["hold"].as_f64().unwrap() + 0.5;
    let arm = |pose: &HumanoidPose| pose.rotations[HumanoidBone::RightUpperArm.index()];
    let waving = rig.run_for(0.4);
    assert!(arm(&waving.pose).angle_between(arm(&rest)) > 0.2, "the arm is up");
    rig.run_for(length as f32);
    let done = rig.step();
    assert!(arm(&done.pose).angle_between(arm(&rest)) < 1e-4, "the one-shot clip ended with itself");
}

#[test]
fn each_cast_member_performs_on_its_own() {
    omuzaki_stage::link();
    let (mut main, mut cohost) = (Rig::new(None), Rig::new(None));
    let cast = CastBook::new();
    cast.insert(MemberId::new("main").unwrap(), Arc::new(main.handle.clone()));
    cast.insert(MemberId::new("cohost").unwrap(), Arc::new(cohost.handle.clone()));
    let services = ServicesBuilder::new().insert::<dyn LiveCast>(Arc::new(cast) as Arc<dyn LiveCast>).build();
    let answer = run(&services, "act.expression", json!({ "expression": "happy", "member": "cohost" })).unwrap();
    assert_eq!(answer["member"], "cohost");
    run(&services, "act.channels", json!({ "channels": { "cheekPuff": 0.3 } })).unwrap();
    let (a, b) = (main.run_for(0.5), cohost.run_for(0.5));
    assert!(b.values[Channel::MOUTH_SMILE_LEFT] > 0.1 && a.values[Channel::MOUTH_SMILE_LEFT] == 0.0);
    assert!((a.values[Channel::CHEEK_PUFF] - 0.3).abs() < 1e-5, "no member named: the subject");
    assert_eq!(b.values[Channel::CHEEK_PUFF], 0.0);
    let error = run(&services, "act.look", json!({ "member": "cohst" })).unwrap_err();
    assert!(error.hint.as_deref().unwrap_or_default().contains("cohost"), "{:?}", error.hint);
    let listed = run(&services, "act.list", json!({ "member": "cohost" })).unwrap();
    assert_eq!(listed["members"], json!(["main", "cohost"]));
}

#[test]
fn a_step_with_acts_playing_and_a_gated_webcam_allocates_nothing() {
    omuzaki_stage::link();
    let mut rig = Rig::tracked();
    let mut map = DriveMap::open();
    map.set(DriveGroup::Head, DriverKind::Tracker, 0.0);
    rig.handle.set_drive(map).unwrap();
    let expression = ActRequest::Expression { expression: ExpressionId::new("happy").unwrap(), intensity: 1.0 };
    rig.handle.act(expression, Envelope::new(0.2, None)).unwrap();
    rig.handle.act(ActRequest::Look { x: 0.5, y: 0.2, head: 0.5 }, Envelope::new(0.2, None)).unwrap();
    rig.handle.act(ActRequest::clip("wave-both", 1.0), Envelope::new(0.2, None)).unwrap();
    rig.run_for(0.5);
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..600 {
        rig.step();
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    MEASURING.with(|m| m.set(false));
    assert_eq!(allocations, 0, "600 steps allocated {allocations} times");
}

/// A project with the scene `main` casting `kit` (its subject), over an executor that serves `services`.
fn project(services: Services) -> Executor {
    let mut doc = json!({ "characters": { "kit": {} }, "scenes": {} });
    let def = SceneDef::from_node(
        &json!({ "scene": { "name": "Main", "subject": "kit" }, "cast": { "kit": { "character": "kit" } } }),
    )
    .unwrap();
    let mut node = json!({});
    def.to_node(&mut node).unwrap();
    doc["scenes"]["main"] = node;
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    Executor::new(Box::new(MemDoc::from_value(doc)), clock).unwrap().with_services(services)
}

fn apply(exec: &Executor, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "you".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    exec.run(request).unwrap_or_else(|e| panic!("{op}: {e}")).plan.output.unwrap_or_default()
}

#[test]
fn a_drive_binding_is_saved_with_the_scene_and_its_undo_reaches_the_live_performer() {
    omuzaki_stage::link();
    let mut rig = Rig::tracked();
    let exec = project(rig.services());
    let you = Actor::Person { name: "you".into() };
    let set =
        apply(&exec, "drive.set", json!({ "member": "kit", "group": "gaze", "driver": "tracker", "weight": 0.0 }));
    assert_eq!(set["live"], true, "the subject's performer took it at once: {set}");
    let member = paths::member(&SceneId::new("main").unwrap(), &MemberId::new("kit").unwrap());
    let saved = exec.get(&member.join("drive"), &Grants::person()).unwrap();
    assert_eq!(saved, Some(json!({ "gaze": { "tracker": 0.0 } })), "saved on the cast member");
    let gated = rig.handle.drive();
    assert_eq!(gated.weight(DriveGroup::Gaze, DriverKind::Tracker), 0.0);
    rig.run_for(0.1);

    exec.undo(&you, &Grants::person()).unwrap().expect("one step to undo");
    assert_eq!(rig.handle.drive(), DriveMap::open(), "the undo reached the live performer");
    let path = DocPath::parse("scenes/main/cast/kit/drive").unwrap();
    assert_eq!(exec.get(&path, &Grants::person()).unwrap(), None, "and the scene");
    exec.redo(&you, &Grants::person()).unwrap().expect("one step to redo");
    assert_eq!(rig.handle.drive(), gated, "a redo too");

    let listed = apply(&exec, "drive.list", json!({}));
    assert_eq!(listed["members"][0]["drive"], json!({ "gaze": { "tracker": 0.0 } }), "{listed}");
    assert_eq!(listed["members"][0]["performing"], json!({ "gaze": { "tracker": 0.0 } }));
    let error = exec
        .run(Request {
            op: "drive.set".into(),
            args: json!({ "member": "kit", "group": "gaze", "driver": "tracker", "weight": 2.0 }),
            mode: Mode::Apply,
            actor: you.clone(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        })
        .unwrap_err();
    assert_eq!(error.field.as_deref(), Some("weight"), "{error}");
}
