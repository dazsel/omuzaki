//! The event envelope (#773): JSON round trips of the envelope and every payload with non-ASCII text, `of` and
//! `decode`, the payload size limit, the registered kinds against design D9's table, and role ranks.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::{
    ChannelRef, ChatMessage, ChatUser, Cheer, EmoteSpan, EngineTick, Follow, HypeTrain, Money, Phase, Poll, PowerUp,
    Prediction, Raid, Redeem, Reward, Role, ShowLifecycle, Sub, SubGift, Tip, VarChanged,
};
use omuzaki_live_api::event::{
    EventError, EventId, EventPayload, LiveEvent, PublishError, Source, UtcMillis, check_payload, event_kinds,
    find_event_kind,
};
use omuzaki_live_api::ids::{EventKind, GraphId, NodeId, Platform, ShowId, VarName};
use omuzaki_live_api::limits::MAX_PAYLOAD_BYTES;
use omuzaki_live_api::show::VarScope;
use omuzaki_ops_api::Actor;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Text that is often Japanese, Korean or emoji, never only ASCII by accident.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,16}",
        proptest::sample::select(vec!["ことり", "하늘", "Bonk #4 from Kiri!", "🎉✨🐱", "こんにちは、하늘 👋", ""])
            .prop_map(String::from),
    ]
}

fn platform() -> impl Strategy<Value = Platform> {
    proptest::sample::select(vec!["twitch", "youtube", "bilibili", "tiktok", "kick"])
        .prop_map(|p| Platform::new(p).unwrap())
}

fn channel() -> impl Strategy<Value = ChannelRef> {
    (text(), text()).prop_map(|(id, name)| ChannelRef::new(id, name))
}

fn role() -> impl Strategy<Value = Role> {
    proptest::sample::select(vec![Role::Broadcaster, Role::Moderator, Role::Vip, Role::Subscriber])
}

fn user() -> impl Strategy<Value = ChatUser> {
    (
        text(),
        text(),
        text(),
        proptest::collection::btree_set(role(), 0..3),
        proptest::collection::vec(text(), 0..2),
        proptest::option::of("#[0-9a-f]{6}"),
    )
        .prop_map(|(id, login, name, roles, badges, colour)| {
            let user = ChatUser::new(id, login, name).with_roles(roles).with_badges(badges);
            match colour {
                Some(colour) => user.with_colour(colour),
                None => user,
            }
        })
}

fn phase() -> impl Strategy<Value = Phase> {
    proptest::sample::select(vec![Phase::Begin, Phase::Progress, Phase::Lock, Phase::End])
}

fn chat_message() -> impl Strategy<Value = ChatMessage> {
    (
        platform(),
        channel(),
        text(),
        user(),
        text(),
        any::<bool>(),
        any::<bool>(),
        proptest::option::of(text()),
        (any::<u32>(), text()),
    )
        .prop_map(|(platform, channel, id, user, body, action, first, reply_to, (at, emote))| {
            let message = ChatMessage::new(platform, channel, id, user, body)
                .with_action(action)
                .with_first_message(first)
                .with_emotes(vec![EmoteSpan::new(at, at.saturating_add(4), "e1", emote)]);
            match reply_to {
                Some(parent) => message.with_reply_to(parent),
                None => message,
            }
        })
}

/// Serialises `value` and reads it back.
fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> T {
    serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
}

/// Wraps `payload` in an event and decodes it back.
fn through_event<P: EventPayload + PartialEq + std::fmt::Debug>(payload: &P) -> P {
    let event = LiveEvent::of(payload, Source::Test, Seconds(1.0)).unwrap();
    assert_eq!(event.kind.as_str(), P::KIND);
    round_trip(&event).decode::<P>().unwrap().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn chat_messages_round_trip(message in chat_message()) {
        prop_assert_eq!(round_trip(&message), message.clone());
        prop_assert_eq!(through_event(&message), message);
    }

    #[test]
    fn engagement_payloads_round_trip(
        (p, c, u, t) in (platform(), channel(), user(), text()),
        (gifter, months, count, bits, viewers, cost, micros) in (proptest::option::of(user()), any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>(), any::<i64>()),
        (input, ph) in (proptest::option::of(text()), phase()),
    ) {
        let follow = Follow::new(p.clone(), c.clone(), u.clone());
        prop_assert_eq!(through_event(&follow), follow);
        let mut sub = Sub::new(p.clone(), c.clone(), u.clone(), t.clone(), months).with_message(t.clone());
        if gifter.is_some() {
            sub = sub.with_gift(gifter.clone());
        }
        prop_assert_eq!(through_event(&sub), sub);
        let mut gift = SubGift::new(p.clone(), c.clone(), count, "1000");
        if let Some(g) = gifter.clone() {
            gift = gift.with_gifter(g);
        }
        prop_assert_eq!(through_event(&gift), gift);
        let cheer = Cheer::new(p.clone(), c.clone(), bits, t.clone()).with_user(u.clone());
        prop_assert_eq!(through_event(&cheer), cheer);
        let tip = Tip::new(p.clone(), c.clone(), u.clone(), Money::new(micros, "JPY")).with_message(t.clone());
        prop_assert_eq!(through_event(&tip), tip);
        let mut redeem = Redeem::new(p.clone(), c.clone(), u.clone(), Reward::new("r1", t.clone(), cost), "rd1");
        if let Some(i) = input {
            redeem = redeem.with_input(i);
        }
        prop_assert_eq!(through_event(&redeem), redeem);
        let raid = Raid::new(p.clone(), c.clone(), u.clone(), viewers);
        prop_assert_eq!(through_event(&raid), raid);
        let power_up = PowerUp::new(p.clone(), c.clone(), u, t, bits);
        prop_assert_eq!(through_event(&power_up), power_up);
        let poll = Poll::new(p.clone(), c.clone(), ph);
        prop_assert_eq!(through_event(&poll), poll);
        let prediction = Prediction::new(p.clone(), c.clone(), ph);
        prop_assert_eq!(through_event(&prediction), prediction);
        let train = HypeTrain::new(p, c, ph);
        prop_assert_eq!(through_event(&train), train);
    }

    #[test]
    fn engine_payloads_round_trip(id in "[a-z][a-z0-9]{0,8}", old in text(), new in text()) {
        let lifecycle = ShowLifecycle::new(ShowId::new(id.as_str()).unwrap());
        prop_assert_eq!(through_event(&lifecycle), lifecycle);
        let changed = VarChanged::new(VarName::new(id.as_str()).unwrap(), VarScope::Show, json!(old), json!(new));
        prop_assert_eq!(through_event(&changed), changed);
        let tick = EngineTick::new(GraphId::new(id.as_str()).unwrap(), NodeId::new(id.as_str()).unwrap());
        prop_assert_eq!(through_event(&tick), tick);
    }

    #[test]
    fn the_envelope_round_trips(
        message in chat_message(),
        (id, at, utc, hops) in (any::<u64>(), -1.0e9..1.0e9_f64, proptest::option::of(any::<i64>()), any::<u8>()),
        (origin, name, source) in (proptest::option::of(text()), text(), 0..9_usize),
    ) {
        let actor = Actor::Person { name: name.clone() };
        let source = match source {
            0 => Source::platform(message.platform.clone(), name.clone()),
            1 => Source::Input,
            2 => Source::Voice,
            3 => Source::Obs,
            4 => Source::Engine,
            5 => Source::graph(GraphId::new("g").unwrap(), NodeId::new("n").unwrap()),
            6 => Source::manual(actor.clone()),
            7 => Source::plugin(name),
            _ => Source::Test,
        };
        let mut event = LiveEvent::of(&message, source, Seconds(at)).unwrap().with_hops(hops).with_acting(actor);
        event.id = EventId(id);
        if let Some(origin) = origin {
            event = event.with_origin_id(origin);
        }
        if let Some(utc) = utc {
            event = event.with_utc(UtcMillis(utc));
        }
        prop_assert_eq!(round_trip(&event), event);
    }
}

fn kiri() -> ChatUser {
    ChatUser::new("u1", "kiri", "Kiri")
}

fn twitch() -> Platform {
    Platform::new("twitch").unwrap()
}

#[test]
fn decode_gives_back_the_payload_only_for_its_kind() {
    let message = ChatMessage::new(twitch(), ChannelRef::new("c1", "Studio"), "m1", kiri(), "hi");
    let event = LiveEvent::of(&message, Source::Test, Seconds(0.0)).unwrap();
    assert_eq!(event.decode::<ChatMessage>(), Some(Ok(message)));
    assert!(event.decode::<Follow>().is_none());

    let stopping = LiveEvent::of_kind(
        EventKind::from_static(ShowLifecycle::STOPPING),
        &ShowLifecycle::new(ShowId::new("main").unwrap()),
        Source::Engine,
        Seconds(0.0),
    )
    .unwrap();
    assert_eq!(stopping.decode::<ShowLifecycle>().unwrap().unwrap().show.as_str(), "main");
    let wrong = LiveEvent::of_kind(EventKind::from_static("redeem"), &kiri_follow(), Source::Test, Seconds(0.0));
    assert!(matches!(wrong, Err(EventError::WrongKind { .. })));

    let mut broken = event.clone();
    broken.payload = json!({"text": "no user"});
    assert!(matches!(broken.decode::<ChatMessage>(), Some(Err(EventError::Decode { .. }))));
}

fn kiri_follow() -> Follow {
    Follow::new(twitch(), ChannelRef::new("c1", "Studio"), kiri())
}

#[test]
fn a_payload_over_the_limit_is_refused() {
    let long = "x".repeat(70 * 1024);
    assert!(long.len() > MAX_PAYLOAD_BYTES);
    let message = ChatMessage::new(twitch(), ChannelRef::new("c1", "Studio"), "m1", kiri(), long.clone());
    assert!(matches!(LiveEvent::of(&message, Source::Test, Seconds(0.0)), Err(EventError::TooLarge { .. })));
    let kind = EventKind::from_static("custom.big");
    assert_eq!(check_payload(&kind, &json!({ "text": long })), Err(PublishError::TooLarge));
    assert_eq!(check_payload(&kind, &json!({ "text": "small" })), Ok(()));
    assert!(matches!(check_payload(&kind, &json!("not an object")), Err(PublishError::BadPayload { .. })));
    assert_eq!(check_payload(&EventKind::from_static("nobody.registered"), &json!({})), Err(PublishError::UnknownKind));
}

#[test]
fn the_registered_kinds_are_the_designs_table() {
    let kinds: Vec<&str> = event_kinds().map(|r| r.kind).collect();
    let unique: BTreeSet<&str> = kinds.iter().copied().collect();
    assert_eq!(kinds.len(), unique.len(), "each kind registers once: {kinds:?}");
    let table: BTreeSet<&str> = [
        "chat.message",
        "follow",
        "sub",
        "sub_gift",
        "cheer",
        "tip",
        "redeem",
        "raid",
        "power_up",
        "poll",
        "prediction",
        "hype_train",
        "show.started",
        "show.stopping",
        "var.changed",
        "engine.tick",
    ]
    .into_iter()
    .collect();
    assert_eq!(unique, table);
    let droppable: Vec<&str> = event_kinds().filter(|r| r.droppable).map(|r| r.kind).collect();
    assert_eq!(droppable, ["chat.message"]);
    assert!(find_event_kind("hotkey").is_none() && find_event_kind("voice.command").is_none());
}

/// A valid payload of every kind, as JSON.
fn valid_payloads() -> Vec<(&'static str, Value)> {
    let (p, c, u) = (twitch(), ChannelRef::new("c1", "Studio"), kiri());
    let show = ShowLifecycle::new(ShowId::new("main").unwrap());
    let value = |v: &dyn erased::Payload| v.json();
    vec![
        ("chat.message", value(&ChatMessage::new(p.clone(), c.clone(), "m1", u.clone(), "hi"))),
        ("follow", value(&Follow::new(p.clone(), c.clone(), u.clone()))),
        ("sub", value(&Sub::new(p.clone(), c.clone(), u.clone(), "1000", 3))),
        ("sub_gift", value(&SubGift::new(p.clone(), c.clone(), 5, "1000"))),
        ("cheer", value(&Cheer::new(p.clone(), c.clone(), 100, "Cheer100"))),
        ("tip", value(&Tip::new(p.clone(), c.clone(), u.clone(), Money::new(5_000_000, "USD")))),
        ("redeem", value(&Redeem::new(p.clone(), c.clone(), u.clone(), Reward::new("r1", "Bonk", 100), "rd1"))),
        ("raid", value(&Raid::new(p.clone(), c.clone(), u.clone(), 42))),
        ("power_up", value(&PowerUp::new(p.clone(), c.clone(), u, "gigantify", 50))),
        ("poll", value(&Poll::new(p.clone(), c.clone(), Phase::Begin))),
        ("prediction", value(&Prediction::new(p.clone(), c.clone(), Phase::Lock))),
        ("hype_train", value(&HypeTrain::new(p, c, Phase::End))),
        ("show.started", value(&show)),
        ("show.stopping", value(&show)),
        ("var.changed", value(&VarChanged::new(VarName::new("bonks").unwrap(), VarScope::Show, json!(3), json!(4)))),
        ("engine.tick", value(&EngineTick::new(GraphId::new("g").unwrap(), NodeId::new("n").unwrap()))),
    ]
}

/// Lets the table above hold payloads of different types.
mod erased {
    pub trait Payload {
        fn json(&self) -> serde_json::Value;
    }

    impl<T: serde::Serialize> Payload for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap()
        }
    }
}

#[test]
fn every_kind_accepts_its_payload_and_refuses_one_missing_a_field() {
    let payloads = valid_payloads();
    assert_eq!(payloads.len(), event_kinds().count());
    for (kind, payload) in payloads {
        let registration = find_event_kind(kind).unwrap();
        assert_eq!((registration.validate)(&payload), Ok(()), "{kind}");
        assert_eq!(check_payload(&EventKind::new(kind).unwrap(), &payload), Ok(()), "{kind}");
        let schema = serde_json::to_value((registration.schema)()).unwrap();
        let required = schema["required"].as_array().unwrap();
        let field = required[0].as_str().unwrap();
        let mut missing = payload.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!((registration.validate)(&missing).is_err(), "{kind} without {field}");
        assert!(matches!(
            check_payload(&EventKind::new(kind).unwrap(), &missing),
            Err(PublishError::BadPayload { .. })
        ));
    }
}

#[test]
fn each_summary_is_the_first_line_of_its_types_doc() {
    for registration in event_kinds() {
        let schema = serde_json::to_value((registration.schema)()).unwrap();
        let description = schema["description"].as_str().unwrap_or_default();
        assert_eq!(description.lines().next(), Some(registration.summary), "{}", registration.kind);
        assert!(!serde_json::to_string(&schema).unwrap().contains("$ref"), "{} is inlined", registration.kind);
    }
}

#[test]
fn a_users_rank_follows_their_highest_role() {
    let with = |roles: &[Role]| ChatUser::new("u", "u", "U").with_roles(roles.iter().copied()).rank();
    let order = [
        with(&[Role::Broadcaster]),
        with(&[Role::Moderator]),
        with(&[Role::Vip]),
        with(&[Role::Subscriber]),
        with(&[]),
    ];
    assert!(order.windows(2).all(|pair| pair[0] > pair[1]), "{order:?}");
    assert_eq!(with(&[Role::Moderator, Role::Subscriber]), with(&[Role::Moderator]));
    assert_eq!(with(&[]), 0);
}
