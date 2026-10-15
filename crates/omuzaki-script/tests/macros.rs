//! Macros over synthetic event streams and a fake runner: what is recorded, what merges, the file format and playback.
//! The end-to-end tests over the real executor are in `omuzaki-test`.

#![allow(clippy::unwrap_used, clippy::too_many_arguments)] // a test file: a failed unwrap is the failure; `applied` spells out one event

mod common;

use common::{Entry, FakeRunner};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch, Value};
use omuzaki_ops_api::{Actor, Cause, CauseKind, ErrorCode, Event, Grants, Mode, OpRecord, TxId};
use omuzaki_script::{FileError, Macro, MacroRecorder, RecordedCall, from_jsonl, play, to_jsonl};
use serde_json::json;

fn alice() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude-1".into(), model: None }
}

/// A patch that only replaces the value at `path`.
fn replaces(path: &str) -> Patch {
    Patch::new().with(DocPath::parse(path).unwrap(), Some(json!(0)), Some(json!(1)))
}

/// A patch that adds a value at `path`.
fn adds(path: &str) -> Patch {
    Patch::new().with(DocPath::parse(path).unwrap(), None, Some(json!(1)))
}

struct Stream {
    runner: FakeRunner,
    recorder: Option<MacroRecorder>,
    seq: u64,
}

impl Stream {
    fn new() -> Self {
        let runner = FakeRunner::default();
        let recorder = MacroRecorder::start(&runner, &alice(), &Grants::person()).unwrap();
        Self { runner, recorder: Some(recorder), seq: 0 }
    }

    fn applied(
        &mut self,
        actor: &Actor,
        tx: u64,
        at: f64,
        op: &str,
        args: Value,
        patch: Patch,
        cause: Option<CauseKind>,
    ) {
        self.seq += 1;
        let mut record =
            OpRecord::new(self.seq, op, args, actor.clone(), TxId(tx), Seconds(at), patch, DocVersion(self.seq));
        if let Some(kind) = cause {
            record = record.with_cause(Cause { kind, text: None });
        }
        self.runner.emit(&Event::op_applied(record));
    }

    /// Alice sets the blend of `character` to `value` in her own transaction `tx`, at time `at`, and commits it.
    fn blend(&mut self, tx: u64, at: f64, character: &str, value: f64) {
        self.edit(tx, at, "face.set_blend", json!({ "character": character, "blend": value }), replaces("face/blend"));
    }

    fn edit(&mut self, tx: u64, at: f64, op: &str, args: Value, patch: Patch) {
        self.applied(&alice(), tx, at, op, args, patch, None);
        self.runner.emit(&Event::tx_committed(TxId(tx), op, alice()));
    }

    fn undone(&self, tx: u64) {
        self.runner.emit(&Event::undone(TxId(tx), "x", alice(), alice(), DocVersion(0)));
    }

    fn redone(&self, tx: u64) {
        self.runner.emit(&Event::redone(TxId(tx), "x", alice(), alice(), DocVersion(0)));
    }

    fn finish(&mut self) -> Macro {
        self.recorder.take().unwrap().finish("m")
    }
}

#[test]
fn start_needs_read() {
    let runner = FakeRunner::default();
    let error = MacroRecorder::start(&runner, &alice(), &Grants::of([])).err().expect("denied");
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn it_keeps_the_persons_committed_edits_in_order() {
    let mut s = Stream::new();
    s.edit(1, 0.0, "face.set_profile", json!({ "character": "a", "profile": "egg" }), replaces("a"));
    s.edit(2, 5.0, "face.set_profile", json!({ "character": "b", "profile": "egg" }), replaces("b"));
    let m = s.finish();
    assert_eq!(m.name, "m");
    assert_eq!(m.steps.iter().map(|c| c.args["character"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "b"]);
}

#[test]
fn queries_other_actors_and_other_causes_are_never_recorded() {
    let mut s = Stream::new();
    s.edit(1, 0.0, "measure.likeness", json!({}), Patch::new());
    s.applied(&agent(), 2, 0.0, "face.set_profile", json!({ "character": "agent" }), replaces("x"), None);
    s.runner.emit(&Event::tx_committed(TxId(2), "agent", agent()));
    for (tx, kind) in [(3, CauseKind::Script), (4, CauseKind::Replay), (5, CauseKind::Prompt)] {
        s.applied(&alice(), tx, 0.0, "face.set_profile", json!({ "character": "scripted" }), replaces("y"), Some(kind));
        s.runner.emit(&Event::tx_committed(TxId(tx), "scripted", alice()));
    }
    s.applied(
        &alice(),
        6,
        0.0,
        "face.set_profile",
        json!({ "character": "direct" }),
        replaces("z"),
        Some(CauseKind::Direct),
    );
    s.runner.emit(&Event::tx_committed(TxId(6), "direct", alice()));
    let m = s.finish();
    assert_eq!(m.steps.len(), 1, "{m:?}");
    assert_eq!(m.steps[0].args["character"], "direct");
}

#[test]
fn a_rolled_back_or_unfinished_transaction_is_not_recorded() {
    let mut s = Stream::new();
    s.applied(&alice(), 1, 0.0, "face.set_profile", json!({ "character": "a" }), replaces("a"), None);
    s.runner.emit(&Event::tx_rolled_back(TxId(1), alice(), DocVersion(1)));
    s.applied(&alice(), 2, 0.0, "face.set_profile", json!({ "character": "open" }), replaces("b"), None);
    assert_eq!(s.finish().steps, vec![]);
}

#[test]
fn a_transaction_with_several_ops_enters_on_its_commit() {
    let mut s = Stream::new();
    s.applied(&alice(), 1, 0.0, "face.set_profile", json!({ "character": "a" }), replaces("a"), None);
    s.applied(&alice(), 1, 0.0, "face.set_profile", json!({ "character": "b" }), replaces("b"), None);
    s.recorder.as_mut().unwrap().poll();
    s.runner.emit(&Event::tx_committed(TxId(1), "both", alice()));
    assert_eq!(s.finish().steps.len(), 2);
}

fn characters(m: &Macro) -> Vec<String> {
    m.steps.iter().map(|c| c.args["character"].as_str().unwrap().to_owned()).collect()
}

fn three_edits(s: &mut Stream) {
    for (tx, name) in [(1, "a"), (2, "b"), (3, "c")] {
        s.edit(
            tx,
            f64::from(u32::try_from(tx).unwrap()),
            "face.set_profile",
            json!({ "character": name }),
            replaces(name),
        );
    }
}

#[test]
fn undo_takes_an_edit_out() {
    let mut s = Stream::new();
    three_edits(&mut s);
    s.undone(2);
    assert_eq!(characters(&s.finish()), ["a", "c"]);
}

#[test]
fn redo_puts_an_edit_back_where_it_was() {
    let mut s = Stream::new();
    three_edits(&mut s);
    s.undone(2);
    s.undone(3);
    s.redone(2);
    assert_eq!(characters(&s.finish()), ["a", "b"], "3 stays undone, 2 is back in its place");
}

#[test]
fn another_actors_undo_does_not_remove_the_persons_edit() {
    let mut s = Stream::new();
    s.edit(1, 0.0, "face.set_profile", json!({ "character": "a" }), replaces("a"));
    s.runner.emit(&Event::undone(TxId(1), "x", agent(), agent(), DocVersion(0)));
    assert_eq!(s.finish().steps.len(), 1);
}

// -- coalescing -----------------------------------------------------------------------------------------------------

#[test]
fn a_slider_drag_of_an_absolute_setter_is_one_call_with_the_last_value() {
    let mut s = Stream::new();
    for i in 0..20 {
        s.blend(i + 1, f64::from(u32::try_from(i).unwrap()) * 0.01, "aki", f64::from(u32::try_from(i).unwrap()) / 20.0);
    }
    let m = s.finish();
    assert_eq!(
        m.steps,
        vec![RecordedCall { op: "face.set_blend".into(), args: json!({ "character": "aki", "blend": 0.95 }) }]
    );
}

#[test]
fn a_drag_inside_one_transaction_merges_whatever_the_time_between() {
    let mut s = Stream::new();
    for (i, at) in [0.0, 30.0, 60.0].into_iter().enumerate() {
        s.applied(
            &alice(),
            1,
            at,
            "face.set_blend",
            json!({ "character": "aki", "blend": i }),
            replaces("face/blend"),
            None,
        );
    }
    s.runner.emit(&Event::tx_committed(TxId(1), "drag", alice()));
    let m = s.finish();
    assert_eq!(m.steps.len(), 1);
    assert_eq!(m.steps[0].args["blend"], 2);
}

#[test]
fn transactions_more_than_a_second_apart_do_not_merge() {
    let mut s = Stream::new();
    s.blend(1, 0.0, "aki", 0.1);
    s.blend(2, 1.5, "aki", 0.2);
    assert_eq!(s.finish().steps.len(), 2);
}

#[test]
fn what_is_not_a_drag_is_kept_as_recorded() {
    // A different character is not a number change.
    let mut s = Stream::new();
    s.blend(1, 0.0, "aki", 0.1);
    s.blend(2, 0.1, "ren", 0.2);
    assert_eq!(s.finish().steps.len(), 2);

    // An op that does not declare `coalesces` stays one call per gesture, even when only a number differs: a relative
    // `x += amount` called with 1 then 2 must replay to 3, not 2.
    let mut s = Stream::new();
    s.edit(1, 0.0, "face.set_profile", json!({ "character": "aki", "blend": 1 }), replaces("face/blend"));
    s.edit(2, 0.1, "face.set_profile", json!({ "character": "aki", "blend": 2 }), replaces("face/blend"));
    assert_eq!(s.finish().steps.len(), 2);

    // Two calls that add a mark each (differing only in size) make two marks: the patches add, they don't replace.
    let mut s = Stream::new();
    s.edit(1, 0.0, "face.set_blend", json!({ "character": "aki", "blend": 1 }), adds("marks/0"));
    s.edit(2, 0.1, "face.set_blend", json!({ "character": "aki", "blend": 2 }), adds("marks/1"));
    assert_eq!(s.finish().steps.len(), 2);

    // The same call replacing different paths is not the same setter.
    let mut s = Stream::new();
    s.edit(1, 0.0, "face.set_blend", json!({ "character": "aki", "blend": 1 }), replaces("a"));
    s.edit(2, 0.1, "face.set_blend", json!({ "character": "aki", "blend": 2 }), replaces("b"));
    assert_eq!(s.finish().steps.len(), 2);

    // Another op in between breaks the run.
    let mut s = Stream::new();
    s.blend(1, 0.0, "aki", 0.1);
    s.edit(2, 0.1, "face.set_profile", json!({ "character": "aki" }), replaces("p"));
    s.blend(3, 0.2, "aki", 0.2);
    assert_eq!(s.finish().steps.len(), 3);
}

#[test]
fn undoing_the_last_of_a_drag_falls_back_to_the_one_before_and_redo_restores_it() {
    let mut s = Stream::new();
    s.blend(1, 0.0, "aki", 0.1);
    s.blend(2, 0.1, "aki", 0.2);
    s.blend(3, 0.2, "aki", 0.3);
    s.undone(3);
    s.recorder.as_mut().unwrap().poll();
    let blend = |m: &Macro| m.steps[0].args["blend"].as_f64().unwrap();
    let mut other = Stream::new();
    for (tx, v) in [(1, 0.1), (2, 0.2), (3, 0.3)] {
        other.blend(tx, 0.0, "aki", v);
    }
    other.undone(3);
    other.redone(3);
    assert!((blend(&other.finish()) - 0.3).abs() < 1e-9, "redo brings the last value back");
    let m = s.finish();
    assert_eq!(m.steps.len(), 1);
    assert!((blend(&m) - 0.2).abs() < 1e-9, "undo falls back to the latest remaining source");
}

#[test]
fn undoing_every_source_of_a_drag_removes_the_step() {
    let mut s = Stream::new();
    s.blend(1, 0.0, "aki", 0.1);
    s.blend(2, 0.1, "aki", 0.2);
    s.undone(2);
    s.undone(1);
    assert_eq!(s.finish().steps, vec![]);
}

// -- the file -------------------------------------------------------------------------------------------------------

fn sample() -> Macro {
    Macro {
        name: "oval and \"blush\"".into(),
        steps: vec![
            RecordedCall {
                op: "face.set_profile".into(),
                args: json!({ "character": "aki", "profile": "egg-oval", "blend": 0.5 }),
            },
            RecordedCall { op: "face.set_blush".into(), args: json!({}) },
        ],
    }
}

#[test]
fn the_file_is_a_header_line_and_one_call_per_line() {
    let text = to_jsonl(&sample());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], r#"{"macro":1,"name":"oval and \"blush\""}"#);
    assert_eq!(lines[1], r#"{"op":"face.set_profile","args":{"blend":0.5,"character":"aki","profile":"egg-oval"}}"#);
    assert!(text.ends_with('\n'));
}

#[test]
fn jsonl_round_trip_is_byte_stable() {
    for m in [
        sample(),
        Macro { name: String::new(), steps: vec![] },
        Macro {
            name: "é ü 日本".into(),
            steps: vec![RecordedCall { op: "a.b".into(), args: json!({ "k": [1, 2.5, null, "x"] }) }],
        },
    ] {
        let text = to_jsonl(&m);
        let back = from_jsonl(&text).unwrap();
        assert_eq!(back, m);
        assert_eq!(to_jsonl(&back), text, "the same bytes");
    }
}

fn refused(text: &str) -> FileError {
    from_jsonl(text).unwrap_err()
}

#[test]
fn from_jsonl_refuses_with_the_line_number() {
    let header = "{\"macro\":1,\"name\":\"m\"}\n";
    assert_eq!(refused("").line, 1);
    assert_eq!(refused("not json\n").line, 1);
    assert_eq!(refused("[1]\n").line, 1);
    let unknown = refused("{\"macro\":2,\"name\":\"m\"}\n");
    assert_eq!(unknown.line, 1);
    assert!(unknown.reason.contains("unknown macro version 2"), "{unknown}");
    assert_eq!(refused("{\"name\":\"m\"}\n").line, 1);
    assert_eq!(refused("{\"macro\":1}\n").line, 1);
    let missing_op = refused(&format!("{header}{{\"op\":\"a.b\",\"args\":{{}}}}\n{{\"args\":{{}}}}\n"));
    assert_eq!(missing_op.line, 3);
    assert!(missing_op.reason.contains("`op`"), "{missing_op}");
    for bad in ["{\"op\":\"a.b\",\"args\":[1]}", "{\"op\":\"a.b\",\"args\":3}", "{\"op\":\"a.b\"}"] {
        let error = refused(&format!("{header}{bad}\n"));
        assert_eq!(error.line, 2, "{bad}");
        assert!(error.reason.contains("must be an object"), "{error}");
    }
    let broken = refused(&format!("{header}{{\"op\":\"a.b\",\"args\":{{}}}}\n{{oops\n"));
    assert_eq!(broken.line, 3);
    let blank = refused(&format!("{header}\n{{\"op\":\"a.b\",\"args\":{{}}}}\n"));
    assert_eq!(blank.line, 2);
    assert!(broken.to_string().starts_with("line 3: "), "{broken}");
}

#[test]
fn a_file_without_the_final_line_break_still_reads() {
    let text = to_jsonl(&sample());
    assert_eq!(from_jsonl(text.trim_end()).unwrap(), sample());
}

// -- playback -------------------------------------------------------------------------------------------------------

fn macro_of(ops: &[(&str, Value)]) -> Macro {
    Macro {
        name: "play me".into(),
        steps: ops.iter().map(|(op, args)| RecordedCall { op: (*op).into(), args: args.clone() }).collect(),
    }
}

#[test]
fn play_runs_every_step_in_one_transaction_with_the_script_cause() {
    let runner = FakeRunner::default();
    let m = macro_of(&[
        ("face.set_profile", json!({ "character": "a" })),
        ("face.set_profile", json!({ "character": "b" })),
    ]);
    let report = play(&runner, &m, &alice(), &Grants::person()).unwrap();
    assert_eq!(report.steps, 2);
    assert_eq!((runner.begins(), runner.commits(), runner.rollbacks()), (1, 1, 0));
    let requests = runner.requests();
    assert_eq!(requests.len(), 2);
    let tx = requests[0].tx.expect("a transaction");
    for request in &requests {
        assert_eq!(request.mode, Mode::Apply);
        assert_eq!(request.tx, Some(tx));
        assert_eq!(request.actor, alice());
        assert_eq!(request.cause, Some(Cause { kind: CauseKind::Script, text: Some("play me".into()) }));
        assert_eq!(request.base, None);
    }
    assert!(matches!(runner.entries().last(), Some(Entry::Commit(t)) if *t == tx));
}

#[test]
fn a_failing_step_rolls_everything_back_and_names_the_step() {
    let runner = FakeRunner::default();
    let m = macro_of(&[
        ("face.set_profile", json!({ "character": "a" })),
        ("face.fail", json!({})),
        ("face.set_profile", json!({ "character": "never" })),
    ]);
    let error = play(&runner, &m, &alice(), &Grants::person()).expect_err("step 2 fails");
    assert_eq!((error.step, error.op.as_str()), (2, "face.fail"));
    assert_eq!(error.error.code, ErrorCode::Invalid);
    assert_eq!(error.error.field.as_deref(), Some("blend"), "the op's own error");
    assert_eq!(runner.requests().len(), 2, "the third step never ran");
    runner.assert_rolled_back();
    assert!(error.to_string().contains("step 2"), "{error}");
}

#[test]
fn an_empty_macro_plays_without_opening_a_transaction() {
    let runner = FakeRunner::default();
    let report = play(&runner, &macro_of(&[]), &alice(), &Grants::person()).unwrap();
    assert_eq!(report.steps, 0);
    assert_eq!(runner.begins(), 0);
}

#[test]
fn a_failed_commit_rolls_back_and_names_the_commit() {
    let runner = FakeRunner::default();
    runner.fail_commit.store(true, std::sync::atomic::Ordering::SeqCst);
    let m = macro_of(&[("face.set_profile", json!({ "character": "a" }))]);
    let error = play(&runner, &m, &alice(), &Grants::person()).expect_err("the commit fails");
    assert_eq!((error.step, error.op.as_str()), (1, "commit"));
    runner.assert_rolled_back();
}
