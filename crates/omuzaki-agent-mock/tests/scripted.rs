//! Acceptance tests for the scripted assistant and conversations (issue #1109).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_agent_api::Assistant;
use omuzaki_agent_api::conversations::{ConversationId, ConversationInfo, Conversations, Resumed};
use omuzaki_agent_api::entry::Entry;
use omuzaki_agent_api::error::HostError;
use omuzaki_agent_api::event::{AssistantEvent, Cap, PlanItem, PlanStatus, QuestionId};
use omuzaki_agent_api::message::UserMessage;
use omuzaki_agent_api::prompts::{PromptInfo, PromptsState};
use omuzaki_agent_mock::{ScriptedAnswer, ScriptedAssistant, ScriptedConversations, TimedEvent};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{ApprovalId, Budget, Spend};

fn delta(t: &str) -> AssistantEvent {
    AssistantEvent::TextDelta(t.into())
}

fn at(s: f64, event: AssistantEvent) -> TimedEvent {
    TimedEvent::new(Seconds(s), event)
}

fn drain(a: &ScriptedAssistant, rx: &omuzaki_agent_api::hub::EventReceiver) -> Vec<AssistantEvent> {
    a.tick();
    std::iter::from_fn(|| rx.try_recv()).collect()
}

fn d10_events() -> Vec<AssistantEvent> {
    vec![
        delta("Let me look. "),
        AssistantEvent::tool_call("c1", "face_profile", "Face profile: egg-oval, 50 %"),
        AssistantEvent::tool_result("c1", true),
        AssistantEvent::tool_call("c2", "render", "Render"),
        AssistantEvent::tool_result("c2", false),
        AssistantEvent::waiting(ApprovalId(7)),
        AssistantEvent::Paused,
        AssistantEvent::Resumed,
        AssistantEvent::question(QuestionId(1), "Which?", vec!["a".into(), "b".into()]),
        AssistantEvent::Plan(vec![PlanItem::new("step", PlanStatus::Doing)]),
        AssistantEvent::Limit(Cap::Turns),
        AssistantEvent::budget(Spend::default(), Budget::default()),
        AssistantEvent::Error(HostError::NoModel),
        AssistantEvent::Done,
    ]
}

fn setup(answers: Vec<ScriptedAnswer>) -> (Arc<FakeClock>, ScriptedAssistant) {
    let clock = Arc::new(FakeClock::new(Seconds(100.0)));
    let a = ScriptedAssistant::new(answers, clock.clone());
    (clock, a)
}

#[test]
fn the_full_d10_script_plays_in_order_as_the_clock_advances() {
    let events = d10_events();
    let timed: Vec<_> = events.iter().zip(0_u32..).map(|(e, i)| at(f64::from(i), e.clone())).collect();
    let (clock, a) = setup(vec![ScriptedAnswer::new(timed)]);
    let rx = a.events();
    a.send(UserMessage::new("go")).unwrap();
    assert!(drain(&a, &rx).len() == 1, "only the event due at 0 s plays at once");
    for expected in &events[1..] {
        assert_eq!(drain(&a, &rx), Vec::<AssistantEvent>::new(), "nothing before its time");
        clock.advance(Seconds(1.0));
        assert_eq!(drain(&a, &rx), vec![expected.clone()]);
    }
    assert_eq!(a.send(UserMessage::new("again")), Ok(()), "Done delivered, so idle again");
}

#[test]
fn a_late_tick_delivers_everything_due_in_order() {
    let (clock, a) =
        setup(vec![ScriptedAnswer::new(vec![at(0.0, delta("a")), at(1.0, delta("b")), at(2.0, AssistantEvent::Done)])]);
    let rx = a.events();
    a.send(UserMessage::new("x")).unwrap();
    clock.advance(Seconds(10.0));
    assert_eq!(drain(&a, &rx), vec![delta("a"), delta("b"), AssistantEvent::Done]);
}

#[test]
fn stop_mid_script_withdraws_unread_text_then_paused_then_done() {
    let script = vec![
        at(0.0, delta("1")),
        at(0.0, delta("2")),
        at(0.0, delta("3")),
        at(5.0, delta("4")),
        at(6.0, AssistantEvent::Done),
    ];
    let (clock, a) = setup(vec![ScriptedAnswer::new(script)]);
    let rx = a.events();
    a.send(UserMessage::new("x")).unwrap();
    a.tick();
    assert_eq!(rx.try_recv(), Some(delta("1")));
    a.stop();
    assert_eq!(a.stop_count(), 1);
    assert_eq!(rx.try_recv(), Some(AssistantEvent::Paused));
    assert_eq!(rx.try_recv(), Some(AssistantEvent::Done));
    assert_eq!(rx.try_recv(), None);
    clock.advance(Seconds(60.0));
    assert_eq!(drain(&a, &rx), Vec::<AssistantEvent>::new(), "dropped events never play");
}

#[test]
fn a_second_stop_changes_nothing_but_counts() {
    let (_clock, a) = setup(vec![ScriptedAnswer::new(vec![at(0.0, delta("1")), at(9.0, AssistantEvent::Done)])]);
    let rx = a.events();
    a.send(UserMessage::new("x")).unwrap();
    a.tick();
    a.stop();
    a.stop();
    assert_eq!(a.stop_count(), 2);
    let all: Vec<_> = std::iter::from_fn(|| rx.try_recv()).collect();
    assert_eq!(all, vec![AssistantEvent::Paused, AssistantEvent::Done]);
}

#[test]
fn send_is_busy_until_done_then_starts_the_next_answer() {
    let (clock, a) = setup(vec![
        ScriptedAnswer::new(vec![at(1.0, delta("one")), at(2.0, AssistantEvent::Done)]),
        ScriptedAnswer::new(vec![
            at(0.0, delta("two")),
            at(0.0, AssistantEvent::question(QuestionId(1), "Which?", Vec::new())),
            at(0.0, AssistantEvent::question(QuestionId(2), "And?", Vec::new())),
            at(1.0, AssistantEvent::Done),
        ]),
    ]);
    let rx = a.events();
    a.send(UserMessage::new("first")).unwrap();
    assert_eq!(a.send(UserMessage::new("rejected")), Err(HostError::Busy));
    clock.advance(Seconds(1.0));
    drain(&a, &rx);
    assert_eq!(a.send(UserMessage::new("rejected")), Err(HostError::Busy), "Done not yet delivered");
    clock.advance(Seconds(1.0));
    assert_eq!(drain(&a, &rx), vec![AssistantEvent::Done]);
    a.send(UserMessage::new("second")).unwrap();
    let second = drain(&a, &rx);
    assert_eq!(second.len(), 3, "the text and the two questions; Done is a second away");
    assert_eq!(a.answer(QuestionId(9), "never asked".into()), Err(HostError::Mcp("no open question 9".into())));
    a.answer(QuestionId(1), "yes".into()).unwrap();
    a.answer(QuestionId(2), "no".into()).unwrap();
    assert_eq!(
        a.answer(QuestionId(1), "again".into()),
        Err(HostError::Mcp("no open question 1".into())),
        "answered once"
    );
    assert_eq!(a.answered(), vec![(QuestionId(1), "yes".to_string()), (QuestionId(2), "no".to_string())]);
    clock.advance(Seconds(1.0));
    assert_eq!(drain(&a, &rx), vec![AssistantEvent::Done]);
    let texts: Vec<_> = a.sent().into_iter().map(|m| m.text).collect();
    assert_eq!(texts, ["first", "second"]);
}

#[test]
fn a_send_after_the_last_answer_is_recorded_and_plays_nothing() {
    let (_clock, a) = setup(Vec::new());
    let rx = a.events();
    a.send(UserMessage::new("hi")).unwrap();
    a.send(UserMessage::new("again")).unwrap();
    assert_eq!(a.sent().len(), 2);
    assert_eq!(drain(&a, &rx), Vec::<AssistantEvent>::new());
}

#[test]
fn stop_delay_holds_done_back_until_the_clock_passes_it() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let a = ScriptedAssistant::new(vec![ScriptedAnswer::new(vec![at(0.0, delta("x"))])], clock.clone())
        .with_stop_delay(Seconds(5.0));
    let rx = a.events();
    a.send(UserMessage::new("go")).unwrap();
    a.stop();
    assert_eq!(rx.try_recv(), Some(AssistantEvent::Paused));
    assert_eq!(rx.try_recv(), None);
    assert_eq!(a.send(UserMessage::new("busy")), Err(HostError::Busy));
    clock.advance(Seconds(4.9));
    assert_eq!(drain(&a, &rx), Vec::<AssistantEvent>::new());
    clock.advance(Seconds(0.1));
    assert_eq!(drain(&a, &rx), vec![AssistantEvent::Done]);
    assert_eq!(a.send(UserMessage::new("now ok")), Ok(()));
}

#[test]
fn from_json_round_trips_a_serialized_script() {
    let answers = vec![ScriptedAnswer::new(
        d10_events().into_iter().zip(0_u32..).map(|(e, i)| at(f64::from(i) * 0.5, e)).collect(),
    )];
    let json = serde_json::to_string(&answers).unwrap();
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let a = ScriptedAssistant::from_json(&json, clock.clone()).unwrap();
    let rx = a.events();
    a.send(UserMessage::new("go")).unwrap();
    clock.advance(Seconds(100.0));
    let expected = d10_events();
    assert_eq!(drain(&a, &rx), expected);
    assert!(ScriptedAssistant::from_json("{\"events\": []}", clock).is_err(), "a bare answer is not an array");
}

#[test]
fn the_transcript_is_what_was_given() {
    let (_c, a) = setup(Vec::new());
    assert_eq!(a.transcript().len(), 0);
    let entries = vec![Entry::user("hi", Vec::new()), Entry::stopped(None)];
    let a = a.with_transcript(entries.clone());
    assert_eq!(a.transcript(), entries);
}

#[test]
fn prompts_come_sorted_from_the_table_and_prompt_calls_are_recorded() {
    let (_c, a) = setup(Vec::new());
    let a = a
        .with_prompts(vec![PromptInfo::new("zeta"), PromptInfo::new("alpha")])
        .with_prompt_text("alpha", "Set up a stream");
    let PromptsState::Ready(list) = a.prompts() else { panic!("not ready") };
    assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["alpha", "zeta"]);
    let args = BTreeMap::from([("character".to_string(), "main".to_string())]);
    assert_eq!(a.prompt("alpha", args.clone()).unwrap(), "Set up a stream");
    assert!(matches!(a.prompt("nope", BTreeMap::new()), Err(HostError::Mcp(_))));
    assert_eq!(a.prompt_calls(), vec![("alpha".to_string(), args), ("nope".to_string(), BTreeMap::new())]);
    assert!(a.sent().is_empty(), "a pick sends nothing");
}

#[test]
fn failed_prompts_recover_through_refresh_after_one_loading() {
    let (_c, a) = setup(Vec::new());
    let a = a
        .with_prompts(vec![PromptInfo::new("p")])
        .with_prompt_text("p", "t")
        .fail_prompts(HostError::Mcp("down".into()));
    assert_eq!(a.prompts(), PromptsState::Failed(HostError::Mcp("down".into())));
    assert_eq!(a.prompt("p", BTreeMap::new()), Err(HostError::Mcp("down".into())));
    a.refresh_prompts();
    assert_eq!(a.refresh_count(), 1);
    assert_eq!(a.prompts(), PromptsState::Loading);
    assert_eq!(a.prompts(), PromptsState::Ready(vec![PromptInfo::new("p")]));
    assert_eq!(a.prompt("p", BTreeMap::new()).unwrap(), "t");
}

fn convo(id: u64, last: &str) -> (ConversationInfo, Resumed) {
    let info =
        ConversationInfo::new(ConversationId(id), "2026-10-01T00:00:00Z", last, "m", format!("first {id}"), false);
    (info, Resumed::new(vec![Entry::user(format!("hi {id}"), Vec::new())], id == 2))
}

#[test]
fn conversations_list_open_delete_export() {
    let store = ScriptedConversations::new(vec![
        convo(1, "2026-10-02T00:00:00Z"),
        convo(2, "2026-10-04T00:00:00Z"),
        convo(3, "2026-10-03T00:00:00Z"),
    ]);
    let ids: Vec<_> = store.list().unwrap().iter().map(|i| i.id.0).collect();
    assert_eq!(ids, [2, 3, 1]);
    assert_eq!(store.open(ConversationId(2)).unwrap(), convo(2, "").1);

    store.export(ConversationId(3), Path::new("out/three")).unwrap();
    assert_eq!(store.exported(), vec![(ConversationId(3), PathBuf::from("out/three"))]);

    store.delete(ConversationId(3)).unwrap();
    assert_eq!(store.deleted(), vec![ConversationId(3)]);
    assert!(matches!(store.open(ConversationId(3)), Err(HostError::Store(_))));
    assert!(matches!(store.delete(ConversationId(3)), Err(HostError::Store(_))));
    assert!(matches!(store.export(ConversationId(9), Path::new("x")), Err(HostError::Store(_))));
    assert_eq!(store.exported().len(), 1, "a refused export is not recorded");

    store.delete_all().unwrap();
    assert_eq!(store.list().unwrap().len(), 0);
    assert_eq!(store.deleted().len(), 3);
}
