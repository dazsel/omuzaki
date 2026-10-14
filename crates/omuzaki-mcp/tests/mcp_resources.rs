//! Resources end to end: the built-in project and character resources, and a host-registered provider.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeRunner;
use omuzaki_mcp::{McpOptions, McpServer, MeasureProvider, RenderProvider, ResourceProvider};
use omuzaki_ops_api::{Actor, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_store_api::{Commercial, ContentHash, Store};
use omuzaki_store_mock::{MemStore, conformance::sample_provenance};
use rmcp::model::{
    CallToolRequestParams, ErrorCode, ResourceContents, ResourceTemplate, ResourceUpdatedNotificationParam,
    ServerNotification, SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
};
use rmcp::model::{
    ClientCapabilities, ClientConfig, Implementation, ProtocolVersion, ReadResourceRequestParams, Resource,
};
use rmcp::service::{NotificationContext, ServiceError};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }
}

async fn connect(doc: Value, options: McpOptions) -> RunningService<RoleClient, Client> {
    let server = McpServer::new(Arc::new(FakeRunner::with_doc(doc)), options).expect("server builds");
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move { server.serve(server_io).await });
    Client.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("client connects")
}

fn two_characters() -> Value {
    json!({
        "name": "Demo",
        "characters": {"mira": {"face": {"blend": 0.5}, "hair": {"style": "bob"}}, "noor": {"face": {}}},
    })
}

async fn uris(client: &RunningService<RoleClient, Client>) -> Vec<String> {
    client.list_all_resources().await.unwrap().into_iter().map(|r| r.uri).collect()
}

async fn read_json<H: ClientHandler>(client: &RunningService<RoleClient, H>, uri: &str) -> Value {
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await.unwrap();
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, mime_type, .. } => {
            assert_eq!(mime_type.as_deref(), Some("application/json"));
            serde_json::from_str(text).unwrap()
        }
        other => panic!("expected text, got {other:?}"),
    }
}

async fn read_error(client: &RunningService<RoleClient, Client>, uri: &str) -> String {
    client.read_resource(ReadResourceRequestParams::new(uri)).await.unwrap_err().to_string()
}

/// The protocol error code of a failed read.
async fn read_error_code(client: &RunningService<RoleClient, Client>, uri: &str) -> ErrorCode {
    match client.read_resource(ReadResourceRequestParams::new(uri)).await.unwrap_err() {
        ServiceError::McpError(error) => error.code,
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

#[tokio::test]
async fn the_list_has_the_log_the_project_and_every_character() {
    let client = connect(two_characters(), McpOptions::default()).await;
    assert_eq!(
        uris(&client).await,
        ["omuzaki://log", "omuzaki://project", "omuzaki://character/mira", "omuzaki://character/noor"]
    );
    let templates = client.list_all_resource_templates().await.unwrap();
    assert_eq!(templates.iter().map(|t| t.uri_template.as_str()).collect::<Vec<_>>(), ["omuzaki://character/{id}"]);
}

#[tokio::test]
async fn the_project_is_a_summary_not_a_dump() {
    let client = connect(two_characters(), McpOptions::default()).await;
    let project = read_json(&client, "omuzaki://project").await;
    assert_eq!(project["sections"], json!({"name": "Demo", "characters": {"ids": ["mira", "noor"]}}));
    assert!(project["note"].as_str().unwrap().contains("omuzaki://character/<id>"));
}

#[tokio::test]
async fn a_character_is_read_whole() {
    let client = connect(two_characters(), McpOptions::default()).await;
    assert_eq!(
        read_json(&client, "omuzaki://character/mira").await,
        json!({"face": {"blend": 0.5}, "hair": {"style": "bob"}})
    );
}

#[tokio::test]
async fn what_does_not_exist_is_resource_not_found_naming_what_does() {
    let client = connect(two_characters(), McpOptions::default()).await;
    let missing = read_error(&client, "omuzaki://character/zed").await;
    assert!(missing.contains("no character") && missing.contains("mira, noor"), "{missing}");
    // A slash would reach into a character's sub-paths, which this resource doesn't offer.
    let nested = read_error(&client, "omuzaki://character/mira/face").await;
    assert!(nested.contains("no character") && nested.contains("mira, noor"), "{nested}");
    for uri in ["omuzaki://character/zed", "omuzaki://character/mira/face", "omuzaki://nope"] {
        assert_eq!(read_error_code(&client, uri).await, ErrorCode::RESOURCE_NOT_FOUND, "{uri}");
    }
    let unknown = read_error(&client, "omuzaki://nope").await;
    assert!(unknown.contains("omuzaki://log") && unknown.contains("omuzaki://character/{id}"), "{unknown}");
}

#[tokio::test]
async fn every_read_needs_the_read_scope() {
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let client = connect(two_characters(), options).await;
    for uri in ["omuzaki://project", "omuzaki://character/mira", "omuzaki://log"] {
        let error = read_error(&client, uri).await;
        assert!(error.contains("needs the `read` scope"), "{uri}: {error}");
    }
    // Listing names characters from the document, so it too is limited to what names nothing.
    assert_eq!(uris(&client).await, ["omuzaki://log"]);
}

/// A resource family a host adds: no list in `omuzaki-mcp` knows about it.
#[derive(Debug)]
struct Notes;

impl ResourceProvider for Notes {
    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate::new("omuzaki://notes/{topic}", "notes")]
    }

    fn list(&self, _runner: &dyn Runner, _grants: &Grants) -> Result<Vec<Resource>, OpError> {
        Ok(vec![Resource::new("omuzaki://notes/todo", "todo")])
    }

    fn serves(&self, uri: &str) -> bool {
        uri.starts_with("omuzaki://notes/")
    }

    fn read(&self, _runner: &dyn Runner, _grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError> {
        match uri {
            "omuzaki://notes/todo" => Ok(vec![ResourceContents::text("ship it", uri)]),
            _ => Err(OpError::not_found(format!("no note at {uri}"))),
        }
    }
}

#[tokio::test]
async fn a_registered_provider_is_listed_templated_and_readable() {
    let client = connect(two_characters(), McpOptions::default().with_provider(Arc::new(Notes))).await;
    assert!(uris(&client).await.contains(&"omuzaki://notes/todo".to_owned()));
    let templates = client.list_all_resource_templates().await.unwrap();
    let templates: Vec<_> = templates.iter().map(|t| t.uri_template.as_str()).collect();
    assert_eq!(templates, ["omuzaki://character/{id}", "omuzaki://notes/{topic}"]);
    let read = client.read_resource(ReadResourceRequestParams::new("omuzaki://notes/todo")).await.unwrap();
    assert!(matches!(&read.contents[0], ResourceContents::TextResourceContents { text, .. } if text == "ship it"));
    assert!(read_error(&client, "omuzaki://notes/other").await.contains("no note"));
    // The built-ins still work next to it.
    assert_eq!(read_json(&client, "omuzaki://character/noor").await, json!({"face": {}}));
}

#[tokio::test]
async fn a_custom_provider_is_read_scoped_by_the_server() {
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter])).with_provider(Arc::new(Notes));
    let client = connect(two_characters(), options).await;
    assert!(read_error(&client, "omuzaki://notes/todo").await.contains("needs the `read` scope"));
}

// ---- subscriptions -----------------------------------------------------------------------------------------------

/// A client that records the URI of every `resources/updated` it is sent.
struct Listener {
    protocol: ProtocolVersion,
    updates: mpsc::UnboundedSender<String>,
}

impl ClientHandler for Listener {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
            .with_protocol_version(self.protocol.clone())
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.updates.send(params.uri);
    }
}

struct Watching {
    client: RunningService<RoleClient, Listener>,
    updates: mpsc::UnboundedReceiver<String>,
}

impl Watching {
    async fn connect(options: McpOptions) -> Self {
        Self::connect_as(options, ProtocolVersion::LATEST_WITH_INITIALIZE, ClientLifecycleMode::Initialize).await
    }

    async fn connect_as(options: McpOptions, protocol: ProtocolVersion, mode: ClientLifecycleMode) -> Self {
        let runner = Arc::new(FakeRunner::with_doc(two_characters()));
        let server = McpServer::new(runner, options).expect("server builds");
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move { server.serve(server_io).await });
        let (tx, updates) = mpsc::unbounded_channel();
        let client = Listener { protocol, updates: tx }.serve_with_lifecycle(client_io, mode).await.expect("connects");
        Self { client, updates }
    }

    #[allow(deprecated)] // the legacy subscribe call is what clients on the `initialize` lifecycle use
    async fn subscribe(&self, uri: &str) {
        self.client.subscribe(SubscribeRequestParams::new(uri)).await.expect("subscribes");
    }

    #[allow(deprecated)] // see `subscribe`
    async fn unsubscribe(&self, uri: &str) {
        self.client.unsubscribe(UnsubscribeRequestParams::new(uri)).await.expect("unsubscribes");
    }

    /// Sets a character's blend through the tool, as the connected agent.
    async fn set_blend(&self, character: &str) {
        let args = json!({"character": character, "blend": 0.25}).as_object().unwrap().clone();
        self.client.call_tool(CallToolRequestParams::new("face_set_blend").with_arguments(args)).await.expect("call");
    }

    /// What the server sent for the last change: everything up to the log's own notification. The server sends the log
    /// last in each wake, and one connection delivers in order, so the log arriving proves nothing else is on its way
    /// (a notification that should not come cannot slip past). Needs the log subscribed.
    async fn wake(&mut self) -> Vec<String> {
        let mut got = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(5), self.updates.recv()).await;
            match next.expect("the log notification in time").expect("open") {
                uri if uri == "omuzaki://log" => return got,
                uri => got.push(uri),
            }
        }
    }
}

#[tokio::test]
async fn an_op_on_a_subscribed_character_notifies_it_and_the_project() {
    let mut w = Watching::connect(McpOptions::default()).await;
    for uri in ["omuzaki://character/mira", "omuzaki://project", "omuzaki://log"] {
        w.subscribe(uri).await;
    }
    w.set_blend("mira").await;
    assert_eq!(w.wake().await, ["omuzaki://character/mira", "omuzaki://project"]);
}

#[tokio::test]
async fn an_edit_to_another_character_sends_nothing_for_this_one() {
    let mut w = Watching::connect(McpOptions::default()).await;
    w.subscribe("omuzaki://character/mira").await;
    w.subscribe("omuzaki://log").await;
    w.set_blend("noor").await;
    assert_eq!(w.wake().await, Vec::<String>::new());
    // The subscription is alive: an edit to mira still arrives.
    w.set_blend("mira").await;
    assert_eq!(w.wake().await, ["omuzaki://character/mira"]);
}

#[tokio::test]
async fn undo_and_redo_notify_what_is_subscribed() {
    let mut w = Watching::connect(McpOptions::default()).await;
    w.subscribe("omuzaki://character/mira").await;
    w.subscribe("omuzaki://log").await;
    w.set_blend("mira").await;
    assert_eq!(w.wake().await, ["omuzaki://character/mira"]);
    for tool in ["history_undo", "history_redo"] {
        w.client.call_tool(CallToolRequestParams::new(tool)).await.expect("call");
        assert_eq!(w.wake().await, ["omuzaki://character/mira"], "{tool}");
    }
}

#[tokio::test]
async fn nothing_arrives_after_unsubscribe() {
    let mut w = Watching::connect(McpOptions::default()).await;
    for uri in ["omuzaki://character/mira", "omuzaki://project", "omuzaki://log"] {
        w.subscribe(uri).await;
    }
    w.unsubscribe("omuzaki://character/mira").await;
    w.set_blend("mira").await;
    // Only what is still subscribed is sent.
    assert_eq!(w.wake().await, ["omuzaki://project"]);
    w.unsubscribe("omuzaki://project").await;
    w.set_blend("mira").await;
    assert_eq!(w.wake().await, Vec::<String>::new());
}

#[tokio::test]
async fn a_provider_resource_can_be_subscribed_and_unknown_uris_cannot() {
    let w = Watching::connect(McpOptions::default().with_provider(Arc::new(Notes))).await;
    w.subscribe("omuzaki://notes/todo").await;
    #[allow(deprecated)]
    let unknown = w.client.subscribe(SubscribeRequestParams::new("omuzaki://nope")).await.unwrap_err();
    assert!(unknown.to_string().contains("unknown resource omuzaki://nope"), "{unknown}");
}

#[tokio::test]
async fn subscribing_and_unsubscribing_need_the_read_scope() {
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let w = Watching::connect(options).await;
    // A served URI and an unknown one are refused alike: the scope is checked before the lookup.
    for uri in ["omuzaki://character/mira", "omuzaki://project", "omuzaki://nope"] {
        #[allow(deprecated)]
        let subscribed = w.client.subscribe(SubscribeRequestParams::new(uri)).await.unwrap_err();
        assert!(subscribed.to_string().contains("needs the `read` scope"), "subscribe {uri}: {subscribed}");
        #[allow(deprecated)]
        let unsubscribed = w.client.unsubscribe(UnsubscribeRequestParams::new(uri)).await.unwrap_err();
        assert!(unsubscribed.to_string().contains("needs the `read` scope"), "unsubscribe {uri}: {unsubscribed}");
    }
}

#[tokio::test]
async fn protocol_2026_listen_covers_provider_resources() {
    let mode = ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] };
    let w = Watching::connect_as(McpOptions::default(), ProtocolVersion::V_2026_07_28, mode).await;
    let filter = SubscriptionFilter::builder()
        .resource_subscription("omuzaki://character/mira")
        .resource_subscription("omuzaki://nope")
        .resource_subscription("omuzaki://log")
        .build();
    let mut subscription = w.client.listen(filter).await.unwrap();
    // What the server agreed to: the resources it serves, not the one it doesn't.
    assert_eq!(
        subscription.acknowledged().resource_subscriptions.as_deref(),
        Some(&["omuzaki://character/mira".to_owned(), "omuzaki://log".to_owned()][..])
    );
    // The stream carries the same notifications, log last.
    for (character, expected) in [("noor", Vec::<&str>::new()), ("mira", vec!["omuzaki://character/mira"])] {
        w.set_blend(character).await;
        // One deadline for the whole wake, and an ended stream is a failure, not something to retry.
        let collect = async {
            let mut got = Vec::new();
            loop {
                match subscription.next().await.expect("the stream carries valid messages") {
                    None => panic!("the subscription ended before the log notification"),
                    Some(ServerNotification::ResourceUpdatedNotification(note)) => {
                        if note.params.uri == "omuzaki://log" {
                            return got;
                        }
                        got.push(note.params.uri);
                    }
                    Some(_) => {}
                }
            }
        };
        let got = tokio::time::timeout(Duration::from_secs(5), collect).await.expect("the log notification in time");
        assert_eq!(got, expected, "after an edit to {character}");
    }
    subscription.cancel().await.unwrap();
}

// ---- measure -----------------------------------------------------------------------------------------------------

/// A stand-in for `measure.liveliness`, which lives in `omuzaki-anim` (this crate doesn't depend on it). It answers what
/// it was given, and refuses one baseline, so the tests can see the arguments, the actor and a refusal.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Liveliness {
    /// A life run or a recording.
    source: Value,
    /// The baseline to judge against.
    baseline: String,
}

impl Op for Liveliness {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.liveliness",
            summary: "Judge a life run or a recording against a baseline.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.baseline == "dancing" {
            return Err(OpError::invalid("baseline", "unknown baseline `dancing`"));
        }
        let ran_as_system = matches!(ctx.actor, Actor::System);
        Ok(Plan::answer(json!({"baseline": self.baseline, "source": self.source, "ran_as_system": ran_as_system})))
    }
}

omuzaki_ops_api::register_op!(Liveliness);

/// A server with the measure provider registered, as the App registers it.
async fn connect_measuring(options: McpOptions) -> RunningService<RoleClient, Client> {
    connect(two_characters(), options.with_provider(Arc::new(MeasureProvider))).await
}

const LIFE_TEMPLATE: &str = "omuzaki://measure/liveliness/life/{seed}/{seconds}/{baseline}{?personality}";
const RECORDING_TEMPLATE: &str = "omuzaki://measure/liveliness/recording/{baseline}/{+path}";

#[tokio::test]
async fn the_measure_templates_are_advertised_and_nothing_is_listed() {
    let client = connect_measuring(McpOptions::default()).await;
    let templates = client.list_all_resource_templates().await.unwrap();
    let advertised: Vec<_> = templates.iter().map(|t| t.uri_template.as_str()).collect();
    assert_eq!(advertised, ["omuzaki://character/{id}", LIFE_TEMPLATE, RECORDING_TEMPLATE]);
    for template in templates.iter().filter(|t| t.uri_template.contains("measure")) {
        assert_eq!(template.mime_type.as_deref(), Some("application/json"), "{}", template.uri_template);
    }
    // A measurement is computed on read, so there is nothing stored to list.
    assert_eq!(
        uris(&client).await,
        ["omuzaki://log", "omuzaki://project", "omuzaki://character/mira", "omuzaki://character/noor"]
    );
}

/// The body a measure read returns: one text resource, as JSON.
async fn measure_body(client: &RunningService<RoleClient, Client>, uri: &str) -> Value {
    let read = client.read_resource(ReadResourceRequestParams::new(uri)).await.unwrap();
    assert_eq!(read.contents.len(), 1, "{uri}");
    read_json(client, uri).await
}

#[tokio::test]
async fn a_life_run_is_measured_as_the_system_and_its_answer_returned() {
    let client = connect_measuring(McpOptions::default()).await;
    let body = measure_body(&client, "omuzaki://measure/liveliness/life/1/20/rest").await;
    assert_eq!(
        body,
        json!({
            "op": "measure.liveliness",
            "args": {"source": {"life": {"seed": 1, "seconds": 20.0}}, "baseline": "rest"},
            "result": {"baseline": "rest", "source": {"life": {"seed": 1, "seconds": 20.0}}, "ran_as_system": true},
        })
    );
}

#[tokio::test]
async fn a_personality_is_passed_to_the_op() {
    let client = connect_measuring(McpOptions::default()).await;
    let body = measure_body(&client, "omuzaki://measure/liveliness/life/1/20/rest?personality=shy").await;
    assert_eq!(body["args"]["source"]["life"]["personality"], "shy");
    assert_eq!(body["result"]["source"]["life"]["personality"], "shy");
}

#[tokio::test]
async fn a_recording_is_measured_by_its_path_against_its_baseline() {
    let client = connect_measuring(McpOptions::default()).await;
    let body = measure_body(&client, "omuzaki://measure/liveliness/recording/talking/takes/a-1.json").await;
    assert_eq!(body["args"], json!({"source": {"recording": "takes/a-1.json"}, "baseline": "talking"}));
    assert_eq!(body["result"]["ran_as_system"], true);
}

#[tokio::test]
async fn a_malformed_measure_uri_is_not_found_naming_both_forms() {
    let client = connect_measuring(McpOptions::default()).await;
    let bad = [
        "omuzaki://measure/liveliness",
        "omuzaki://measure/likeness/x",
        "omuzaki://measure/liveliness/life/x/20/rest",
        "omuzaki://measure/liveliness/life/1/abc/rest",
        "omuzaki://measure/liveliness/life/1/20",
        "omuzaki://measure/liveliness/life/1/20/rest?color=red",
        "omuzaki://measure/liveliness/recording/rest/a%20b.json",
        "omuzaki://measure/liveliness/recording/rest/",
    ];
    for uri in bad {
        assert_eq!(read_error_code(&client, uri).await, ErrorCode::RESOURCE_NOT_FOUND, "{uri}");
        let message = read_error(&client, uri).await;
        assert!(
            message.contains("life/<seed>/<seconds>/<baseline>") && message.contains("recording/<baseline>/<path>"),
            "{uri}: {message}"
        );
    }
}

#[tokio::test]
async fn a_baseline_the_op_refuses_is_not_found_naming_the_measurement() {
    let client = connect_measuring(McpOptions::default()).await;
    let uri = "omuzaki://measure/liveliness/life/1/20/dancing";
    assert_eq!(read_error_code(&client, uri).await, ErrorCode::RESOURCE_NOT_FOUND);
    let message = read_error(&client, uri).await;
    assert!(message.contains("names no measurement") && message.contains("dancing"), "{message}");
}

#[tokio::test]
async fn a_measurement_needs_the_read_scope() {
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let client = connect_measuring(options).await;
    let error = read_error(&client, "omuzaki://measure/liveliness/life/1/20/rest").await;
    assert!(error.contains("needs the `read` scope"), "{error}");
}

// ---- render ------------------------------------------------------------------------------------------------------

/// A PNG for the tests: the eight signature bytes, then three more.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];

/// A server with the render provider over `store`, as the App registers it.
async fn connect_rendering(store: &Arc<MemStore>, options: McpOptions) -> RunningService<RoleClient, Client> {
    let store: Arc<dyn Store> = store.clone();
    connect(two_characters(), options.with_provider(Arc::new(RenderProvider::new(store)))).await
}

fn render_uri(hash: &ContentHash) -> String {
    format!("omuzaki://render/{hash}.png")
}

#[tokio::test]
async fn a_stored_render_is_read_as_a_png_blob() {
    let store = Arc::new(MemStore::new());
    let hash = store.put(PNG, sample_provenance(Commercial::Ok)).expect("stores");
    let client = connect_rendering(&store, McpOptions::default()).await;
    let uri = render_uri(&hash);
    let read = client.read_resource(ReadResourceRequestParams::new(uri.clone())).await.unwrap();
    assert_eq!(read.contents.len(), 1);
    match &read.contents[0] {
        ResourceContents::BlobResourceContents { uri: got, mime_type, blob, .. } => {
            assert_eq!(got, &uri);
            assert_eq!(mime_type.as_deref(), Some("image/png"));
            assert_eq!(blob, "iVBORw0KGgoBAgM=");
        }
        other => panic!("expected a blob, got {other:?}"),
    }
}

#[tokio::test]
async fn the_render_template_is_advertised_and_nothing_is_listed() {
    let store = Arc::new(MemStore::new());
    store.put(PNG, sample_provenance(Commercial::Ok)).expect("stores");
    let client = connect_rendering(&store, McpOptions::default()).await;
    let templates = client.list_all_resource_templates().await.unwrap();
    let advertised: Vec<_> = templates.iter().map(|t| t.uri_template.as_str()).collect();
    assert_eq!(advertised, ["omuzaki://character/{id}", "omuzaki://render/{id}.png"]);
    assert_eq!(templates[1].mime_type.as_deref(), Some("image/png"));
    // A render never changes and the store cannot tell renders from other assets, so none is listed.
    assert_eq!(
        uris(&client).await,
        ["omuzaki://log", "omuzaki://project", "omuzaki://character/mira", "omuzaki://character/noor"]
    );
}

#[tokio::test]
async fn what_names_no_render_is_resource_not_found() {
    let store = Arc::new(MemStore::new());
    let hash = store.put(PNG, sample_provenance(Commercial::Ok)).expect("stores");
    let client = connect_rendering(&store, McpOptions::default()).await;
    let never_stored = render_uri(&ContentHash::of(b"never stored"));
    assert_eq!(read_error_code(&client, &never_stored).await, ErrorCode::RESOURCE_NOT_FOUND);
    assert!(read_error(&client, &never_stored).await.contains("no render"));
    let hash = hash.to_string();
    for uri in [
        "omuzaki://render/zzz.png".to_owned(),
        format!("omuzaki://render/{hash}"),
        format!("omuzaki://render/{}.png", hash.to_uppercase()),
    ] {
        assert_eq!(read_error_code(&client, &uri).await, ErrorCode::RESOURCE_NOT_FOUND, "{uri}");
        assert!(read_error(&client, &uri).await.contains("64 lowercase hex characters"), "{uri}");
    }
}

#[tokio::test]
async fn a_stored_asset_that_is_not_a_png_is_not_a_render() {
    let store = Arc::new(MemStore::new());
    let hash = store.put(b"not a png", sample_provenance(Commercial::Ok)).expect("stores");
    let client = connect_rendering(&store, McpOptions::default()).await;
    let uri = render_uri(&hash);
    assert_eq!(read_error_code(&client, &uri).await, ErrorCode::RESOURCE_NOT_FOUND);
    assert!(read_error(&client, &uri).await.contains("isn't a PNG"));
}

#[tokio::test]
async fn a_damaged_render_is_an_internal_error() {
    let store = Arc::new(MemStore::new());
    let hash = store.put(PNG, sample_provenance(Commercial::Ok)).expect("stores");
    assert!(store.flip_byte(&hash));
    let client = connect_rendering(&store, McpOptions::default()).await;
    let uri = render_uri(&hash);
    assert_eq!(read_error_code(&client, &uri).await, ErrorCode::INTERNAL_ERROR);
    assert!(read_error(&client, &uri).await.contains("damaged"));
}

#[tokio::test]
async fn a_render_needs_the_read_scope() {
    let store = Arc::new(MemStore::new());
    let hash = store.put(PNG, sample_provenance(Commercial::Ok)).expect("stores");
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let client = connect_rendering(&store, options).await;
    let error = read_error(&client, &render_uri(&hash)).await;
    assert!(error.contains("needs the `read` scope"), "{error}");
}
