//! The answer's transaction state machine (`tx.rs`) and the log cursors (`history.rs`) under every ordering: Stop at
//! each phase of an answer, the executor's outcome arriving late, closes that fail, and changes by others landing
//! anywhere. Ordering is by handshakes (held calls, a held stream, the host's watched clock), never by sleeping.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::event::AssistantEvent;
use omuzaki_agent_api::message::UserMessage;
use omuzaki_core::time::Clock as _;

/// A small deterministic generator, so a failing interleaving can be replayed from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((self.0 >> 33) % below as u64).unwrap_or(0)
    }
}

/// The standard tools plus `session_info`, whose heads-up the context block reads.
fn with_session_info(sessions: &Arc<FakeSessions>) -> ScriptedServer {
    let server = standard_server(sessions);
    server.add_fixed(tool("session_info", "session.info", "builtin", true), Outcome::text("session 1"));
    server
}

/// Checks what every answer's end must satisfy: `Done` only when the executor holds no transaction, `Unclosed` only when
/// it does; and the Stop cutoffs each name the answer whose `User` entry precedes them.
fn assert_closed_as_reported(rig: &Rig, end: &AssistantEvent, context: &str) {
    match end {
        AssistantEvent::Done => assert!(rig.sessions.open_tx().is_none(), "{context}: Done with an open transaction"),
        AssistantEvent::Unclosed => assert!(rig.sessions.open_tx().is_some(), "{context}: Unclosed with none open"),
        other => panic!("{context}: the answer ended with {other:?}"),
    }
    let entries = rig.sink.entries();
    let mut current = None;
    for entry in &entries {
        match entry {
            Entry::User { .. } => current = entry.answer_id(),
            Entry::Stopped { .. } => {
                assert!(entry.answer_id().is_some() && entry.answer_id() == current, "{context}: {entries:?}");
            }
            _ => {}
        }
    }
}

/// The conversation read back from the store agrees with the live transcript, and the export exists.
fn assert_replay_agrees(rig: &Rig, context: &str) {
    let entries = rig.sink.entries();
    let (resumed, markdown) = store_replay(&entries);
    assert_eq!(resumed.entries, rig.host.transcript(), "{context}: the resumed conversation is the live one");
    let stops = entries.iter().filter(|e| matches!(e, Entry::Stopped { .. })).count();
    assert_eq!(markdown.matches("Stopped by the person.").count(), stops, "{context}");
    assert_eq!(assistant_text(&resumed.entries), assistant_text(&rig.host.transcript()), "{context}");
}

/// Where a Stop can land: a held call of the named tool, or the held stream.
#[derive(Clone, Copy, Debug)]
enum Phase {
    /// The context block's `session_info`.
    Context,
    /// The model's edit.
    Edit,
    /// The model's query, with the transaction already open.
    Query,
    /// The commit at the answer's end.
    Commit,
    /// The second turn's stream, with the transaction open.
    Stream,
}

#[test]
fn stop_in_each_phase_closes_the_transaction_cuts_only_its_own_answer_and_replays_the_same() {
    for phase in [Phase::Context, Phase::Edit, Phase::Query, Phase::Commit, Phase::Stream] {
        // Never released: Stop cancels the held call, and the server drops it unanswered.
        let gate = Arc::new(tokio::sync::Notify::new());
        let (g, armed, holding) =
            (Arc::clone(&gate), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (a, h) = (Arc::clone(&armed), Arc::clone(&holding));
        let first = say_turn("FIRST-ANSWER");
        let edits = call_turn("Editing. ", &[("c1", "face_set_profile", "{}"), ("c2", "render_preview", "{}")]);
        // Events before the second answer's second turn: the first answer's 4, then the edit turn's 4 + 3 × 2.
        let stream_hold = matches!(phase, Phase::Stream).then_some(4 + 10 + 2);
        let rig = Rig::with(
            script(vec![first, edits, long_text(20), say_turn("spare")]),
            RigOptions { hold_after: stream_hold, ..RigOptions::default() },
            move |sessions| {
                let server = with_session_info(sessions);
                let held = |name: &str| match phase {
                    Phase::Context => name == "session_info",
                    Phase::Edit => name == "face_set_profile",
                    Phase::Query => name == "render_preview",
                    Phase::Commit => name == "tx_commit",
                    Phase::Stream => false,
                };
                // The held tool applies its effect, then waits: Stop arrives with the call in flight.
                let s = Arc::clone(sessions);
                for (name, kind, read_only) in [
                    ("session_info", "builtin", true),
                    ("face_set_profile", "edit", false),
                    ("render_preview", "query", true),
                    ("tx_commit", "builtin", false),
                ] {
                    if !held(name) {
                        continue;
                    }
                    let (s, g, title) = (Arc::clone(&s), Arc::clone(&g), name.to_owned());
                    let (a, h) = (Arc::clone(&a), Arc::clone(&h));
                    server.add(tool(name, &title, kind, read_only), move |_, _| {
                        match title.as_str() {
                            "face_set_profile" => s.own_change("face.set_profile"),
                            "tx_commit" => s.commit_open_tx(),
                            _ => {}
                        }
                        // Held in the second answer only, once its effect is applied.
                        if a.load(Ordering::SeqCst) {
                            h.store(true, Ordering::SeqCst);
                            return Outcome::text("done").held(&g);
                        }
                        Outcome::text("done")
                    });
                }
                server
            },
        );
        rig.say("one", 0.0);
        let shown_first = assistant_text(&rig.host.transcript());
        armed.store(true, Ordering::SeqCst);
        rig.host.send(UserMessage::new("two")).unwrap();
        match phase {
            Phase::Stream => rig.wait_parked(),
            _ => Rig::wait_for("the held call", || holding.load(Ordering::SeqCst)),
        }
        rig.host.stop();
        rig.gate.open();
        let events = rig.until_done(0.0);
        let context = format!("{phase:?}");
        assert_closed_as_reported(&rig, events.last().unwrap(), &context);
        assert_eq!(events.last(), Some(&AssistantEvent::Done), "{context}: {events:?}");
        rig.settle_steps(rig.sessions.committed().len());
        let steps: Vec<u64> = rig.steps().into_iter().map(|(tx, _, _)| tx).collect();
        assert_eq!(steps, rig.sessions.committed(), "{context}: a step for each commit, and only those");
        assert!(
            assistant_text(&rig.host.transcript()).starts_with(&shown_first),
            "{context}: the first answer is whole"
        );
        assert_replay_agrees(&rig, &context);
    }
}

/// What one seeded run went through, so the test can show its seeds reach every kind of ending.
#[derive(Default)]
struct Covered {
    changes: usize,
    stopped: usize,
    unclosed: usize,
    steps: usize,
    lagged: usize,
}

/// A seeded answer: one to three turns of one to two calls (an edit or a query), then a closing turn; and the number of
/// events the answer streams. Spare turns follow, for the messages after it.
fn seeded_turns(rng: &mut Lcg) -> (Vec<omuzaki_llm_mock::ScriptTurn>, usize) {
    let mut turns = Vec::new();
    let mut answer_events = 0;
    for t in 0..=rng.next(3) {
        let calls: Vec<(String, &str)> = (0..=rng.next(2))
            .map(|c| {
                let name = if rng.next(2) == 0 { "face_set_profile" } else { "render_preview" };
                (format!("c{t}{c}"), name)
            })
            .collect();
        let refs: Vec<(&str, &str, &str)> = calls.iter().map(|(id, name)| (id.as_str(), *name, "{}")).collect();
        answer_events += 4 + 3 * refs.len();
        turns.push(call_turn(&format!("T{t} "), &refs));
    }
    turns.push(say_turn("LAST"));
    answer_events += 4;
    // Leftover turns of a stopped answer are played by the next messages; spares keep the script from running out.
    turns.extend((0..6).map(|i| say_turn(&format!("SPARE{i}"))));
    (turns, answer_events)
}

/// The server of a seeded run: `session_info`; commits and rollbacks refused while `refusing` is set; and, while `armed`
/// is set, a change by somebody else before a seeded third of the calls, each named in `told`.
fn seeded_server(
    sessions: &Arc<FakeSessions>,
    refusing: &Arc<AtomicBool>,
    armed: &Arc<AtomicBool>,
    told: &Arc<Mutex<Vec<String>>>,
    seed: u64,
) -> ScriptedServer {
    sessions.other_change("before.zero");
    sessions.other_change("before.one");
    sessions.open_caught_up();
    let server = with_session_info(sessions);
    let (commit, r) = (Arc::clone(sessions), Arc::clone(refusing));
    server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
        if r.load(Ordering::SeqCst) {
            return Outcome::failure("cannot commit now");
        }
        commit.commit_open_tx();
        Outcome::text("tx committed")
    });
    let (rollback, r) = (Arc::clone(sessions), Arc::clone(refusing));
    server.add(tool("tx_rollback", "tx.rollback", "builtin", false), move |_, _| {
        if r.load(Ordering::SeqCst) {
            return Outcome::failure("cannot roll back now");
        }
        rollback.rollback_open_tx();
        Outcome::text("tx rolled back")
    });
    let (others, a, t, inject) =
        (Arc::clone(sessions), Arc::clone(armed), Arc::clone(told), Mutex::new(Lcg(seed ^ 0x9e37_79b9)));
    server.on_call(move |name, args| {
        // The start read's hook can run just after the test sees it counted; it is not one to land before.
        let start_read =
            name == "history_log" && args.get("after_n").and_then(serde_json::Value::as_u64) == Some(u64::MAX);
        if a.load(Ordering::SeqCst) && !start_read && inject.lock().unwrap().next(3) == 0 {
            let mut told = t.lock().unwrap();
            let op = format!("other.change.{}", told.len());
            others.other_change(&op);
            told.push(op);
        }
    });
    server
}

/// One seeded run: an answer of a few turns of calls, changes by others landing before random calls, a Stop at a random
/// event (or none), a random way the executor takes it, a log that may lag, and a commit that may be refused once.
/// Then a message that delivers what is held, and the checks.
fn one_interleaving(seed: u64, covered: &mut Covered) {
    let mut rng = Lcg(seed);
    let (turns, answer_events) = seeded_turns(&mut rng);
    let stop_at = (rng.next(4) != 0).then(|| 1 + rng.next(answer_events - 1));
    let stop_how = match rng.next(4) {
        0 => StopBehaviour::Committed,
        1 => StopBehaviour::Pending { close_after: 0.0 },
        2 => StopBehaviour::Pending { close_after: 0.3 },
        _ => StopBehaviour::Refused,
    };
    let lag = rng.next(2) == 0;
    let refuse_first_commit = rng.next(3) == 0;

    let armed = Arc::new(AtomicBool::new(false));
    // The executor refuses commits and rollbacks during the first answer only: its end, or the host's closes after a
    // refused Stop.
    let refusing = Arc::new(AtomicBool::new(refuse_first_commit));
    let told: Arc<Mutex<Vec<String>>> = Arc::default();
    let (r, a, t) = (Arc::clone(&refusing), Arc::clone(&armed), Arc::clone(&told));
    let options = RigOptions { hold_after: stop_at, ..RigOptions::default() };
    let rig = Rig::with(script(turns), options, move |sessions| seeded_server(sessions, &r, &a, &t, seed));
    rig.sessions.set_stop(stop_how, 0.0);
    rig.sessions.set_log_lag(lag);
    // The host has read where its numbering starts; only then may others' changes land.
    Rig::wait_for("the start read", || rig.server.host_log_reads() >= 1);
    armed.store(true, Ordering::SeqCst);

    let context = format!("seed {seed}: stop at {stop_at:?} ({stop_how:?}), lag {lag}, refuse {refuse_first_commit}");
    rig.host.send(UserMessage::new("go")).unwrap();
    if stop_at.is_some() {
        rig.wait_parked();
        rig.host.stop();
    }
    let first = rig.until_done(0.01);
    assert_closed_as_reported(&rig, first.last().unwrap(), &context);
    refusing.store(false, Ordering::SeqCst);
    rig.sessions.release_log();
    rig.gate.open();

    let second = rig.say("next", 0.01);
    assert_closed_as_reported(&rig, second.last().unwrap(), &context);
    assert_eq!(second.last(), Some(&AssistantEvent::Done), "{context}: the second message closes what was left");
    // Every change by others has now been told to the model or is held for the next message.
    armed.store(false, Ordering::SeqCst);
    rig.settle_steps(rig.sessions.committed().len());
    let mut steps: Vec<u64> = rig.steps().into_iter().map(|(tx, _, _)| tx).collect();
    steps.sort_unstable();
    assert_eq!(steps, rig.sessions.committed(), "{context}: one step for each commit, and only those");
    assert!(rig.sessions.open_tx().is_none(), "{context}");

    rig.say("check", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    for op in told.lock().unwrap().iter() {
        assert_eq!(
            request.matches(&format!("{op}\"")).count() + request.matches(&format!("{op} ")).count(),
            1,
            "{context}: {op} is told exactly once:\n{request}"
        );
    }
    assert!(!request.contains("before.zero") && !request.contains("before.one"), "{context}: not this session's");
    assert_replay_agrees(&rig, &context);

    covered.changes += told.lock().unwrap().len();
    covered.stopped += usize::from(stop_at.is_some());
    covered.unclosed += usize::from(first.last() == Some(&AssistantEvent::Unclosed));
    covered.steps += steps.len();
    covered.lagged += usize::from(lag && !steps.is_empty());
}

#[test]
fn random_interleavings_close_every_transaction_and_tell_every_change_exactly_once() {
    let mut covered = Covered::default();
    for seed in 0..120 {
        one_interleaving(seed, &mut covered);
    }
    // The seeds are not vacuous: they reach every kind of ending.
    assert!(covered.changes > 200, "{} changes by others", covered.changes);
    assert!(covered.stopped > 60 && covered.stopped < 120, "{} stopped", covered.stopped);
    assert!(covered.unclosed > 5, "{} ended Unclosed", covered.unclosed);
    assert!(
        covered.steps > 60 && covered.lagged > 20,
        "{} steps, {} with a lagging log",
        covered.steps,
        covered.lagged
    );
}

#[test]
fn a_heads_up_the_model_never_saw_reaches_it_once_in_the_next_message() {
    // The commit at the end of the answer is the host's own call: the heads-up its result carries is not the model's
    // to read there, so the next message's context gives it.
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done"), say_turn("ok")]),
        RigOptions::default(),
        |sessions| {
            let server = standard_server(sessions);
            let s = Arc::clone(sessions);
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
                s.other_change("during.the.commit");
                s.commit_open_tx();
                Outcome::text("tx committed")
            });
            server
        },
    );
    rig.say("go", 0.0);
    rig.say("again", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    assert_eq!(request.matches("during.the.commit").count(), 1, "{request}");
}

#[test]
fn a_change_after_session_info_is_reported_by_the_next_result_and_never_swallowed() {
    // The first server call after the context block's `session_info` is preceded by somebody's edit. Whatever that call
    // is, the edit must reach the model: a read of the log there would use up its heads-up.
    let after_info = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&after_info);
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("done"), say_turn("ok")]),
        RigOptions::default(),
        move |sessions| {
            let server = with_session_info(sessions);
            let s = Arc::clone(sessions);
            server.on_call(move |name, _| {
                if name == "session_info" {
                    flag.store(true, Ordering::SeqCst);
                } else if flag.swap(false, Ordering::SeqCst) {
                    s.other_change("right.after.info");
                }
            });
            server
        },
    );
    rig.say("go", 0.0);
    rig.say("again", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    assert_eq!(request.matches("right.after.info").count(), 1, "{request}");
}

#[test]
fn a_late_results_heads_up_reaches_the_next_message() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "render_preview", "{}")]), say_turn("ok"), say_turn("ok again")]),
        RigOptions { late_server: true, ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the call", || {
        seen.extend(std::iter::from_fn(|| rig.rx.try_recv()));
        seen.iter().any(|e| matches!(e, AssistantEvent::ToolCall { .. }))
    });
    rig.host.stop();
    rig.until_done(0.0);
    Rig::wait_for("the late result", || {
        rig.sink.entries().iter().any(|e| matches!(e, Entry::ToolResult { id, .. } if id == "c1:late"))
    });
    rig.say("again", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    assert!(request.contains("finished after the stop"), "{request}");
    assert_eq!(request.matches("late.change").count(), 1, "the heads-up the late result carried: {request}");
}

#[test]
fn the_models_own_log_read_counts_as_told() {
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "history_log", "{}")]),
            call_turn("", &[("c2", "face_set_profile", "{}")]),
            long_text(20),
            say_turn("ok"),
        ]),
        // Two one-call turns of 6 events each, then two events into the long one.
        RigOptions { hold_after: Some(6 + 6 + 2), ..RigOptions::default() },
        |sessions| {
            let server = standard_server(sessions);
            let s = Arc::clone(sessions);
            server.on_call(move |name, args| {
                if name == "history_log" && !args.contains_key("after_n") {
                    s.other_change("read.by.the.model");
                }
            });
            server
        },
    );
    rig.sessions.set_stop(StopBehaviour::Pending { close_after: 0.0 }, 0.0);
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    rig.host.stop();
    rig.until_done(0.0);
    // The idle read for the stopped step returns the change too; the model read it itself, so it is not listed.
    rig.settle_steps(1);
    rig.gate.open();
    rig.say("again", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    assert_eq!(request.matches("read.by.the.model").count(), 1, "in the model's own read only: {request}");
}

/// A turn that ends the answer with a prompt big enough that the next message compacts before its first turn.
fn heavy_ending(text: &str) -> omuzaki_llm_mock::ScriptTurn {
    omuzaki_llm_mock::ScriptTurn::new(vec![
        omuzaki_llm_api::stream::LlmEvent::start("m"),
        omuzaki_llm_api::stream::LlmEvent::TextDelta(text.into()),
        omuzaki_llm_api::stream::LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        omuzaki_llm_api::stream::LlmEvent::Stop(omuzaki_llm_api::stream::StopReason::EndTurn),
    ])
}

/// A `tx_commit` that somebody else's edit precedes: its heads-up reaches the host, never the model directly.
fn commit_after_an_edit_by_others(sessions: &Arc<FakeSessions>) -> ScriptedServer {
    let server = standard_server(sessions);
    let s = Arc::clone(sessions);
    server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
        s.other_change("during.the.commit");
        s.commit_open_tx();
        Outcome::text("tx committed")
    });
    server
}

#[test]
fn stop_during_the_close_of_unclosed_changes_does_not_wait_for_the_journal() {
    // The next message's commit of the first answer's changes is applied, and its reply waits on the journal (held,
    // never released): Stop abandons it at once, and the step keeps the first answer's rationale.
    let gate = Arc::new(tokio::sync::Notify::new());
    let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (g, n) = (Arc::clone(&gate), Arc::clone(&commits));
    let rig = Rig::with(
        script(vec![call_turn("RATIONALE-ONE ", &[("c1", "face_set_profile", "{}")]), say_turn("done"), say_turn("x")]),
        RigOptions::default(),
        move |sessions| {
            let server = standard_server(sessions);
            let s = Arc::clone(sessions);
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
                if n.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Outcome::failure("cannot commit now");
                }
                s.commit_open_tx();
                Outcome::text("tx committed").held(&g)
            });
            server
        },
    );
    assert_eq!(rig.say("one", 0.0).last(), Some(&AssistantEvent::Unclosed));
    rig.host.send(UserMessage::new("two")).unwrap();
    Rig::wait_for("the close of the first answer's changes", || commits.load(Ordering::SeqCst) == 2);
    let clicked = rig.clock.now().0;
    rig.host.stop();
    let events = rig.until_done(0.0);
    assert_eq!(events.last(), Some(&AssistantEvent::Done), "{events:?}");
    assert!(rig.clock.now().0 - clicked <= 0.1, "Done within D4's 100 ms");
    Rig::wait_for("the abandoned commit", || rig.server.cancelled() == 1);
    assert_eq!(rig.server.called("tx_rollback"), 0, "nothing was discarded");
    rig.settle_steps(1);
    let steps = rig.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].2.contains("RATIONALE-ONE"), "the first answer's rationale: {steps:?}");
    drop(gate);
}

#[test]
fn a_host_closed_before_the_log_shows_a_stopped_commit_records_the_step_when_it_reopens() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let g = Arc::clone(&gate);
    let rig = Rig::with(
        script(vec![call_turn("WHY-ONE ", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        move |sessions| {
            let server = standard_server(sessions);
            let s = Arc::clone(sessions);
            // The commit is applied; its reply is still on its way when Stop cuts the call.
            server.add(tool("tx_commit", "tx.commit", "builtin", false), move |_, _| {
                s.commit_open_tx();
                Outcome::text("tx committed").held(&g)
            });
            server
        },
    );
    rig.sessions.set_log_lag(true);
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the commit", || rig.server.called("tx_commit") == 1);
    rig.host.stop();
    assert_eq!(rig.until_done(0.0).last(), Some(&AssistantEvent::Done));
    // Closed at once, before the log has shown the commit.
    let entries = rig.sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::Retained { .. })), "the pending outcome is stored");
    assert!(rig.steps().is_empty());
    let sessions = Arc::clone(&rig.sessions);
    drop(rig);

    sessions.set_state(omuzaki_ops_api::SessionState::Active);
    sessions.open_caught_up();
    sessions.release_log();
    let reopened = Rig::with(
        script(vec![say_turn("hi")]),
        RigOptions { history: entries, sessions: Some(sessions), ..RigOptions::default() },
        standard_server,
    );
    reopened.settle_steps(1);
    let steps = reopened.steps();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps[0].1.ends_with("(stopped)") && steps[0].2.contains("WHY-ONE"), "{steps:?}");
    drop(gate);
}

#[test]
fn notices_owed_when_the_host_closes_are_given_by_the_reopened_host() {
    let rig = Rig::with(
        script(vec![call_turn("", &[("c1", "face_set_profile", "{}")]), say_turn("done")]),
        RigOptions::default(),
        commit_after_an_edit_by_others,
    );
    rig.say("go", 0.0);
    let entries = rig.sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::Notice { .. })), "the commit's heads-up is stored as owed");
    let sessions = Arc::clone(&rig.sessions);
    drop(rig);

    sessions.set_state(omuzaki_ops_api::SessionState::Active);
    sessions.open_caught_up();
    let reopened = Rig::with(
        script(vec![say_turn("ok")]),
        RigOptions { history: entries, sessions: Some(sessions), ..RigOptions::default() },
        standard_server,
    );
    reopened.say("again", 0.0);
    let request = request_text(reopened.llm.requests().last().unwrap());
    assert_eq!(request.matches("during.the.commit").count(), 1, "{request}");
}

#[test]
fn a_heads_up_lost_with_an_abandoned_internal_call_is_read_back_from_the_log() {
    // Somebody edits; the server writes `session_info`'s heads-up and moves the session's position past the edit, and
    // the reply is still on its way when Stop cuts the call. No transaction is retained, yet the edit must reach the
    // model.
    let gate = Arc::new(tokio::sync::Notify::new());
    let held = Arc::new(AtomicBool::new(false));
    let (g, h) = (Arc::clone(&gate), Arc::clone(&held));
    let rig = Rig::with(script(vec![say_turn("ok")]), RigOptions::default(), move |sessions| {
        let server = with_session_info(sessions);
        let s = Arc::clone(sessions);
        server.add(tool("session_info", "session.info", "builtin", true), move |_, n| {
            if n > 0 {
                return Outcome::text("session 1");
            }
            s.other_change("lost.with.the.reply");
            h.store(true, Ordering::SeqCst);
            Outcome::text("session 1").held_after_heads_up(&g)
        });
        server
    });
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the held reply", || held.load(Ordering::SeqCst));
    rig.host.stop();
    assert_eq!(rig.until_done(0.0).last(), Some(&AssistantEvent::Done));
    assert!(rig.steps().is_empty(), "no transaction was retained");
    rig.say("again", 0.0);
    let request = request_text(rig.llm.requests().last().unwrap());
    assert_eq!(request.matches("lost.with.the.reply").count(), 1, "{request}");
    drop(gate);
}

#[test]
fn a_compaction_before_the_first_turn_keeps_the_owed_notices_the_context_gave() {
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "face_set_profile", "{}")]),
            heavy_ending("DONE-ONE"),
            say_turn("SUMMARY-TEXT"),
            say_turn("ok"),
        ]),
        RigOptions { window: Some(20_000), prices: free_prices(), ..RigOptions::default() },
        commit_after_an_edit_by_others,
    );
    rig.say("one", 0.0);
    rig.say("two", 0.0);
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 4, "the second message compacted first");
    let request = request_text(requests.last().unwrap());
    assert!(request.contains("SUMMARY-TEXT"), "{request}");
    assert_eq!(request.matches("during.the.commit").count(), 1, "kept through the compaction: {request}");
}

#[test]
fn a_compaction_before_the_first_turn_keeps_a_late_result_the_context_gave() {
    let heavy_call = omuzaki_llm_mock::ScriptTurn::new(vec![
        omuzaki_llm_api::stream::LlmEvent::start("m"),
        omuzaki_llm_api::stream::LlmEvent::tool_use_start("c1", "render_preview"),
        omuzaki_llm_api::stream::LlmEvent::tool_use_end("c1"),
        omuzaki_llm_api::stream::LlmEvent::Usage(omuzaki_llm_api::usage::Usage::new(15_001, 10)),
        omuzaki_llm_api::stream::LlmEvent::Stop(omuzaki_llm_api::stream::StopReason::ToolUse),
    ]);
    let rig = Rig::with(
        script(vec![heavy_call, say_turn("SUMMARY-TEXT"), say_turn("ok")]),
        RigOptions { window: Some(20_000), prices: free_prices(), late_server: true, ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    let mut seen = Vec::new();
    Rig::wait_for("the call", || {
        seen.extend(std::iter::from_fn(|| rig.rx.try_recv()));
        seen.iter().any(|e| matches!(e, AssistantEvent::ToolCall { .. }))
    });
    rig.host.stop();
    rig.until_done(0.0);
    Rig::wait_for("the late result", || {
        rig.sink.entries().iter().any(|e| matches!(e, Entry::ToolResult { id, .. } if id == "c1:late"))
    });
    rig.say("again", 0.0);
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 3, "the second message compacted first");
    let request = request_text(requests.last().unwrap());
    assert!(request.contains("SUMMARY-TEXT") && request.contains("finished after the stop"), "{request}");
    assert_eq!(request.matches("late.change").count(), 1, "kept through the compaction: {request}");
}

#[test]
fn a_heads_up_lost_with_an_abandoned_call_survives_closing_the_host() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held = Arc::new(AtomicBool::new(false));
    let (g, h) = (Arc::clone(&gate), Arc::clone(&held));
    let rig = Rig::with(script(vec![]), RigOptions::default(), move |sessions| {
        let server = with_session_info(sessions);
        let s = Arc::clone(sessions);
        server.add(tool("session_info", "session.info", "builtin", true), move |_, n| {
            if n > 0 {
                return Outcome::text("session 1");
            }
            s.other_change("lost.before.closing");
            h.store(true, Ordering::SeqCst);
            Outcome::text("session 1").held_after_heads_up(&g)
        });
        server
    });
    rig.host.send(UserMessage::new("go")).unwrap();
    Rig::wait_for("the held reply", || held.load(Ordering::SeqCst));
    rig.host.stop();
    rig.until_done(0.0);
    let sessions = Arc::clone(&rig.sessions);
    let sink = Arc::clone(&rig.sink);
    // Closed before another message could read the log: the store already says it is unread, and nothing waits.
    drop(rig);
    let entries = sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::Unread { .. })), "{entries:?}");

    sessions.set_state(omuzaki_ops_api::SessionState::Active);
    sessions.open_caught_up();
    let reopened = Rig::with(
        script(vec![say_turn("ok")]),
        RigOptions { history: entries, sessions: Some(sessions), ..RigOptions::default() },
        with_session_info,
    );
    reopened.say("again", 0.0);
    let request = request_text(reopened.llm.requests().last().unwrap());
    assert_eq!(request.matches("lost.before.closing").count(), 1, "{request}");
    drop(gate);
}

#[test]
fn a_host_dropped_while_stopping_still_stores_the_cutoff() {
    // The first turn is stored whole before Stop; the person read none of it. Dropping the host right after Stop must
    // still store the cutoff, or a reopened conversation would show text nobody read.
    let rig = Rig::with(
        script(vec![call_turn("FIRST-TURN-TEXT ", &[("c1", "render_preview", "{}")]), long_text(20)]),
        RigOptions { hold_after: Some(7 + 2), ..RigOptions::default() },
        standard_server,
    );
    rig.host.send(UserMessage::new("go")).unwrap();
    rig.wait_parked();
    rig.host.stop();
    let sink = Arc::clone(&rig.sink);
    drop(rig);
    let entries = sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::Stopped { .. })), "{entries:?}");
    let (resumed, markdown) = store_replay(&entries);
    assert_eq!(assistant_text(&resumed.entries), "", "nothing was read");
    assert!(!markdown.contains("FIRST-TURN-TEXT"), "{markdown}");
}

/// Every kind of MCP request the host thread makes, for the held-reply test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    UiContext,
    ChipResource,
    Describe,
    SessionInfo,
    LogReadAtStart,
    TxBegin,
    ModelEdit,
    ModelLogResource,
    ApprovalWait,
    Rollback,
    Commit,
    RecoveryCommit,
}

const EVERY_REQUEST: [Request; 12] = [
    Request::UiContext,
    Request::ChipResource,
    Request::Describe,
    Request::SessionInfo,
    Request::LogReadAtStart,
    Request::TxBegin,
    Request::ModelEdit,
    Request::ModelLogResource,
    Request::ApprovalWait,
    Request::Rollback,
    Request::Commit,
    Request::RecoveryCommit,
];

/// The resource a held-request kind reads, if it is a resource read.
fn held_resource(kind: Request) -> Option<&'static str> {
    match kind {
        Request::UiContext => Some("omuzaki://ui/context"),
        Request::ChipResource => Some("omuzaki://note/1"),
        Request::ModelLogResource => Some("omuzaki://log"),
        _ => None,
    }
}

/// The server for one held request: the context tools and resources, and `kind` replaced by a version that, once
/// `armed`, lets somebody else edit, moves the session's position past the edit (the heads-up is written, or the log is
/// read), sets `held`, and holds its reply for ever.
fn held_request_server(
    sessions: &Arc<FakeSessions>,
    kind: Request,
    armed: &Arc<AtomicBool>,
    held: &Arc<AtomicBool>,
    gate: &Arc<tokio::sync::Notify>,
) -> ScriptedServer {
    let server = with_session_info(sessions);
    server.add_fixed(tool("project_describe", "Project", "query", true), Outcome::text("Project: one character."));
    server.resources.lock().unwrap().insert(
        "omuzaki://ui/context".into(),
        rmcp::model::ResourceContents::text("selection: head", "omuzaki://ui/context"),
    );
    server
        .resources
        .lock()
        .unwrap()
        .insert("omuzaki://note/1".into(), rmcp::model::ResourceContents::text("a note", "omuzaki://note/1"));
    if held_resource(kind).is_some() {
        // The test holds the resource itself (`Rig::server.held_resource`), right before the message.
        return server;
    }
    let (name, tool_kind, read_only) = match kind {
        Request::Describe => ("project_describe", "query", true),
        Request::SessionInfo => ("session_info", "builtin", true),
        Request::LogReadAtStart => ("history_log", "builtin", true),
        Request::TxBegin => ("tx_begin", "builtin", false),
        Request::ModelEdit => ("face_set_profile", "edit", false),
        Request::ApprovalWait => ("approval_wait", "builtin", false),
        Request::Rollback => ("tx_rollback", "builtin", false),
        _ => ("tx_commit", "builtin", false),
    };
    let (s, armed, held, gate) = (Arc::clone(sessions), Arc::clone(armed), Arc::clone(held), Arc::clone(gate));
    server.add(tool(name, name, tool_kind, read_only), move |args, _| {
        let after = args.get("after_n").and_then(serde_json::Value::as_u64);
        // What the request does when it runs normally.
        let normal = |s: &Arc<FakeSessions>| match kind {
            Request::Describe => Outcome::text("Project: one character."),
            Request::SessionInfo => Outcome::text("session 1"),
            Request::LogReadAtStart => {
                let (events, latest) = s.read_log(after.unwrap_or(0));
                Outcome::text(&serde_json::json!({"latest": latest, "events": events}).to_string())
            }
            Request::TxBegin => {
                s.begin_tx(args.get("label").and_then(serde_json::Value::as_str).unwrap_or("").to_owned());
                Outcome::text("tx started")
            }
            Request::ModelEdit => Outcome::text("applied"),
            Request::ApprovalWait => Outcome::text("approval applied"),
            Request::Rollback => {
                s.rollback_open_tx();
                Outcome::text("tx rolled back")
            }
            _ => {
                s.commit_open_tx();
                Outcome::text("tx committed")
            }
        };
        // The start read is never the one held, and nor is the first answer's commit of a recovery.
        let start_read = after == Some(u64::MAX);
        if start_read || !armed.load(Ordering::SeqCst) {
            if kind == Request::RecoveryCommit {
                return Outcome::failure("cannot commit now");
            }
            return normal(&s);
        }
        armed.store(false, Ordering::SeqCst);
        s.other_change("held.change");
        let out = normal(&s);
        held.store(true, Ordering::SeqCst);
        out.held_after_heads_up(&gate)
    });
    server
}

#[test]
fn stop_abandons_every_kind_of_request_at_once_and_the_next_message_reads_what_it_lost() {
    for kind in EVERY_REQUEST {
        let context = format!("{kind:?}");
        let (armed, held, gate) =
            (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(tokio::sync::Notify::new()));
        let edit = ("c1", "face_set_profile", "{}");
        let turns = match kind {
            Request::TxBegin | Request::ModelEdit | Request::ApprovalWait => vec![call_turn("", &[edit])],
            Request::ModelLogResource => {
                vec![call_turn("", &[("c1", "read_resource", "{\"uri\":\"omuzaki://log\"}")])]
            }
            Request::Rollback => vec![call_turn("", &[edit, ("c2", "discard_changes", "{}")])],
            Request::Commit => vec![call_turn("", &[edit]), say_turn("done")],
            Request::RecoveryCommit => vec![call_turn("", &[edit]), say_turn("done")],
            Request::LogReadAtStart => vec![call_turn("", &[edit]), long_text(20)],
            _ => vec![],
        };
        let mut all = turns;
        all.push(say_turn("ok"));
        let stream_hold = (kind == Request::LogReadAtStart).then_some(6 + 2);
        let (a, h, g) = (Arc::clone(&armed), Arc::clone(&held), Arc::clone(&gate));
        let rig =
            Rig::with(script(all), RigOptions { hold_after: stream_hold, ..RigOptions::default() }, move |sessions| {
                held_request_server(sessions, kind, &a, &h, &g)
            });
        // What comes first: an Unclosed transaction for the recovery; a retained outcome for the read at the start.
        match kind {
            Request::RecoveryCommit => assert_eq!(rig.say("one", 0.0).last(), Some(&AssistantEvent::Unclosed)),
            Request::LogReadAtStart => {
                rig.sessions.set_stop(StopBehaviour::Pending { close_after: 0.0 }, 0.0);
                rig.sessions.set_log_lag(true);
                rig.host.send(UserMessage::new("one")).unwrap();
                rig.wait_parked();
                rig.host.stop();
                rig.until_done(0.0);
                rig.gate.open();
                rig.sessions.set_stop(StopBehaviour::Committed, 0.0);
            }
            Request::ApprovalWait => rig.sessions.queue_approval(7),
            _ => {}
        }
        if kind == Request::ApprovalWait {
            let queued = omuzaki_ops_api::ErrorDetail::Queued {
                approval: omuzaki_ops_api::ApprovalId(7),
                need: omuzaki_ops_api::Need::Person { reason: "r".into() },
            };
            rig.server.add_fixed(
                tool("face_set_profile", "Face profile", "edit", false),
                Outcome::refusal("waiting", &queued),
            );
        }
        armed.store(true, Ordering::SeqCst);
        if let Some(uri) = held_resource(kind) {
            *rig.server.held_resource.lock().unwrap() = Some(HeldResource {
                uri: uri.to_owned(),
                op: "held.change".into(),
                held: Arc::clone(&held),
                gate: Arc::clone(&gate),
            });
        }
        let message = UserMessage::new("go");
        let message = if kind == Request::ChipResource {
            message.with_chips(vec![omuzaki_agent_api::message::ContextChip::Resource("omuzaki://note/1".into())])
        } else {
            message
        };
        rig.host.send(message).unwrap();
        Rig::wait_for(&format!("the held {context}"), || held.load(Ordering::SeqCst));
        let reads = rig.server.host_log_reads();
        let (clicked, started) = (rig.clock.now().0, std::time::Instant::now());
        rig.host.stop();
        let events = rig.until_done(0.0);
        // No wait of any length on the reply: on the host's clock, and no fixed wait in real time either (the removed
        // one was five seconds).
        assert!(rig.clock.now().0 - clicked <= 0.1, "{context}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{context}: {:?}", started.elapsed());
        assert!(events.last().is_some_and(AssistantEvent::ends_answer), "{context}: {events:?}");
        rig.sessions.release_log();

        rig.say("again", 0.0);
        assert!(rig.server.host_log_reads() > reads, "{context}: the next message read the log");
        let request = request_text(rig.llm.requests().last().unwrap());
        assert_eq!(request.matches("held.change").count(), 1, "{context}: told once:\n{request}");
        drop(gate);
    }
}

#[test]
fn notices_a_stopped_compaction_never_sent_still_reach_the_model_through_the_next_compaction() {
    // The second message's context carries a notice and is stored; Stop lands during its compaction's summary, before
    // any request carried that context. The third message compacts again: the notice must still reach the model.
    let rig = Rig::with(
        script(vec![
            call_turn("", &[("c1", "face_set_profile", "{}")]),
            heavy_ending("DONE-ONE"),
            say_turn("SUMMARY-A"),
            say_turn("SUMMARY-B"),
            say_turn("ok"),
        ]),
        RigOptions {
            window: Some(20_000),
            prices: free_prices(),
            hold_after: Some(6 + 4 + 2),
            ..RigOptions::default()
        },
        commit_after_an_edit_by_others,
    );
    rig.host.send(UserMessage::new("one")).unwrap();
    assert_eq!(rig.until_done(0.0).last(), Some(&AssistantEvent::Done));
    rig.host.send(UserMessage::new("two")).unwrap();
    rig.wait_parked();
    assert_eq!(rig.llm.requests().len(), 3, "parked in the second message's summary turn");
    rig.host.stop();
    rig.until_done(0.0);
    let entries = rig.sink.entries();
    assert!(entries.iter().any(|e| matches!(e, Entry::Context { text, .. } if text.contains("during.the.commit"))));
    assert!(!entries.iter().any(|e| matches!(e, Entry::Compacted { .. })), "the stopped compaction stored nothing");
    rig.gate.open();
    rig.say("three", 0.0);
    let requests = rig.llm.requests();
    assert_eq!(requests.len(), 5, "the third message compacted first");
    let request = request_text(requests.last().unwrap());
    assert!(request.contains("SUMMARY-B"), "{request}");
    assert_eq!(request.matches("during.the.commit").count(), 1, "{request}");
}
