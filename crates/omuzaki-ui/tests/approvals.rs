//! Approval cards (#420, `shell.md` section 10, `agent-sessions.md` section 7): batching, decisions offered per need,
//! "waiting for you" on the asking session's row, the stack at the top right of the window while the Agent panel is
//! not on screen, and no timer ever deciding anything.

mod fake_agents;

use std::sync::Arc;

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use fake_agents::{Call, FakeAgents, Rig, approval, click, confirm, rig_with, session, shows};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Budget, Decision, Need, Scope, Spend, Transport};
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::set_strings;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, unlabelled_interactive};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::agent::approvals::batches;
use omuzaki_ui::shell::Shell;

fn scope(scope: Scope) -> Need {
    Need::Scope { scope }
}

/// Six identical requests from the stdio agent and one different one from the HTTP agent.
fn seven() -> FakeAgents {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let http = session(2, "studio_bot", Transport::Http { token: "studio_token".into() });
    let mut approvals: Vec<_> = (1..=6).map(|i| approval(i, &stdio, "render.preview", scope(Scope::Render))).collect();
    approvals.push(approval(7, &http, "character.rename", scope(Scope::EditCharacter)));
    FakeAgents::new(vec![stdio, http], approvals)
}

fn cards(h: &Harness<'_, Rig>) -> Vec<String> {
    h.query_all_by_role(Role::Group)
        .filter_map(|n| egui_kittest::kittest::NodeT::accesskit_node(&n).label())
        .filter(|l| l.contains(" needs you: "))
        .collect()
}

#[test]
fn identical_requests_collapse_into_one_card_and_approve_all_resolves_each_exactly_once() {
    let (mut h, _) = rig_with(seven(), vec2(420.0, 1200.0));
    assert_eq!(
        cards(&h),
        ["claude_code needs you: Run render.preview", "studio_bot needs you: Run character.rename"],
        "six identical requests are one card"
    );
    assert!(shows(&h, "6 similar requests"));
    click(&mut h, "Approve all 6 similar requests");
    let calls = h.state().fake.calls();
    assert_eq!(calls, (1..=6).map(|id| Call::Resolve(id, Decision::Approve)).collect::<Vec<_>>());
    h.run_steps(3);
    assert_eq!(cards(&h), ["studio_bot needs you: Run character.rename"], "the batch's card goes with its events");
}

#[test]
fn a_single_approve_answers_only_the_cards_own_request() {
    let (mut h, _) = rig_with(seven(), vec2(420.0, 1200.0));
    click(&mut h, "Approve: Run render.preview (claude_code)");
    assert_eq!(h.state().fake.calls(), [Call::Resolve(1, Decision::Approve)]);
    h.run_steps(3);
    assert!(shows(&h, "5 similar requests"), "the rest of the batch stays: {:?}", all_strings(&h));
}

#[test]
fn a_reject_comment_reaches_resolve() {
    let (mut h, _) = rig_with(seven(), vec2(420.0, 1200.0));
    click(&mut h, "Reject: Run character.rename (studio_bot)");
    h.event(egui::Event::Text("Keep the old name".into()));
    h.run_steps(2);
    click(&mut h, "Reject and tell studio_bot");
    let comment = Some("Keep the old name".to_owned());
    assert_eq!(h.state().fake.calls(), [Call::Resolve(7, Decision::Reject { comment })]);
}

#[test]
fn no_timer_resolves_anything() {
    let (mut h, clock) = rig_with(seven(), vec2(420.0, 1200.0));
    for _ in 0..20 {
        clock.advance(Seconds(600.0));
        h.run_steps(5);
    }
    assert!(h.state().fake.calls().is_empty(), "two hours later nothing has been decided");
    assert_eq!(cards(&h).len(), 2);
}

#[test]
fn the_asking_sessions_row_reads_waiting_for_you() {
    let (mut h, _) = rig_with(seven(), vec2(420.0, 1400.0));
    assert!(shows(&h, "Waiting for you"));
    click(&mut h, "Approve all 6 similar requests");
    click(&mut h, "Approve: Run character.rename (studio_bot)");
    h.run_steps(3);
    assert!(!shows(&h, "Waiting for you"), "{:?}", all_strings(&h));
}

#[test]
fn each_need_offers_its_own_decisions() {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let mut http = session(2, "studio_bot", Transport::Http { token: "studio_token".into() });
    http.budget = Budget { usd_micros: 2_000_000, ..Budget::default() };
    let disclaimer = "Switching the model sends your prompts to a different provider.";
    let approvals = vec![
        approval(1, &stdio, "render.preview", scope(Scope::Render)),
        approval(2, &http, "character.rename", scope(Scope::EditCharacter)),
        approval(3, &stdio, "model.switch", Need::Person { reason: disclaimer.into() }),
        approval(
            4,
            &http,
            "connector.generate",
            Need::Budget {
                needed: Spend { usd_micros: 600_000, ..Spend::default() },
                left: Spend { usd_micros: 200_000, ..Spend::default() },
            },
        ),
    ];
    let (mut h, _) = rig_with(FakeAgents::new(vec![stdio, http], approvals), vec2(460.0, 2000.0));
    // A scope: Approve, for the session, and for the project only where there is a credential.
    let project = |h: &Harness<'_, Rig>, what: &str, agent: &str| {
        h.query_by_label(&format!("Approve for this project: {what} ({agent})")).is_some()
    };
    assert!(!project(&h, "Run render.preview", "claude_code"), "stdio has no credential");
    assert!(project(&h, "Run character.rename", "studio_bot"));
    // A person's confirmation: the reason in full, and no wider approval.
    assert!(shows(&h, disclaimer));
    assert!(h.query_by_label("Approve for this session: Run model.switch (claude_code)").is_none());
    // A budget: Approve once and Raise the budget (to what it needs past what is left).
    assert!(h.query_by_label("Approve once: Run connector.generate (studio_bot)").is_some());
    click(&mut h, "Raise the budget to $2.40: Run connector.generate (studio_bot)");
    click(&mut h, "Approve for this project: Run character.rename (studio_bot)");
    let raised = Budget { usd_micros: 2_400_000, ..Budget::default() };
    assert_eq!(
        h.state().fake.calls(),
        [Call::Resolve(4, Decision::RaiseBudget { budget: raised }), Call::Resolve(2, Decision::ApproveForProject)]
    );
    assert!(unlabelled_interactive(&h).is_empty(), "{:?}", unlabelled_interactive(&h));
}

#[test]
fn a_request_that_reaches_outside_the_project_takes_the_two_step_confirm() {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let approvals = vec![approval(1, &stdio, "stream.go_live", scope(Scope::Broadcast))];
    let (mut h, clock) = rig_with(FakeAgents::new(vec![stdio], approvals), vec2(420.0, 900.0));
    assert!(h.query_by_label("Approve for this session: Run stream.go_live (claude_code)").is_none());
    confirm(
        &mut h,
        &clock,
        "Approve\u{2026}: Run stream.go_live (claude_code)",
        "Confirm: Run stream.go_live (claude_code)",
    );
    assert_eq!(h.state().fake.calls(), [Call::Resolve(1, Decision::Approve)]);
}

#[test]
fn every_word_goes_through_the_message_table() {
    let (mut h, _) = rig_with(seven(), vec2(420.0, 1200.0));
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    let english = plain_english(all_strings(&h).iter().map(String::as_str));
    assert!(english.is_empty(), "plain English in the pseudo-locale: {english:?}");
}

#[test]
fn batches_keep_the_order_their_oldest_arrived_in() {
    let fake = seven();
    let approvals = omuzaki_ops_api::Sessions::approvals(&fake);
    let grouped = batches(&approvals);
    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].ids.len(), 6);
    assert_eq!(grouped[1].first.id, omuzaki_ops_api::ApprovalId(7));
}

// ---- the stack at the top right of the window ----

struct Window {
    shell: Shell,
}

fn window(fake: &Arc<FakeAgents>) -> Harness<'static, Window> {
    let mut shell = Shell::new(Arc::clone(fake) as Arc<dyn omuzaki_ops_api::Runner>, None);
    shell.insert_view(Agents(Arc::clone(fake) as Arc<dyn omuzaki_ops_api::Sessions>));
    let mut h = Harness::builder().with_size(vec2(1920.0, 1080.0)).with_pixels_per_point(1.0).build_ui_state(
        |ui, w: &mut Window| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
            w.shell.ui(ui);
        },
        Window { shell },
    );
    h.run_steps(4);
    h
}

/// Runs frames until `seen` holds (the shell hears the registry's events on a thread), at most 500.
fn until(h: &mut Harness<'_, Window>, what: &str, seen: impl Fn(&Harness<'_, Window>) -> bool) {
    for _ in 0..500 {
        h.step();
        if seen(h) {
            return;
        }
        std::thread::yield_now();
    }
    panic!("never: {what}");
}

#[test]
fn the_stack_shows_requests_while_the_agent_panel_is_not_on_screen_and_decides_them() {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let fake = Arc::new(FakeAgents::new(vec![stdio.clone()], Vec::new()));
    let mut h = window(&fake);
    // The first workspace, Perform, has no Agent panel: a request appears in the stack.
    fake.ask(approval(1, &stdio, "character.rename", scope(Scope::EditCharacter)));
    until(&mut h, "the stack shows the request", |h| {
        h.query_by_label("Approve: Run character.rename (claude_code)").is_some()
    });
    assert!(h.query_by_label_contains("Waiting for you: 1").is_some(), "the top-bar chip counts it");
    h.get_by_label("Approve: Run character.rename (claude_code)").click();
    until(&mut h, "the card goes once it is decided", |h| {
        h.query_by_label("Approve: Run character.rename (claude_code)").is_none()
    });
    assert_eq!(fake.calls(), [Call::Resolve(1, Decision::Approve)]);
}

#[test]
fn with_the_agent_panel_on_screen_the_request_shows_there_once() {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let fake = Arc::new(FakeAgents::new(vec![stdio.clone()], Vec::new()));
    let mut h = window(&fake);
    h.get_by_label("Build").click();
    h.run_steps(4);
    fake.ask(approval(1, &stdio, "character.rename", scope(Scope::EditCharacter)));
    until(&mut h, "the Agent panel shows the request", |h| {
        h.query_by_label("Approve: Run character.rename (claude_code)").is_some()
    });
    h.run_steps(3);
    assert_eq!(h.query_all_by_label("Approve: Run character.rename (claude_code)").count(), 1, "not twice");
}

#[test]
fn a_request_in_the_background_marks_the_window_title() {
    let stdio = session(1, "claude_code", Transport::Stdio);
    let fake = Arc::new(FakeAgents::new(vec![stdio.clone()], Vec::new()));
    let mut h = window(&fake);
    // The window is in the background.
    h.input_mut().focused = false;
    h.run_steps(2);
    fake.ask(approval(1, &stdio, "character.rename", scope(Scope::EditCharacter)));
    let titled = |h: &Harness<'_, Window>| {
        h.output()
            .viewport_output
            .values()
            .flat_map(|v| &v.commands)
            .any(|c| matches!(c, egui::ViewportCommand::Title(t) if t.contains("waiting for you")))
    };
    until(&mut h, "the title says a request waits", titled);
}
