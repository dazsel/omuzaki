//! The `YouTube` adapter against a scripted API: it finds the live broadcast, skips the backlog, publishes chat, Super
//! Chats and memberships in order, turns new subscribers into follows, counts its quota, keeps the token in the
//! header only, and stops with a clear reason when the stream ends or the token is refused.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_http_api::{HttpResponse, Method};
use omuzaki_http_mock::{FakeHttp, Reply};
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError};
use omuzaki_ops_api::{Actor, OpCtx, ServicesBuilder, find};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use omuzaki_secrets_api::value::SecretValue;
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_youtube::api::Api;
use omuzaki_youtube::{Endpoints, YouTubeService};
use serde_json::{Value, json};

const API: &str = "https://yt.test/v3";
const TOKEN: &str = "ya29.secret-token";

struct Sink(Mutex<Sender<LiveEvent>>);

impl EventSink for Sink {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        self.0.lock().unwrap().send(event).unwrap();
        Ok(())
    }
    fn producer_dropped(&self, _: u64) {}
}

fn sink() -> (Arc<Sink>, Receiver<LiveEvent>) {
    let (tx, rx) = channel();
    (Arc::new(Sink(Mutex::new(tx))), rx)
}

fn page(next: &str, items: &Value) -> Reply {
    let body = json!({ "nextPageToken": next, "pollingIntervalMillis": 10, "items": items });
    Reply::Response(HttpResponse { status: 200, headers: Vec::new(), body: body.to_string().into_bytes() })
}

fn viewer(id: &str, name: &str) -> Value {
    json!({ "channelId": id, "displayName": name, "isChatSponsor": false })
}

fn text(id: &str, who: &str, words: &str) -> Value {
    json!({ "id": id, "snippet": { "type": "textMessageEvent", "textMessageDetails": { "messageText": words } },
            "authorDetails": viewer(who, who) })
}

fn messages_url(page: Option<&str>) -> String {
    let base = format!("{API}/liveChat/messages?liveChatId=chat1&part=snippet%2CauthorDetails&maxResults=2000");
    page.map_or(base.clone(), |p| format!("{base}&pageToken={p}"))
}

fn scripted() -> FakeHttp {
    let broadcast =
        json!({ "items": [{ "snippet": { "liveChatId": "chat1", "channelId": "UCme", "title": "Cosy stream" } }] });
    let super_chat = json!({ "id": "sc1", "snippet": { "type": "superChatEvent",
        "superChatDetails": { "amountMicros": "5000000", "currency": "USD", "userComment": "for the outfit" } },
        "authorDetails": viewer("UCmochi", "Mochi") });
    let member = json!({ "id": "mb1", "snippet": { "type": "newSponsorEvent", "newSponsorDetails": { "memberLevelName": "Stars" } },
        "authorDetails": viewer("UCpip", "Pip") });
    let subscribers = |ids: &[&str]| {
        let items: Vec<Value> =
            ids.iter().map(|id| json!({ "id": id, "subscriberSnippet": { "channelId": id, "title": id } })).collect();
        Reply::Response(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: json!({ "items": items }).to_string().into_bytes(),
        })
    };
    let offline = Reply::Response(HttpResponse {
        status: 200,
        headers: Vec::new(),
        body: json!({ "pollingIntervalMillis": 10, "offlineAt": "2026-10-30T05:00:00Z", "items": [] })
            .to_string()
            .into_bytes(),
    });
    FakeHttp::new()
        .json(
            Method::Get,
            &format!("{API}/liveBroadcasts?part=snippet&broadcastStatus=active&broadcastType=all&maxResults=5"),
            200,
            broadcast,
        )
        .route(
            Method::Get,
            &messages_url(None),
            vec![page("p1", &json!([text("old", "UCold", "said before we joined")]))],
        )
        .route(
            Method::Get,
            &messages_url(Some("p1")),
            vec![page("p2", &json!([text("m1", "UCkaz", "hi chat"), super_chat]))],
        )
        .route(Method::Get, &messages_url(Some("p2")), vec![page("p3", &json!([member]))])
        .route(Method::Get, &messages_url(Some("p3")), vec![offline])
        .route(
            Method::Get,
            &format!("{API}/subscriptions?part=subscriberSnippet&myRecentSubscribers=true&maxResults=50"),
            vec![subscribers(&["UCa"]), subscribers(&["UCnew", "UCa"])],
        )
}

fn endpoints(http: Arc<FakeHttp>) -> Endpoints {
    let mut endpoints = Endpoints::new(Api::new(http, API));
    endpoints.min_poll = Duration::from_millis(10);
    endpoints.subscribers_every = Duration::from_millis(15);
    endpoints
}

fn wait_for(youtube: &YouTubeService, done: impl Fn(&omuzaki_youtube::YouTubeStatus) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if youtube.status().is_some_and(|s| done(&s)) {
            return;
        }
        std::thread::yield_now();
    }
    panic!("timed out; status {:?}", youtube.status());
}

#[test]
fn chat_super_chats_members_and_subscribers_reach_the_show_and_the_end_is_reported() {
    let http = Arc::new(scripted());
    let (sink, events) = sink();
    let youtube = YouTubeService::with_endpoints(endpoints(Arc::clone(&http)), sink);
    youtube.connect(SecretValue::new(TOKEN), None).unwrap();
    wait_for(&youtube, |s| s.error.is_some());

    let got: Vec<LiveEvent> = events.try_iter().collect();
    let kinds: Vec<&str> = got.iter().filter(|e| e.kind.as_str() != "follow").map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["chat.message", "tip", "sub"], "the backlog is skipped, the rest arrives in order");
    assert!(got.iter().all(|e| e.payload.get("text").is_none_or(|t| t != "said before we joined")));
    let follows: Vec<&Value> =
        got.iter().filter(|e| e.kind.as_str() == "follow").map(|e| &e.payload["user"]["id"]).collect();
    assert!(follows.iter().all(|id| *id == "UCnew"), "only the new subscriber: {follows:?}");

    let status = youtube.status().unwrap();
    assert!(!status.connected);
    assert_eq!(status.error.as_deref(), Some("the stream's live chat has ended"));
    assert_eq!(status.broadcast.as_deref(), Some("Cosy stream"));
    assert!(status.quota_used > 4 * 5, "one list call and four chat pages at least: {status:?}");
    assert_eq!(status.polls, 4);

    // The token went only into the Authorization header.
    for call in http.calls() {
        assert!(!call.url.contains(TOKEN), "{}", call.url);
        assert!(call.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Bearer {TOKEN}")));
    }
    assert_eq!(http.unmatched(), Vec::new());
}

#[test]
fn a_refused_token_stops_the_adapter_with_a_way_to_fix_it() {
    let http = Arc::new(FakeHttp::new().json(
        Method::Get,
        &format!("{API}/liveBroadcasts?part=snippet&broadcastStatus=active&broadcastType=all&maxResults=5"),
        401,
        json!({ "error": { "code": 401 } }),
    ));
    let (sink, _events) = sink();
    let youtube = YouTubeService::with_endpoints(endpoints(http), sink);
    youtube.connect(SecretValue::new(TOKEN), None).unwrap();
    wait_for(&youtube, |s| s.error.is_some());
    let error = youtube.status().unwrap().error.unwrap();
    assert!(error.contains("youtube/token") && !error.contains(TOKEN), "{error}");
}

#[test]
fn a_spent_quota_stops_polling() {
    let http = Arc::new(
        FakeHttp::new()
            .json(
                Method::Get,
                &format!("{API}/liveBroadcasts?part=snippet&broadcastStatus=active&broadcastType=all&maxResults=5"),
                200,
                json!({ "items": [{ "snippet": { "liveChatId": "chat1", "channelId": "UCme", "title": "t" } }] }),
            )
            .json(
                Method::Get,
                &messages_url(None),
                403,
                json!({ "error": { "errors": [{ "reason": "quotaExceeded" }] } }),
            )
            .json(
                Method::Get,
                &format!("{API}/subscriptions?part=subscriberSnippet&myRecentSubscribers=true&maxResults=50"),
                200,
                json!({ "items": [] }),
            ),
    );
    let (sink, _events) = sink();
    let youtube = YouTubeService::with_endpoints(endpoints(http), sink);
    youtube.connect(SecretValue::new(TOKEN), None).unwrap();
    wait_for(&youtube, |s| s.error.is_some());
    assert!(youtube.status().unwrap().error.unwrap().contains("quota"));
}

#[test]
fn the_connect_op_reads_only_youtube_secrets_and_never_takes_a_token_argument() {
    omuzaki_youtube::link();
    let (sink, _events) = sink();
    let youtube = Arc::new(YouTubeService::with_endpoints(endpoints(Arc::new(FakeHttp::new())), sink));
    let secrets = Arc::new(MemorySecrets::new());
    let services = ServicesBuilder::new()
        .insert::<YouTubeService>(Arc::clone(&youtube))
        .insert::<dyn SecretStore>(secrets.clone() as Arc<dyn SecretStore>)
        .build();
    let (doc, clock, actor) =
        (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Agent { id: "a#1".into(), model: None });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(&services);
    let run = |args: Value| {
        let op = (find("youtube.connect").unwrap().decode)(args).map_err(|e| e.to_string())?;
        op.plan(&ctx).map(|p| p.output.unwrap_or(Value::Null)).map_err(|e| e.to_string())
    };
    assert!(run(json!({ "token": "twitch/token" })).unwrap_err().contains("not a YouTube secret"));
    assert!(run(json!({ "access_token": TOKEN })).is_err(), "no field takes a token");
    assert!(run(json!({})).unwrap_err().contains("no YouTube token"));
    assert!(run(json!({ "video": "../x" })).unwrap_err().contains("video id"));
    secrets.set(&SecretLabel::new("youtube/token").unwrap(), SecretValue::new(TOKEN)).unwrap();
    let out = run(json!({})).unwrap();
    assert_eq!(out["connecting"], true);
    assert!(!out.to_string().contains(TOKEN));
    let status = (find("youtube.status").unwrap().decode)(json!({})).unwrap().plan(&ctx).unwrap().output.unwrap();
    assert!(!status.to_string().contains(TOKEN));
}
