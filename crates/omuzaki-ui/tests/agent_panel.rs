//! The Agent panel's sessions (#390, `shell.md` section 10): one row per session with every field, Stop always there,
//! Pause, Undo its steps and Revoke each calling their control exactly once after the two-step confirm, and the
//! grants popover reflecting and changing grants. The panel is drawn alone over a fake session registry that records
//! each control and sends the executor's events, so the rows update from events only.

mod fake_agents;

use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event as UiEvent, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use fake_agents::{Call, FakeAgents, Rig, click, confirm, paused, rig_with, session, shows, with_tx, you};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{Actor, Budget, EndReason, Event, Grants, Scope, Spend, Transport};
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::confirm::ARM_DELAY;
use omuzaki_ui::ds::set_strings;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, press, unlabelled_interactive};

/// Three sessions: an agent over stdio mid-step with three steps, one over HTTP with a budget, one paused.
fn three() -> Vec<omuzaki_ops_api::SessionInfo> {
    let mut stdio = with_tx(session(1, "claude_code", Transport::Stdio), "Round the jaw");
    stdio.model = Some("model_x".into());
    stdio.steps = 3;
    let mut http = session(2, "studio_bot", Transport::Http { token: "studio_token".into() });
    http.budget = Budget { usd_micros: 2_000_000, ..Budget::default() };
    http.spent = Spend { usd_micros: 400_000, tokens: 1200, seconds: 0 };
    let held = paused(session(3, "rigger_bot", Transport::Stdio));
    vec![stdio, http, held]
}

fn rig() -> (Harness<'static, Rig>, Arc<FakeClock>) {
    rig_with(FakeAgents::new(three(), Vec::new()), vec2(420.0, 1100.0))
}

#[test]
fn every_row_shows_who_for_whom_how_its_status_steps_and_spend() {
    let (h, _) = rig();
    for text in [
        "claude_code",
        "model_x",
        "For you \u{b7} MCP over stdio",
        "In progress: Round the jaw",
        "3 steps",
        "studio_bot",
        "For you \u{b7} MCP over HTTP (studio_token)",
        "$0.40 of $2.00 \u{b7} 1200 tokens",
        "rigger_bot",
    ] {
        assert!(shows(&h, text), "{text:?} in {:?}", all_strings(&h));
    }
    // States are words, not colours alone: a transaction open reads as working, a quiet session as idle.
    assert!(shows(&h, "Working"), "{:?}", all_strings(&h));
    assert!(shows(&h, "Idle"));
    assert!(shows(&h, "Paused"));
    // Stop is always there while a session runs.
    for name in ["claude_code", "studio_bot", "rigger_bot"] {
        assert!(h.query_by_label(&format!("Stop {name}")).is_some(), "Stop for {name}");
    }
    assert!(h.query_by_label("Resume rigger_bot").is_some() && h.query_by_label("Pause rigger_bot").is_none());
    assert!(unlabelled_interactive(&h).is_empty(), "{:?}", unlabelled_interactive(&h));
}

#[test]
fn rows_follow_the_session_events_without_asking_the_registry() {
    let (mut h, _) = rig();
    // A session that connects later appears from its event.
    h.state().fake.connect(session(4, "late_bot", Transport::Stdio));
    h.run_steps(3);
    assert!(shows(&h, "late_bot"));
    // A commit by its agent moves the step count at once.
    let actor = Actor::Agent { id: "late_bot#4".into(), model: None };
    h.state().fake.emit(Event::tx_committed(omuzaki_ops_api::TxId(7), "Bigger eyes", actor));
    h.run_steps(3);
    assert!(shows(&h, "1 steps") && shows(&h, "Working"), "{:?}", all_strings(&h));
}

#[test]
fn pause_resume_and_stop_call_their_control_once() {
    let (mut h, _) = rig();
    click(&mut h, "Pause claude_code");
    click(&mut h, "Resume rigger_bot");
    click(&mut h, "Stop studio_bot");
    assert_eq!(h.state().fake.calls(), [Call::Pause(1), Call::Resume(3), Call::Stop(2)]);
    assert!(h.query_by_label("Resume claude_code").is_some(), "the row follows the pause event");
}

#[test]
fn stop_pauses_on_an_executor_without_stop() {
    let fake = FakeAgents::new(three(), Vec::new()).without_stop();
    let (mut h, _) = rig_with(fake, vec2(420.0, 1100.0));
    click(&mut h, "Stop claude_code");
    assert_eq!(h.state().fake.calls(), [Call::Pause(1)]);
}

#[test]
fn revoke_ends_the_session_once_after_the_confirm_and_revokes_its_credential_by_default() {
    let (mut h, clock) = rig();
    click(&mut h, "Revoke\u{2026} studio_bot");
    assert!(shows(&h, "Revoke studio_bot?"), "the confirm asks first");
    assert!(h.state().fake.calls().is_empty(), "nothing is called before the confirm");
    let credential = h.get_by_role_and_label(Role::CheckBox, "Also revoke its access, so it can't connect again");
    assert_eq!(credential.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "checked by default");
    confirm(&mut h, &clock, "Revoke studio_bot", "Confirm: revoke studio_bot");
    assert_eq!(h.state().fake.calls(), [Call::End(2, EndReason::Revoked { by: you(), credential: true })]);
    h.run_steps(3);
    assert!(!shows(&h, "studio_bot"), "the ended session's row goes with its event");
}

#[test]
fn revoke_names_the_other_sessions_on_the_credential_and_can_keep_it() {
    let mut sessions = three();
    sessions.push(session(5, "helper_bot", Transport::Http { token: "studio_token".into() }));
    let (mut h, clock) = rig_with(FakeAgents::new(sessions, Vec::new()), vec2(420.0, 1300.0));
    click(&mut h, "Revoke\u{2026} studio_bot");
    assert!(shows(&h, "This also ends: helper_bot"), "{:?}", all_strings(&h));
    click(&mut h, "Also revoke its access, so it can't connect again");
    assert!(!shows(&h, "This also ends: helper_bot"), "unticked, the others stay");
    confirm(&mut h, &clock, "Revoke studio_bot", "Confirm: revoke studio_bot");
    assert_eq!(h.state().fake.calls(), [Call::End(2, EndReason::Revoked { by: you(), credential: false })]);
}

#[test]
fn a_stdio_session_has_no_credential_to_revoke() {
    let (mut h, clock) = rig();
    click(&mut h, "Revoke\u{2026} claude_code");
    assert!(h.query_by_label("Also revoke its access, so it can't connect again").is_none());
    confirm(&mut h, &clock, "Revoke claude_code", "Confirm: revoke claude_code");
    assert_eq!(h.state().fake.calls(), [Call::End(1, EndReason::Revoked { by: you(), credential: false })]);
}

#[test]
fn undo_its_steps_calls_undo_session_once_after_the_confirm_and_cancel_calls_nothing() {
    let (mut h, clock) = rig();
    click(&mut h, "Undo its 3 steps\u{2026} claude_code");
    click(&mut h, "Cancel");
    assert_eq!(h.state().fake.calls(), []);
    click(&mut h, "Undo its 3 steps\u{2026} claude_code");
    // A double click on the first step does not confirm: the second press must come after the dwell.
    click(&mut h, "Undo 3 steps");
    click(&mut h, "Confirm: undo 3 steps");
    assert!(h.state().fake.calls().is_empty(), "a press inside the dwell does not count");
    clock.advance(Seconds(ARM_DELAY + 0.1));
    h.run_steps(2);
    click(&mut h, "Confirm: undo 3 steps");
    assert_eq!(h.state().fake.calls(), [Call::UndoSession(1)]);
}

#[test]
fn the_grants_popover_reflects_and_changes_grants() {
    let (mut h, clock) = rig();
    click(&mut h, "Grants for claude_code");
    let checked = |h: &Harness<'_, Rig>, scope: &str| {
        h.get_by_role_and_label(Role::CheckBox, scope).accesskit_node().toggled()
            == Some(egui::accesskit::Toggled::True)
    };
    for scope in ["read", "edit:character", "edit:scene", "render"] {
        assert!(checked(&h, scope), "{scope} is granted by default");
    }
    for scope in ["broadcast", "publish", "spend", "native", "net"] {
        assert!(!checked(&h, scope), "{scope} is not");
    }
    assert!(shows(&h, "claude_code can edit the project, and every edit can be undone."), "{:?}", all_strings(&h));
    assert!(shows(&h, "This session only"));

    // A scope inside the project is granted (or taken back) at once.
    click(&mut h, "render");
    let without = Grants::agent_default().without(Scope::Render);
    assert_eq!(h.state().fake.calls(), [Call::Grants(1, without.clone())]);
    assert!(!checked(&h, "render"), "the box follows the session's event");

    // One that reaches outside the project waits for the two-step confirm.
    click(&mut h, "broadcast");
    assert_eq!(h.state().fake.calls().len(), 1, "nothing granted before the confirm");
    assert!(shows(&h, "Let claude_code use broadcast?"));
    confirm(&mut h, &clock, "Grant broadcast", "Confirm: grant broadcast");
    assert_eq!(h.state().fake.calls()[1], Call::Grants(1, without.with(Scope::Broadcast)));
    assert!(shows(&h, "claude_code can go live and switch scenes."), "the widest grant is said in words");
}

#[test]
fn spend_needs_a_cap_above_zero_and_the_cap_sets_the_budget() {
    let (mut h, _) = rig();
    click(&mut h, "Grants for claude_code");
    assert!(h.get_by_role_and_label(Role::CheckBox, "spend").accesskit_node().is_disabled(), "no cap, no spend");
    h.get_by_label("Spending cap in dollars").focus();
    h.run();
    for _ in 0..4 {
        press(&mut h, Modifiers::NONE, Key::Backspace);
    }
    h.event(UiEvent::Text("2.50".into()));
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().fake.calls(), [Call::Budget(1, Budget { usd_micros: 2_500_000, ..Budget::default() })]);
    assert!(!h.get_by_role_and_label(Role::CheckBox, "spend").accesskit_node().is_disabled());
}

#[test]
fn the_panel_works_from_the_keyboard_alone() {
    let (mut h, _) = rig();
    let pause = h.get_by_label("Pause claude_code");
    pause.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().fake.calls(), [Call::Pause(1)]);
    // Tab moves on to the session's next control.
    press(&mut h, Modifiers::NONE, Key::Tab);
    let focused =
        h.query_all_by_role(Role::Button).find(egui_kittest::Node::is_focused).and_then(|n| n.accesskit_node().label());
    assert!(focused.is_some(), "Tab reaches a control");
}

#[test]
fn every_word_goes_through_the_message_table() {
    let (mut h, _) = rig();
    click(&mut h, "Grants for claude_code");
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    let english = plain_english(all_strings(&h).iter().map(String::as_str));
    assert!(english.is_empty(), "plain English in the pseudo-locale: {english:?}");
}
