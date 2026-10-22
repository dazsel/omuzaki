//! Who holds what (#969, `agent-sessions.md` section 5.3): the fence board over the live sessions, the status bar's
//! chip, the inspector's note on a held part and the outliner's marker. A fence binds agents only: the note never
//! blocks the person's edit.

mod fake_agents;

use std::sync::mpsc::Receiver;

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use fake_agents::{fenced, session};
use omuzaki_char_api::{CharId, PartId};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, EndReason, Ended, Event, Grants, Runner as _, SessionInfo, SessionState, Transport};
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::panels::agent::AgentFeed;
use omuzaki_ui::panels::agent::fences::{FenceBoard, fence_chip};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::inspector::Inspector;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::outliner::Outliner;
use serde_json::json;

fn path(text: &str) -> DocPath {
    DocPath::parse(text).unwrap_or_else(|e| panic!("{e}"))
}

/// Two fenced sessions (a designer on the whole character, a sub-agent on its hair inside that claim) and one that
/// ended while it held the body.
fn three() -> Vec<SessionInfo> {
    let designer = fenced(session(1, "claude_code", Transport::Stdio), &["characters/kit"]);
    let mut hair = fenced(session(2, "hair_bot", Transport::Stdio), &["characters/kit/parts/hair"]);
    hair.role = Some("hair".into());
    hair.parent = Some(designer.id);
    let mut gone = fenced(session(3, "rigger_bot", Transport::Stdio), &["characters/kit/parts/body"]);
    gone.state = SessionState::Ended { reason: EndReason::Closed, at: Seconds(1.0) };
    vec![designer, hair, gone]
}

fn feed(sessions: &[SessionInfo]) -> AgentFeed {
    let mut feed = AgentFeed::default();
    let events: Vec<Event> = sessions.iter().cloned().map(Event::session_opened).collect();
    feed.follow(&events);
    feed
}

#[test]
fn the_board_holds_live_fenced_sessions_and_the_deepest_claim_wins() {
    let board = FenceBoard::from_sessions(&three());
    let names: Vec<&str> = board.holders.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["claude_code", "hair_bot"], "the ended session holds nothing");
    let holder = |p: &str| board.holder_of(&path(p)).map(|h| h.name.clone());
    assert_eq!(holder("characters/kit/parts/hair/params/length").as_deref(), Some("hair_bot"));
    assert_eq!(holder("characters/kit/parts/head").as_deref(), Some("claude_code"));
    assert_eq!(holder("characters/kitten/parts/head"), None, "paths compare by segment");
    assert_eq!(holder("scenes/main"), None);
}

#[test]
fn the_chip_names_live_holders_and_an_end_removes_one_on_its_event() {
    let mut feed = feed(&three());
    let mut h = harness_state(Settings::default(), vec2(900.0, 60.0), (feed.clone(), false), |ui, s| {
        let board = FenceBoard::from_feed(&s.0);
        s.1 |= fence_chip(ui, &board).clicked();
    });
    h.run_steps(2);
    assert!(
        all_strings(&h).iter().any(|s| s == "claude_code holds kit \u{b7} hair_bot holds hair"),
        "{:?}",
        all_strings(&h)
    );
    h.get_by_role(Role::Button).click();
    h.run_steps(2);
    assert!(h.state().1, "the chip answers a click (the shell opens the Agent panel)");

    let mut ended = three()[1].clone();
    ended.state = SessionState::Ended { reason: EndReason::Disconnected, at: Seconds(2.0) };
    let gone = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() };
    feed.follow(&[Event::session_ended(ended, gone)]);
    h.state_mut().0 = feed;
    h.run_steps(2);
    assert!(all_strings(&h).iter().any(|s| s == "claude_code holds kit"), "{:?}", all_strings(&h));
}

#[test]
fn the_chip_counts_holders_past_two() {
    let mut sessions = three();
    sessions.push(fenced(session(4, "light_bot", Transport::Stdio), &["scenes/main"]));
    let feed = feed(&sessions);
    let h = harness_state(Settings::default(), vec2(900.0, 60.0), feed, |ui, f| {
        let _ = fence_chip(ui, &FenceBoard::from_feed(f));
    });
    assert!(all_strings(&h).iter().any(|s| s.ends_with("\u{b7} 1 more")), "{:?}", all_strings(&h));
}

// ---- the inspector's note and the outliner's marker, over the real part ops ----

fn person() -> Actor {
    Actor::Person { name: "kit".into() }
}

fn runner() -> MockRunner<MemDoc> {
    omuzaki_char::link();
    omuzaki_char_mock::link();
    let reg = omuzaki_char_api::find_kind("mock-head").unwrap_or_else(|| panic!("mock-head is registered"));
    let head = json!({"kind": "mock-head", "params": (reg.defaults)()});
    let node = json!({"name": "Kit", "template": "mock", "parts": {"hair": head.clone(), "head": head}});
    MockRunner::new(MemDoc::from_value(json!({"characters": {"kit": node}})))
}

struct Edit {
    inspector: Inspector,
    outliner: Outliner,
    runner: MockRunner<MemDoc>,
    events: Receiver<Event>,
    frame: Vec<Event>,
    feed: AgentFeed,
}

fn edit(select: &str) -> Harness<'static, Edit> {
    let focus = EditFocus::new(CharId::new("kit").unwrap_or_else(|_| panic!("an id")));
    focus.set(vec![PartId::new(select).unwrap_or_else(|_| panic!("a part id"))]);
    let runner = runner();
    let events = runner.subscribe(&Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let state = Edit {
        inspector: Inspector::new(focus.clone()),
        outliner: Outliner::new(focus.clone()),

        runner,
        events,
        frame: Vec::new(),
        feed: feed(&three()),
    };
    let mut h = harness_state(Settings::default(), vec2(420.0, 1400.0), state, |ui, s| {
        s.frame = s.events.try_iter().collect();
        let actor = person();
        s.outliner.follow_fences(Some(&s.feed));
        ui.push_id("outliner", |ui| {
            ui.set_max_height(300.0);
            s.outliner.ui(ui, Io::new(&s.runner, &actor, &s.frame));
        });
        s.inspector.fence_note(ui, Some(&s.feed));
        s.inspector.ui(ui, Io::new(&s.runner, &actor, &s.frame));
    });
    h.run_steps(4);
    h
}

const NOTE: &str = "hair_bot is working on this part. Your edit takes precedence and it will be told.";

fn note_shown(h: &Harness<'_, Edit>) -> bool {
    all_strings(h).iter().any(|s| s == NOTE)
}

#[test]
fn a_held_part_gets_the_note_and_its_edit_still_applies() {
    let mut h = edit("hair");
    assert!(note_shown(&h), "{:?}", all_strings(&h));
    // The person is never blocked: an edit made while the note shows goes through.
    let before = h.state().runner.records().len();
    let slider = h.query_all_by_role(Role::Slider).next().unwrap_or_else(|| panic!("the form has a slider"));
    slider.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    h.run_steps(3);
    let after = h.state().runner.records();
    assert!(after.len() > before, "the edit applied");
    assert!(after.iter().any(|r| r.op == "part.set_param" && r.actor == person()));
}

#[test]
fn the_note_is_dismissible_and_returns_for_a_different_session() {
    let mut h = edit("hair");
    h.get_by_label("Dismiss").click();
    h.run_steps(3);
    assert!(!note_shown(&h), "dismissed for this session");
    // Another session takes over the hair: the note returns, naming it.
    let mut feed = feed(&three());
    let mut ended = three()[1].clone();
    ended.state = SessionState::Ended { reason: EndReason::Closed, at: Seconds(3.0) };
    let gone = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() };
    let next = fenced(session(5, "groom_bot", Transport::Stdio), &["characters/kit/parts/hair"]);
    feed.follow(&[Event::session_ended(ended, gone), Event::session_opened(next)]);
    h.state_mut().feed = feed;
    h.run_steps(3);
    let returned = "groom_bot is working on this part. Your edit takes precedence and it will be told.";
    assert!(all_strings(&h).iter().any(|s| s == returned), "{:?}", all_strings(&h));
}

#[test]
fn a_free_part_gets_no_note() {
    let mut h = edit("head");
    // The designer's claim covers the whole character, so free its head by ending the designer.
    let mut feed = feed(&three());
    let mut designer = three()[0].clone();
    designer.state = SessionState::Ended { reason: EndReason::Closed, at: Seconds(3.0) };
    let gone = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() };
    feed.follow(&[Event::session_ended(designer, gone)]);
    h.state_mut().feed = feed;
    h.run_steps(3);
    assert!(!all_strings(&h).iter().any(|s| s.contains("is working on this part")), "{:?}", all_strings(&h));
}

#[test]
fn the_outliner_marks_held_parts_with_their_holder() {
    let h = edit("head");
    let held = h.state().outliner.held().clone();
    let named: Vec<(String, String)> = held.into_iter().map(|(p, n)| (p.to_string(), n)).collect();
    assert_eq!(named, [("hair".into(), "hair_bot".into()), ("head".into(), "claude_code".into())]);
    let described = h
        .query_all_by_role(Role::TreeItem)
        .filter_map(|n| n.accesskit_node().description())
        .collect::<Vec<_>>();
    assert!(described.iter().any(|d| d.contains("Locked (hair_bot)")), "{described:?}");
}
