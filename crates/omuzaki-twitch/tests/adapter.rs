//! The adapter against a fake Twitch: an `EventSub` socket on a local port and a scripted Helix. The token check and
//! the subscriptions, a redeem reaching the show, a reconnect that keeps the subscriptions, a dropped connection that
//! starts over, a full inbox that keeps a redeem and sheds chat, a refused token, and the ops. The token never shows
//! up in anything the adapter reports.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_http_api::{HttpError, HttpResponse, HttpTransport, Method};
use omuzaki_http_mock::{FakeHttp, Reply};
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError};
use omuzaki_ops_api::{Actor, OpCtx, Services, ServicesBuilder, find};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_twitch::eventsub::{TOPICS, normalise};
use omuzaki_twitch::helix::Helix;
use omuzaki_twitch::{Endpoints, TwitchService, TwitchStatus};
use serde_json::{Value, json};
use tungstenite::{Message, Utf8Bytes, WebSocket};

const TOKEN: &str = "test-only-token-0123456789";
const API: &str = "http://helix.test";
const OAUTH: &str = "http://oauth.test";
/// How long a test waits for something that should happen at once.
const WAIT: Duration = Duration::from_secs(10);

/// What the show received, and how many publishes it refused as `Full` before taking any.
struct Sink {
    sent: Mutex<Sender<LiveEvent>>,
    refuse: Mutex<usize>,
    dropped: AtomicUsize,
}

impl Sink {
    fn new(refuse: usize) -> (Arc<Self>, Receiver<LiveEvent>) {
        let (tx, rx) = channel();
        let sink = Self { sent: Mutex::new(tx), refuse: Mutex::new(refuse), dropped: AtomicUsize::new(0) };
        (Arc::new(sink), rx)
    }
}

impl EventSink for Sink {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        {
            let mut refuse = self.refuse.lock().unwrap();
            if *refuse > 0 {
                *refuse -= 1;
                return Err(PublishError::Full);
            }
        }
        let _ = self.sent.lock().unwrap().send(event);
        Ok(())
    }

    fn producer_dropped(&self, count: u64) {
        self.dropped.fetch_add(usize::try_from(count).unwrap(), Ordering::SeqCst);
    }
}

fn message(kind: &str, id: &str, payload: &Value) -> Value {
    json!({ "metadata": { "message_id": id, "message_type": kind, "message_timestamp": "2026-10-22T00:00:00Z" },
            "payload": payload })
}

fn welcome(session: &str) -> Value {
    message(
        "session_welcome",
        &format!("welcome-{session}"),
        &json!({ "session": { "id": session, "status": "connected", "keepalive_timeout_seconds": 10 } }),
    )
}

fn notification(id: &str, kind: &str, event: &Value) -> Value {
    let mut m = message("notification", id, &json!({ "subscription": { "type": kind }, "event": event }));
    m["metadata"]["subscription_type"] = json!(kind);
    m
}

fn redeem(id: &str, title: &str) -> Value {
    notification(
        id,
        "channel.channel_points_custom_reward_redemption.add",
        &json!({
            "id": format!("redemption-{id}"),
            "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo", "broadcaster_user_name": "KitDemo",
            "user_id": "2002", "user_login": "viewer", "user_name": "Viewer",
            "user_input": "",
            "reward": { "id": "r1", "title": title, "cost": 100 },
        }),
    )
}

fn follow(id: &str) -> Value {
    notification(
        id,
        "channel.follow",
        &json!({
            "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo", "broadcaster_user_name": "KitDemo",
            "user_id": "3003", "user_login": "newfan", "user_name": "NewFan",
        }),
    )
}

fn chat(id: &str, text: &str) -> Value {
    notification(
        id,
        "channel.chat.message",
        &json!({
            "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo", "broadcaster_user_name": "KitDemo",
            "chatter_user_id": "2002", "chatter_user_login": "viewer", "chatter_user_name": "Viewer",
            "message_id": format!("chat-{id}"),
            "message": { "text": text, "fragments": [{ "type": "text", "text": text }] },
            "badges": [{ "set_id": "moderator", "id": "1" }, { "set_id": "subscriber", "id": "12" }],
            "color": "#FF0000",
            "message_type": "user_intro",
        }),
    )
}

fn send(ws: &mut WebSocket<TcpStream>, m: &Value) {
    ws.send(Message::Text(Utf8Bytes::from(m.to_string()))).unwrap();
}

/// Reads until the client closes or goes away, so the connection stays open while the test runs.
fn hold(ws: &mut WebSocket<TcpStream>) {
    while let Ok(m) = ws.read() {
        if matches!(m, Message::Close(_)) {
            break;
        }
    }
}

/// What one accepted connection does.
type Script = Box<dyn FnOnce(&mut WebSocket<TcpStream>) + Send>;

/// A fake `EventSub` socket: accepts one connection per script, in order, and runs it.
struct FakeSocket {
    url: String,
    thread: Option<JoinHandle<()>>,
}

impl FakeSocket {
    fn new(scripts: Vec<Script>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://127.0.0.1:{}/ws", listener.local_addr().unwrap().port());
        let thread = std::thread::spawn(move || {
            for script in scripts {
                let Ok((stream, _)) = listener.accept() else { return };
                let Ok(mut ws) = tungstenite::accept(stream) else { return };
                script(&mut ws);
            }
        });
        Self { url, thread: Some(thread) }
    }

    fn join(mut self) {
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

/// A Helix that accepts the token (with `scopes`) and every subscription.
fn helix(scopes: &[&str]) -> Arc<FakeHttp> {
    let info = json!({ "client_id": "test-client", "login": "kitdemo", "user_id": "1001", "scopes": scopes,
                       "expires_in": 3600 });
    Arc::new(FakeHttp::new().json(Method::Get, &format!("{OAUTH}/validate"), 200, info).json(
        Method::Post,
        &format!("{API}/eventsub/subscriptions"),
        202,
        json!({ "data": [] }),
    ))
}

fn service(socket: &str, http: &Arc<FakeHttp>, sink: &Arc<Sink>) -> TwitchService {
    let transport: Arc<dyn HttpTransport> = Arc::clone(http) as Arc<dyn HttpTransport>;
    let endpoints = Endpoints { socket: socket.to_owned(), helix: Helix::new(transport, API, OAUTH) };
    TwitchService::with_endpoints(endpoints, Arc::clone(sink) as Arc<dyn EventSink>)
}

/// Waits until the status says `done`, and returns it.
fn until(service: &TwitchService, done: impl Fn(&TwitchStatus) -> bool) -> TwitchStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = service.status().unwrap();
        if done(&status) || Instant::now() > deadline {
            return status;
        }
        std::thread::yield_now();
    }
}

fn subscriptions(http: &FakeHttp) -> Vec<Value> {
    http.calls()
        .iter()
        .filter(|c| c.url.ends_with("/eventsub/subscriptions"))
        .map(|c| serde_json::from_slice(&c.body).unwrap())
        .collect()
}

#[test]
fn a_redeem_reaches_the_show_through_the_subscriptions_the_token_allows() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        send(ws, &redeem("n1", "BRB"));
        hold(ws);
    })]);
    let http = helix(&["channel:read:redemptions", "moderator:read:followers"]);
    let (sink, events) = Sink::new(0);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let event = events.recv_timeout(WAIT).unwrap();
    assert_eq!(event.kind.as_str(), "redeem");
    assert_eq!(event.payload["reward"]["title"], "BRB");
    assert_eq!(event.payload["user"]["login"], "viewer");
    assert_eq!(event.payload["redemption_id"], "redemption-n1");
    assert_eq!(event.origin_id.as_deref(), Some("n1"), "the message id dedups a redelivery");
    assert!(event.payload.get("input").is_none(), "an empty input is no input");

    // The token check sends the token as `OAuth`, the subscriptions as `Bearer` with the app's client id.
    let calls = http.calls();
    let check = calls.iter().find(|c| c.url.ends_with("/validate")).unwrap();
    assert!(check.headers.contains(&("Authorization".to_owned(), format!("OAuth {TOKEN}"))));
    let sub = calls.iter().find(|c| c.url.ends_with("/eventsub/subscriptions")).unwrap();
    assert!(sub.headers.contains(&("Authorization".to_owned(), format!("Bearer {TOKEN}"))));
    assert!(sub.headers.contains(&("Client-Id".to_owned(), "test-client".to_owned())));
    assert_eq!(http.unmatched(), Vec::new());

    // Redeems, follows and raids (which needs no scope); nothing else.
    let made = subscriptions(&http);
    let kinds: Vec<&str> = made.iter().map(|s| s["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["channel.channel_points_custom_reward_redemption.add", "channel.follow", "channel.raid"]);
    assert!(made.iter().all(|s| s["transport"] == json!({ "method": "websocket", "session_id": "S1" })));
    assert_eq!(made[0]["condition"], json!({ "broadcaster_user_id": "1001" }));
    assert_eq!(made[1]["condition"], json!({ "broadcaster_user_id": "1001", "moderator_user_id": "1001" }));
    assert_eq!(made[2]["condition"], json!({ "to_broadcaster_user_id": "1001" }));

    let status = until(&twitch, |s| s.received == 1);
    assert!(status.connected);
    assert_eq!(status.login.as_deref(), Some("kitdemo"));
    assert_eq!(status.events, ["redeem", "follow", "raid"]);
    let skipped: Vec<&str> = status.skipped.iter().map(|s| s.event.as_str()).collect();
    assert_eq!(skipped, ["chat.message", "cheer", "sub"]);
    assert_eq!(status.skipped[0].needs, ["user:read:chat"]);
    assert!(!serde_json::to_string(&status).unwrap().contains(TOKEN));

    assert!(twitch.disconnect());
    assert!(twitch.status().is_none());
    socket.join();
}

#[test]
fn a_reconnect_moves_to_the_new_address_and_keeps_the_subscriptions() {
    // The old connection delivers a redeem after asking for the move; the new one says welcome only once it has.
    let (sent, old_done) = channel::<()>();
    let second = FakeSocket::new(vec![Box::new(move |ws| {
        old_done.recv_timeout(WAIT).unwrap();
        send(ws, &welcome("S2"));
        send(ws, &follow("n2"));
        hold(ws);
    })]);
    let next = second.url.clone();
    let first = FakeSocket::new(vec![Box::new(move |ws| {
        send(ws, &welcome("S1"));
        send(ws, &message("session_reconnect", "m2", &json!({ "session": { "id": "S1", "reconnect_url": next } })));
        send(ws, &redeem("n1", "Late"));
        sent.send(()).unwrap();
        hold(ws);
    })]);
    let http = helix(&["moderator:read:followers", "channel:read:redemptions"]);
    let (sink, events) = Sink::new(0);
    let twitch = service(&first.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let late = events.recv_timeout(WAIT).unwrap();
    assert_eq!(late.kind.as_str(), "redeem", "the old connection delivers until the new one is open");
    assert_eq!(late.payload["reward"]["title"], "Late");
    let event = events.recv_timeout(WAIT).unwrap();
    assert_eq!(event.kind.as_str(), "follow");
    assert_eq!(event.payload["user"]["name"], "NewFan");
    let status = until(&twitch, |s| s.reconnects == 1 && s.connected);
    assert_eq!(status.reconnects, 1);
    assert!(status.error.is_none());
    // Redeems, follows and raids, subscribed once on S1; S2 inherits them.
    let made = subscriptions(&http);
    assert_eq!(made.len(), 3);
    assert!(made.iter().all(|s| s["transport"]["session_id"] == "S1"));

    drop(twitch);
    // The old connection was closed once the new one opened, so its script has ended.
    first.join();
    second.join();
}

#[test]
fn a_dropped_connection_starts_over_with_fresh_subscriptions() {
    let socket = FakeSocket::new(vec![
        Box::new(|ws| {
            send(ws, &welcome("S1"));
            let _ = ws.close(None);
            let _ = ws.flush();
        }),
        Box::new(|ws| {
            send(ws, &welcome("S2"));
            send(ws, &redeem("n3", "Hydrate"));
            hold(ws);
        }),
    ]);
    let http = helix(&["channel:read:redemptions"]);
    let (sink, events) = Sink::new(0);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let event = events.recv_timeout(WAIT).unwrap();
    assert_eq!(event.payload["reward"]["title"], "Hydrate");
    let status = until(&twitch, |s| s.connected && s.reconnects == 1);
    assert_eq!(status.reconnects, 1);
    // Redeems and raids, on each session.
    let sessions: Vec<String> =
        subscriptions(&http).iter().map(|s| s["transport"]["session_id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(sessions, ["S1", "S1", "S2", "S2"]);

    drop(twitch);
    socket.join();
}

#[test]
fn a_full_inbox_keeps_a_redeem_for_later_and_sheds_chat() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        send(ws, &redeem("n4", "BRB"));
        send(ws, &chat("n5", "hello"));
        send(ws, &follow("n6"));
        hold(ws);
    })]);
    let http = helix(&["channel:read:redemptions", "user:read:chat", "moderator:read:followers"]);
    // The first two publishes find the inbox full: the redeem, then its first retry.
    let (sink, events) = Sink::new(2);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let first = events.recv_timeout(WAIT).unwrap();
    let second = events.recv_timeout(WAIT).unwrap();
    assert_eq!(first.kind.as_str(), "redeem", "kept events go out in order");
    assert_eq!(second.kind.as_str(), "follow");
    assert!(events.recv_timeout(Duration::from_millis(300)).is_err(), "chat behind a full inbox is shed");
    let status = until(&twitch, |s| s.received == 3);
    assert_eq!(status.lost, 0);
    assert_eq!(sink.dropped.load(Ordering::SeqCst), 0);

    drop(twitch);
    socket.join();
}

#[test]
fn a_refused_token_is_reported_and_nothing_connects() {
    let http = Arc::new(FakeHttp::new().json(Method::Get, &format!("{OAUTH}/validate"), 401, json!({ "status": 401 })));
    let (sink, _events) = Sink::new(0);
    let twitch = service("ws://127.0.0.1:9/ws", &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();
    let status = until(&twitch, |s| s.error.is_some());
    assert!(!status.connected);
    assert!(status.error.as_deref().unwrap().contains("refused the token"));
    assert!(!format!("{status:?}").contains(TOKEN));
    assert_eq!(http.calls().len(), 1);
}

#[test]
fn a_token_check_that_cannot_reach_twitch_is_tried_again() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        hold(ws);
    })]);
    let info = json!({ "client_id": "test-client", "login": "kitdemo", "user_id": "1001", "scopes": [],
                       "expires_in": 3600 });
    let ok = HttpResponse { status: 200, headers: Vec::new(), body: info.to_string().into_bytes() };
    let http = Arc::new(
        FakeHttp::new()
            .route(
                Method::Get,
                &format!("{OAUTH}/validate"),
                vec![Reply::Error(HttpError::Network("offline".to_owned())), Reply::Response(ok)],
            )
            .json(Method::Post, &format!("{API}/eventsub/subscriptions"), 202, json!({ "data": [] })),
    );
    let (sink, _events) = Sink::new(0);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let status = until(&twitch, |s| s.connected);
    assert!(status.connected, "a network blip at the start doesn't end the adapter: {status:?}");
    assert_eq!(http.calls_to("/validate"), 2);
    assert_eq!(status.events, ["raid"]);
    drop(twitch);
    socket.join();
}

#[test]
fn an_expired_token_stops_the_adapter_instead_of_reconnecting_forever() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        let _ = ws.close(None);
        let _ = ws.flush();
    })]);
    let info = json!({ "client_id": "test-client", "login": "kitdemo", "user_id": "1001", "scopes": [],
                       "expires_in": 3600 });
    let ok = HttpResponse { status: 200, headers: Vec::new(), body: info.to_string().into_bytes() };
    let expired = HttpResponse { status: 401, headers: Vec::new(), body: b"{}".to_vec() };
    let http = Arc::new(
        FakeHttp::new()
            .route(Method::Get, &format!("{OAUTH}/validate"), vec![Reply::Response(ok), Reply::Response(expired)])
            .json(Method::Post, &format!("{API}/eventsub/subscriptions"), 202, json!({ "data": [] })),
    );
    let (sink, _events) = Sink::new(0);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let status = until(&twitch, |s| s.error.as_deref().is_some_and(|e| e.contains("new token")));
    assert!(!status.connected);
    let error = status.error.unwrap();
    assert!(error.contains("refused the token") && error.contains("twitch.connect"), "{error}");
    assert!(!error.contains(TOKEN));
    // Checked at the start and before starting over; subscribed once, on the first session only.
    assert_eq!(http.calls_to("/validate"), 2);
    assert_eq!(subscriptions(&http).len(), 1);
    drop(twitch);
    socket.join();
}

#[test]
fn a_revoked_subscription_leaves_the_events_list_and_triggers_a_token_check() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        let revoked = json!({ "subscription": { "type": "channel.follow", "status": "authorization_revoked" } });
        send(ws, &message("revocation", "r1", &revoked));
        hold(ws);
    })]);
    let http = helix(&["moderator:read:followers"]);
    let (sink, _events) = Sink::new(0);
    let twitch = service(&socket.url, &http, &sink);
    twitch.connect(SecretValue::new(TOKEN)).unwrap();

    let status = until(&twitch, |s| !s.refused.is_empty() && http.calls_to("/validate") == 2);
    assert_eq!(status.events, ["raid"], "the follow subscription is gone");
    assert!(status.refused[0].contains("channel.follow") && status.refused[0].contains("authorization_revoked"));
    assert_eq!(http.calls_to("/validate"), 2, "the revocation checks the token at once");
    assert!(status.connected, "a token that still works keeps the connection");
    drop(twitch);
    socket.join();
}

#[test]
fn chat_carries_roles_badges_and_a_first_message() {
    let m = chat("n7", "hi there");
    let event = normalise("channel.chat.message", &m["payload"]["event"], "n7", Seconds(2.0)).unwrap();
    assert_eq!(event.kind.as_str(), "chat.message");
    assert_eq!(event.at, Seconds(2.0));
    let user = &event.payload["user"];
    assert_eq!(user["roles"], json!(["moderator", "subscriber"]));
    assert_eq!(user["badges"], json!(["moderator/1", "subscriber/12"]));
    assert_eq!(user["colour"], "#FF0000");
    assert_eq!(event.payload["text"], "hi there");
    assert_eq!(event.payload["first_message"], true);
}

#[test]
fn raids_cheers_and_subs_normalise() {
    let raid = json!({ "from_broadcaster_user_id": "4004", "from_broadcaster_user_login": "friend",
                       "from_broadcaster_user_name": "Friend", "to_broadcaster_user_id": "1001",
                       "to_broadcaster_user_login": "kitdemo", "to_broadcaster_user_name": "KitDemo", "viewers": 42 });
    let event = normalise("channel.raid", &raid, "n8", Seconds(0.0)).unwrap();
    assert_eq!(event.kind.as_str(), "raid");
    assert_eq!(event.payload["viewers"], 42);
    assert_eq!(event.payload["from"]["login"], "friend");

    let cheer = json!({ "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo", "is_anonymous": true,
                        "bits": 100, "message": "Cheer100 nice" });
    let event = normalise("channel.cheer", &cheer, "n9", Seconds(0.0)).unwrap();
    assert_eq!(event.kind.as_str(), "cheer");
    assert_eq!(event.payload["bits"], 100);
    assert!(event.payload.get("user").is_none_or(Value::is_null), "an anonymous cheer names nobody");

    let sub = json!({ "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo", "user_id": "5005",
                      "user_login": "subber", "user_name": "Subber", "tier": "1000", "is_gift": false });
    let event = normalise("channel.subscribe", &sub, "n10", Seconds(0.0)).unwrap();
    assert_eq!(event.kind.as_str(), "sub");

    assert!(normalise("channel.update", &json!({}), "n11", Seconds(0.0)).is_none());
    assert_eq!(TOPICS.len(), 6);
}

// The ops.

fn services(twitch: Arc<TwitchService>, secrets: Option<Arc<MemorySecrets>>) -> Services {
    let mut builder = ServicesBuilder::new().insert::<TwitchService>(twitch);
    if let Some(secrets) = secrets {
        builder = builder.insert::<dyn SecretStore>(secrets as Arc<dyn SecretStore>);
    }
    builder.build()
}

fn call(name: &str, args: &Value, services: &Services, dry_run: bool) -> Result<Value, String> {
    omuzaki_twitch::link();
    let op = (find(name).unwrap().decode)(args.clone()).map_err(|e| e.message)?;
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx).map(|p| p.output.unwrap_or(Value::Null)).map_err(|e| e.message)
}

#[test]
fn connect_reads_the_token_from_the_secret_store_and_never_shows_it() {
    let socket = FakeSocket::new(vec![Box::new(|ws| {
        send(ws, &welcome("S1"));
        hold(ws);
    })]);
    let http = helix(&[]);
    let (sink, _events) = Sink::new(0);
    let twitch = Arc::new(service(&socket.url, &http, &sink));

    // No store, or no token in it: refused with a hint, and nothing connects.
    let none = services(Arc::clone(&twitch), Some(Arc::new(MemorySecrets::new())));
    let refused = call("twitch.connect", &json!({}), &none, false).unwrap_err();
    assert!(refused.contains("twitch/token"), "{refused}");
    assert!(twitch.status().is_none());

    let secrets = Arc::new(MemorySecrets::new());
    secrets.set(&SecretLabel::new("twitch/token").unwrap(), SecretValue::new(TOKEN)).unwrap();
    secrets.set(&SecretLabel::new("connector/openai").unwrap(), SecretValue::new("another-service-key")).unwrap();
    let services = services(Arc::clone(&twitch), Some(secrets));
    // Another service's secret never goes to Twitch.
    let other = call("twitch.connect", &json!({ "token": "connector/openai" }), &services, false).unwrap_err();
    assert!(other.contains("not a Twitch secret"), "{other}");
    assert!(twitch.status().is_none());
    let dry = call("twitch.connect", &json!({}), &services, true).unwrap();
    assert_eq!(dry["token_stored"], true);
    assert_eq!(dry["events"], json!(["redeem", "chat.message", "follow", "raid", "cheer", "sub"]));
    assert!(twitch.status().is_none(), "a dry run connects nothing");

    let done = call("twitch.connect", &json!({ "token": "twitch/token" }), &services, false).unwrap();
    assert_eq!(done["connecting"], true);
    let status = until(&twitch, |s| s.connected);
    assert_eq!(status.events, ["raid"]);
    let reported = call("twitch.status", &json!({}), &services, false).unwrap();
    assert_eq!(reported["connected"], true);
    for shown in [&dry, &done, &reported] {
        assert!(!shown.to_string().contains(TOKEN));
    }

    assert_eq!(call("twitch.disconnect", &json!({}), &services, false).unwrap()["disconnected"], true);
    assert_eq!(call("twitch.status", &json!({}), &services, false).unwrap(), json!({ "connected": false }));
    assert_eq!(call("twitch.disconnect", &json!({}), &services, false).unwrap()["disconnected"], false);
    socket.join();
}
