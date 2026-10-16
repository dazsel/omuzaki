//! Expression triggers over a real `Performer` (PF-25): the bindings, the fades they produce on the channels, the refusals,
//! the `trigger.*` ops, and a perform-thread step that allocates nothing with bindings and presses in between.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim::{MAX_TRIGGERS, TriggerMode};
use omuzaki_anim_api::{Channel, ChannelBuf, DeviceProfile, FrameSource, HumanoidPose, LifeConfig, Source, TrackFrame};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_ops_api::{Actor, Effect, ErrorCode, OpCtx, OpError, Plan, ServicesBuilder, Undo, find};
use omuzaki_stage::perform::host::{PerformControl, PerformError, PerformHandle, PerformHost, perform_channel};
use omuzaki_stage::perform::triggers::{BindOutcome, TriggerBinding};
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};
use serde_json::{Value, json};

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

/// A tracker that never delivers a frame, so only the triggers move the face.
struct Silent;

impl FrameSource for Silent {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        None
    }
}

/// The time the middle of step `i` falls at, so one `advance` runs exactly one step.
fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(STEP_HZ))
}

/// Who owns the channels the tests watch after one step: `happy`'s smile, `angry`'s brow and `surprised`'s eyes.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Owners {
    smile: Source,
    brow: Source,
    wide: Source,
}

/// A performer on a silent tracker, its two channel ends, and the next step to run.
struct Rig {
    handle: PerformHandle,
    control: PerformControl,
    performer: Performer,
    next: u64,
}

impl Rig {
    fn new() -> Self {
        let parts = PerformerParts {
            source: Box::new(Silent),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
            life: Box::new(Life::new(LifeConfig::new(7))),
            animator: Box::new(MockAnimator),
        };
        let mut performer = Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST);
        let (handle, mut control) = perform_channel(&performer);
        control.advance(&mut performer, mid_step(0));
        Self { handle, control, performer, next: 1 }
    }

    /// Runs one step and reports who owns the watched channels after it.
    fn step(&mut self) -> Owners {
        let now = mid_step(self.next);
        self.next += 1;
        let out = self.control.advance(&mut self.performer, now).expect("one step is due");
        let owners = &out.bus.owners;
        Owners {
            smile: owners.of(Channel::MOUTH_SMILE_LEFT),
            brow: owners.of(Channel::BROW_DOWN_LEFT),
            wide: owners.of(Channel::EYE_WIDE_LEFT),
        }
    }

    /// Runs `n` steps, so queued requests reach the performer and the fades move.
    fn steps(&mut self, n: u64) {
        for _ in 0..n {
            self.step();
        }
    }

    /// Lets a queued request apply: one step takes up to four of them.
    fn settle(&mut self) {
        self.steps(1);
    }

    /// Binds `key` and lets the performer take the slot.
    fn bind(&mut self, key: &str, expression: &str, mode: TriggerMode) -> Result<BindOutcome, PerformError> {
        let binding = TriggerBinding { key: key.to_owned(), expression: id(expression), mode };
        let outcome = self.handle.bind_trigger(binding);
        self.settle();
        outcome
    }

    /// A full key press: down, the fade to its end, then up.
    fn tap(&mut self, key: &str) {
        self.handle.press(key, true).unwrap();
        self.settle();
        self.handle.press(key, false).unwrap();
        self.settle();
    }
}

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).unwrap()
}

/// A rig with the starter expressions and four keys: F1 happy (toggle), F2 angry (toggle), F3 surprised (hold) and F4
/// sad (hold).
fn starter_rig() -> Rig {
    let mut rig = Rig::new();
    rig.handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    rig.settle();
    rig.bind("F1", "happy", TriggerMode::Toggle).unwrap();
    rig.bind("F2", "angry", TriggerMode::Toggle).unwrap();
    rig.bind("F3", "surprised", TriggerMode::Hold).unwrap();
    rig.bind("F4", "sad", TriggerMode::Hold).unwrap();
    rig
}

#[test]
fn a_toggle_shows_after_its_fade_in_and_hides_on_the_next_press() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap();
    rig.steps(12); // 0.2 s
    assert_eq!(rig.step().smile, Source::Trigger);
    rig.tap("F1");
    rig.steps(15);
    assert_ne!(rig.step().smile, Source::Trigger, "the second press hides it");
}

#[test]
fn switching_fades_the_first_expression_out_before_the_second_appears() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap();
    rig.steps(12); // happy at full weight
    rig.handle.press("F1", false).unwrap(); // a toggle ignores its release
    rig.handle.press("F2", true).unwrap(); // switch to angry while happy is showing
    rig.settle();
    let mut last_smile = None;
    let mut first_brow = None;
    for i in 0..60 {
        let owners = rig.step();
        assert!(
            !(owners.smile == Source::Trigger && owners.brow == Source::Trigger),
            "step {i} carries values from both expressions"
        );
        if owners.smile == Source::Trigger {
            last_smile = Some(i);
        }
        if owners.brow == Source::Trigger && first_brow.is_none() {
            first_brow = Some(i);
        }
    }
    let (last_smile, first_brow) = (last_smile.expect("happy was shown"), first_brow.expect("angry appears"));
    assert!(last_smile < first_brow, "happy must be gone before angry shows");
    rig.tap("F2");
    rig.steps(15);
    assert_ne!(rig.step().brow, Source::Trigger, "the second press hides angry");
}

#[test]
fn a_hold_shows_while_the_key_is_down_and_fades_on_release() {
    let mut rig = starter_rig();
    rig.handle.press("F3", true).unwrap();
    rig.steps(12);
    assert_eq!(rig.step().wide, Source::Trigger, "shown while held");
    rig.handle.press("F3", false).unwrap();
    rig.steps(15);
    assert_ne!(rig.step().wide, Source::Trigger, "faded after release");
}

#[test]
fn a_rebake_keeps_the_presses_whose_expressions_did_not_change() {
    let mut rig = starter_rig();
    rig.handle.press("F3", true).unwrap(); // held down for the rest of the test
    rig.steps(40);
    assert_eq!(rig.step().wide, Source::Trigger, "F3 is shown while held");
    rig.handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    rig.steps(40);
    assert_eq!(rig.step().wide, Source::Trigger, "the same set leaves the held key shown");

    let mut without = ExpressionSet::starter();
    assert!(without.0.remove(&id("surprised")).is_some());
    rig.handle.set_expressions(without, ChannelBuf::NEUTRAL).unwrap();
    rig.steps(40);
    assert_ne!(rig.step().wide, Source::Trigger, "a set without the expression fades it out");
}

#[test]
fn a_rebake_keeps_a_toggled_expression_shown() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap();
    rig.steps(12);
    assert_eq!(rig.step().smile, Source::Trigger);
    rig.handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    rig.steps(40);
    assert_eq!(rig.step().smile, Source::Trigger, "a reload with the same set leaves the toggle on");
}

/// The starter set with `name` showing what `like` shows, so a rebake changes that slot's values.
fn with_values_of(name: &str, like: &str) -> ExpressionSet {
    let mut set = ExpressionSet::starter();
    let copy = set.0.get(&id(like)).expect("the starter set has the expression").clone();
    set.0.insert(id(name), copy);
    set
}

#[test]
fn a_rebake_that_changes_a_held_expression_keeps_it_held() {
    let mut rig = starter_rig();
    rig.handle.press("F3", true).unwrap(); // `surprised`, held down for the rest of the test
    rig.steps(40);
    assert_eq!(rig.step().wide, Source::Trigger);
    rig.handle.set_expressions(with_values_of("surprised", "angry"), ChannelBuf::NEUTRAL).unwrap();
    rig.steps(60);
    let owners = rig.step();
    assert_eq!(owners.brow, Source::Trigger, "the held key shows the new values");
    assert_ne!(owners.wide, Source::Trigger, "and no longer the old ones");
    rig.handle.press("F3", false).unwrap();
    rig.steps(40);
    assert_ne!(rig.step().brow, Source::Trigger, "the release still hides it");
}

#[test]
fn a_rebake_that_changes_a_toggled_expression_keeps_it_on() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap(); // `happy`, toggled on
    rig.steps(20);
    assert_eq!(rig.step().smile, Source::Trigger);
    rig.handle.set_expressions(with_values_of("happy", "angry"), ChannelBuf::NEUTRAL).unwrap();
    rig.steps(60);
    let owners = rig.step();
    assert_eq!(owners.brow, Source::Trigger, "the toggle stays on with the new values");
    assert_ne!(owners.smile, Source::Trigger);
    rig.tap("F1");
    rig.steps(40);
    assert_ne!(rig.step().brow, Source::Trigger, "the next press still toggles it off");
}

#[test]
fn an_expression_that_goes_missing_ends_its_press_and_the_key_must_be_pressed_again() {
    let mut rig = starter_rig();
    rig.handle.press("F3", true).unwrap(); // `surprised`, held
    rig.steps(40);
    assert_eq!(rig.step().wide, Source::Trigger);
    let mut without = ExpressionSet::starter();
    assert!(without.0.remove(&id("surprised")).is_some());
    rig.handle.set_expressions(without, ChannelBuf::NEUTRAL).unwrap();
    // While the expression is missing a press or release of F3 does not reach the performer, so nothing can say
    // whether the key is still down when the expression returns; showing it again could stick.
    assert_eq!(rig.handle.press("F3", false), Ok(false));
    rig.handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    rig.steps(60);
    assert_ne!(rig.step().wide, Source::Trigger, "the returned expression stays hidden until its key is pressed");
    rig.handle.press("F3", true).unwrap();
    rig.steps(40);
    assert_eq!(rig.step().wide, Source::Trigger, "pressed again, it shows");
}

#[test]
fn a_reset_drops_the_shown_expression_at_once_and_keeps_the_bindings() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap();
    rig.steps(20);
    assert_eq!(rig.step().smile, Source::Trigger);
    rig.control.reset(&mut rig.performer);
    assert_ne!(rig.step().smile, Source::Trigger, "tick zero after a reset carries no trigger");
    assert_eq!(rig.handle.triggers().len(), 4, "the bindings stay");
    rig.handle.press("F1", true).unwrap();
    rig.steps(20);
    assert_eq!(rig.step().smile, Source::Trigger, "a bound key works again after the reset");
}

#[test]
fn a_press_queued_before_a_reset_does_not_survive_it() {
    let mut rig = starter_rig();
    rig.handle.press("F1", true).unwrap(); // queued; the performer has not taken it yet
    rig.control.reset(&mut rig.performer);
    rig.steps(30);
    assert_ne!(rig.step().smile, Source::Trigger, "the key must be pressed again after a reset");
    assert_eq!(rig.handle.triggers().len(), 4, "the bindings stay");
    rig.handle.press("F1", true).unwrap();
    rig.steps(30);
    assert_eq!(rig.step().smile, Source::Trigger, "a press after the reset shows it");
}

#[test]
fn a_binding_queued_before_a_reset_is_still_applied() {
    let mut rig = starter_rig();
    let binding = TriggerBinding { key: "F6".to_owned(), expression: id("happy"), mode: TriggerMode::Hold };
    rig.handle.bind_trigger(binding).unwrap();
    rig.control.reset(&mut rig.performer); // the bind is still queued
    rig.handle.press("F6", true).unwrap();
    rig.steps(30);
    assert_eq!(rig.step().smile, Source::Trigger, "the bind reached the performer, so its key shows `happy`");
}

#[test]
fn the_seventeenth_binding_is_refused_and_a_repeated_key_replaces_its_binding() {
    let mut rig = starter_rig();
    let first = rig.bind("f1", "sad", TriggerMode::Hold).unwrap();
    assert_eq!(first, BindOutcome { slot: 0, replaced: true }, "`f1` is `F1`, so it takes F1's slot");
    for n in 5..=MAX_TRIGGERS {
        rig.bind(&format!("K{n}"), "happy", TriggerMode::Toggle).unwrap();
    }
    assert_eq!(rig.handle.triggers().len(), MAX_TRIGGERS);
    assert_eq!(rig.bind("K17", "happy", TriggerMode::Toggle), Err(PerformError::TooManyTriggers));
}

#[test]
fn unknown_and_malformed_keys_are_refused_and_unbind_reports_what_it_removed() {
    let mut rig = starter_rig();
    assert_eq!(rig.handle.press("nope", true), Err(PerformError::UnknownKey));
    assert_eq!(rig.bind("", "happy", TriggerMode::Toggle), Err(PerformError::InvalidKey));
    assert_eq!(rig.bind(&"k".repeat(33), "happy", TriggerMode::Toggle), Err(PerformError::InvalidKey));
    assert_eq!(rig.handle.unbind_trigger("nope"), Ok(false));
    assert_eq!(rig.handle.unbind_trigger("F4"), Ok(true));
    assert_eq!(rig.handle.press("F4", true), Err(PerformError::UnknownKey));
}

#[test]
fn a_binding_whose_expression_left_the_set_is_listed_missing_and_does_nothing() {
    let mut rig = starter_rig();
    let mut without_sad = ExpressionSet::starter();
    without_sad.0.remove(&id("sad"));
    rig.handle.set_expressions(without_sad, ChannelBuf::NEUTRAL).unwrap();
    rig.settle();
    let f4 = |rig: &Rig| rig.handle.triggers().into_iter().find(|t| t.binding.key == "F4").unwrap();
    assert!(f4(&rig).missing, "sad is no longer in the set");
    assert_eq!(rig.handle.press("F4", true), Ok(false));
    rig.steps(12);
    assert!(!f4(&rig).active, "pressing a missing expression shows nothing");
    rig.handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    rig.settle();
    assert!(!rig.handle.triggers().iter().any(|t| t.missing), "sad is back");
}

/// Plans `op` with `handle` as the performer.
fn run_plan(handle: &PerformHandle, op: &str, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    let host: Arc<dyn PerformHost> = Arc::new(handle.clone());
    let services = ServicesBuilder::new().insert::<dyn PerformHost>(host).build();
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Person { name: "performer".into() };
    let ctx = OpCtx::new(&doc, &actor, &clock).with_dry_run(dry_run).with_services(&services);
    let op = (find(op).expect("the op registers itself").decode)(args)?;
    op.plan(&ctx)
}

/// Runs `op` with `handle` as the performer and returns its answer.
fn run(handle: &PerformHandle, op: &str, args: Value, dry_run: bool) -> Result<Value, OpError> {
    Ok(run_plan(handle, op, args, dry_run)?.output.unwrap_or(Value::Null))
}

#[test]
fn the_ops_bind_list_press_and_unbind_through_the_host() {
    let mut rig = starter_rig();
    let bound = run(&rig.handle, "trigger.bind", json!({ "key": "F5", "expression": "happy" }), false).unwrap();
    assert_eq!(bound, json!({ "slot": 4, "replaced": false }));
    rig.settle();
    let listed = run(&rig.handle, "trigger.list", json!({}), false).unwrap();
    assert_eq!(listed["max"], json!(MAX_TRIGGERS));
    assert_eq!(listed["triggers"].as_array().unwrap().len(), 5);
    assert_eq!(run(&rig.handle, "trigger.press", json!({ "key": "F5" }), false).unwrap(), json!({ "pressed": true }));
    assert_eq!(run(&rig.handle, "trigger.unbind", json!({ "key": "F5" }), false).unwrap(), json!({ "removed": true }));
}

#[test]
fn an_unknown_expression_is_not_found_with_a_did_you_mean_hint() {
    let rig = starter_rig();
    let error = run(&rig.handle, "trigger.bind", json!({ "key": "F5", "expression": "hapy" }), false).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.hint.as_deref().unwrap_or_default().contains("happy"), "{:?}", error.hint);
    assert_eq!(rig.handle.triggers().len(), 4, "nothing was bound");
}

/// Every binding as `(key, expression, mode)`, in slot order, so two states compare.
fn table(rig: &Rig) -> Vec<(String, String, TriggerMode)> {
    rig.handle
        .triggers()
        .into_iter()
        .map(|t| (t.binding.key, t.binding.expression.as_str().to_owned(), t.binding.mode))
        .collect()
}

/// Runs one of an effect's stored calls.
fn replay(rig: &Rig, call: &omuzaki_ops_api::OpCall) {
    run(&rig.handle, &call.op, call.args.clone(), false).expect("a stored call runs");
}

#[test]
fn binding_a_new_key_is_undone_by_unbinding_it() {
    let rig = starter_rig();
    let before = table(&rig);
    let plan = run_plan(&rig.handle, "trigger.bind", json!({ "key": "f5", "expression": "happy" }), false).unwrap();
    let Effect { undo, redo, .. } = plan.effect.expect("a bind outside the document records its effect");
    assert_eq!(undo.op, "trigger.unbind");
    assert_eq!(undo.args, json!({ "key": "f5" }));
    assert_eq!(redo.op, "trigger.bind");
    assert_eq!(redo.args, json!({ "key": "f5", "expression": "happy", "mode": "toggle", "restore": true }));
    assert_eq!(rig.handle.triggers().len(), 5);
    replay(&rig, &undo);
    assert_eq!(table(&rig), before, "undo removes the new key");
    replay(&rig, &undo); // idempotent: unbinding an unbound key changes nothing
    assert_eq!(table(&rig), before);
    replay(&rig, &redo);
    assert_eq!(rig.handle.triggers().len(), 5, "redo binds it again");
}

#[test]
fn rebinding_a_key_is_undone_by_binding_what_it_was() {
    let rig = starter_rig();
    let before = table(&rig);
    let plan =
        run_plan(&rig.handle, "trigger.bind", json!({ "key": "f1", "expression": "sad", "mode": "hold" }), false)
            .unwrap();
    let effect = plan.effect.expect("a bind records its effect");
    assert_eq!(effect.undo.op, "trigger.bind");
    assert_eq!(
        effect.undo.args,
        json!({ "key": "F1", "expression": "happy", "mode": "toggle", "restore": true }),
        "the key as it was bound"
    );
    assert_ne!(table(&rig), before);
    replay(&rig, &effect.undo);
    assert_eq!(table(&rig), before, "undo puts the old binding back, in its slot");
    replay(&rig, &effect.redo);
    assert_eq!(table(&rig)[0], ("f1".to_owned(), "sad".to_owned(), TriggerMode::Hold));
}

#[test]
fn unbinding_is_undone_by_binding_the_key_again_and_an_unbound_key_records_nothing() {
    let rig = starter_rig();
    let plan = run_plan(&rig.handle, "trigger.unbind", json!({ "key": "f2" }), false).unwrap();
    let effect = plan.effect.expect("removing a binding records its effect");
    assert_eq!(effect.undo.op, "trigger.bind");
    assert_eq!(effect.undo.args, json!({ "key": "F2", "expression": "angry", "mode": "toggle", "restore": true }));
    assert_eq!(effect.redo.op, "trigger.unbind");
    assert_eq!(rig.handle.triggers().len(), 3);
    replay(&rig, &effect.undo);
    assert!(table(&rig).contains(&("F2".to_owned(), "angry".to_owned(), TriggerMode::Toggle)));
    replay(&rig, &effect.redo);
    assert_eq!(rig.handle.triggers().len(), 3, "redo removes it again");

    let none = run_plan(&rig.handle, "trigger.unbind", json!({ "key": "F9" }), false).unwrap();
    assert!(none.effect.is_none(), "nothing was bound, so there is nothing to undo");
}

#[test]
fn a_dry_run_records_no_effect_and_the_edits_declare_explicit_undo() {
    let rig = starter_rig();
    for (op, args) in
        [("trigger.bind", json!({ "key": "F5", "expression": "happy" })), ("trigger.unbind", json!({ "key": "F1" }))]
    {
        assert!(run_plan(&rig.handle, op, args, true).unwrap().effect.is_none(), "{op}: a dry run changes nothing");
        assert_eq!(((find(op).unwrap().meta)()).undo, Undo::Explicit, "{op} has effects outside the document");
    }
}

#[test]
fn a_dry_run_calls_nothing_and_the_schema_names_the_fields() {
    let rig = starter_rig();
    let answer = run(&rig.handle, "trigger.bind", json!({ "key": "F5", "expression": "happy" }), true).unwrap();
    assert_eq!(answer["dry_run"], json!(true));
    assert_eq!(rig.handle.triggers().len(), 4, "a dry run binds nothing");
    let schema = serde_json::to_value((find("trigger.bind").unwrap().schema)()).unwrap();
    let text = schema.to_string();
    for field in ["key", "expression", "mode"] {
        assert!(text.contains(&format!("\"{field}\"")), "the schema names {field}");
    }
}

#[test]
fn advance_allocates_nothing_with_four_bindings_and_presses_in_between() {
    let mut rig = starter_rig();
    let mut measured = 0;
    for step in 0..3600_u64 {
        if step % 100 == 0 {
            // Presses cross the queue on the ops' side; only the perform thread's advance is measured.
            rig.handle.press("F1", true).unwrap();
            rig.handle.press("F3", true).unwrap();
        }
        if step % 100 == 50 {
            rig.handle.press("F1", false).unwrap();
            rig.handle.press("F3", false).unwrap();
        }
        let now = mid_step(rig.next);
        rig.next += 1;
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        MEASURING.with(|m| m.set(true));
        let out = rig.control.advance(&mut rig.performer, now);
        MEASURING.with(|m| m.set(false));
        measured += usize::from(out.is_some());
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before, "step {step} allocated");
    }
    assert_eq!(measured, 3600);
}

#[test]
fn a_dry_run_of_an_unknown_expression_predicts_the_same_refusal_as_the_real_call() {
    let rig = starter_rig();
    let args = json!({ "key": "F5", "expression": "hapy" });
    let dry = run(&rig.handle, "trigger.bind", args.clone(), true).unwrap_err();
    let real = run(&rig.handle, "trigger.bind", args, false).unwrap_err();
    assert_eq!(dry.code, ErrorCode::NotFound);
    assert_eq!((dry.code, dry.message, dry.hint), (real.code, real.message, real.hint));
}

#[test]
fn the_host_checks_the_expression_under_the_same_lock_that_binds() {
    let mut rig = starter_rig();
    let mut without = ExpressionSet::starter();
    without.0.remove(&id("happy"));
    // The reload lands after an op read the set and before it binds: the bind itself refuses.
    rig.handle.set_expressions(without, ChannelBuf::NEUTRAL).unwrap();
    rig.settle();
    let binding = TriggerBinding { key: "F5".to_owned(), expression: id("happy"), mode: TriggerMode::Toggle };
    assert_eq!(rig.handle.bind_trigger_checked(binding.clone()), Err(PerformError::UnknownExpression));
    assert_eq!(rig.handle.triggers().len(), 4, "nothing was bound");
    assert!(rig.handle.bind_trigger(binding).is_ok(), "the unchecked bind keeps it, listed missing");
}

#[test]
fn undoing_an_unbind_restores_a_binding_whose_expression_is_missing() {
    let mut rig = starter_rig();
    let mut without = ExpressionSet::starter();
    without.0.remove(&id("sad"));
    rig.handle.set_expressions(without, ChannelBuf::NEUTRAL).unwrap();
    rig.settle();
    let before = table(&rig);
    let effect = run_plan(&rig.handle, "trigger.unbind", json!({ "key": "F4" }), false).unwrap().effect.unwrap();
    assert_eq!(rig.handle.triggers().len(), 3);
    replay(&rig, &effect.undo);
    assert_eq!(table(&rig), before);
    assert!(rig.handle.triggers().iter().find(|t| t.binding.key == "F4").unwrap().missing);
}

#[test]
fn a_switch_lists_the_outgoing_and_the_incoming_expression_as_active_until_the_outgoing_one_is_gone() {
    let mut rig = starter_rig();
    let active = |rig: &Rig| -> Vec<String> {
        rig.handle.triggers().into_iter().filter(|t| t.active).map(|t| t.binding.key).collect()
    };
    assert!(active(&rig).is_empty());
    rig.handle.press("F1", true).unwrap();
    rig.steps(12);
    assert_eq!(active(&rig), ["F1"]);
    rig.handle.press("F2", true).unwrap(); // switch while happy shows
    rig.steps(2);
    assert_eq!(active(&rig), ["F1", "F2"], "F1 is still fading out and F2 is wanted");
    rig.steps(30);
    assert_eq!(active(&rig), ["F2"], "F1 has faded out");
    rig.handle.press("F2", true).unwrap(); // toggled off
    rig.steps(2);
    assert_eq!(active(&rig), ["F2"], "fading out");
    rig.steps(30);
    assert!(active(&rig).is_empty());
}
