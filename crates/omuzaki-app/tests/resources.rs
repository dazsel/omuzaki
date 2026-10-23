//! The asset store and the MCP resources of the app core: every op reaches the person's asset store, and an agent reads
//! what an op stored there as a resource of the same server.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::sync::Arc;

use omuzaki_app::{AppCore, AppOptions, with_resource_providers};
use omuzaki_library_api::Library;
use omuzaki_library_mock::MockLibrary;
use omuzaki_mcp::{InAppSpec, McpOptions, MetricsSource};
use omuzaki_ops_api::{
    Actor, ErrorCode, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Services, ServicesBuilder, Undo,
    register_op,
};
use omuzaki_store_api::{Commercial, Store};
use omuzaki_store_mock::{MemStore, conformance::sample_provenance};
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, ReadResourceRequestParams, ResourceContents};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The eight bytes every PNG starts with, then a body: enough for the render family, which checks the signature.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

/// Stores a render in the asset store and answers its hash, as a render op does.
#[derive(Serialize, Deserialize, JsonSchema)]
struct StoreRender {}

impl Op for StoreRender {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.store_render",
            summary: "Store a render in the asset store.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let store = ctx.service::<dyn Store>()?;
        let hash = store.put(PNG, sample_provenance(Commercial::Ok)).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(json!({ "hash": hash.to_string() })))
    }
}
register_op!(StoreRender);

#[derive(Debug)]
struct Numbers;

impl MetricsSource for Numbers {
    fn snapshot(&self) -> Result<Map<String, Value>, OpError> {
        Ok(Map::from_iter([("fps".to_owned(), json!(60.0))]))
    }
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
    }
}

fn options() -> AppOptions {
    AppOptions::studio(Actor::Person { name: "pat".into() })
}

/// The provider type names in `options`' debug text, in the order they were registered.
fn providers(options: &McpOptions) -> Vec<&'static str> {
    let text = format!("{options:?}");
    let mut found: Vec<(usize, &'static str)> =
        ["RenderProvider", "MeasureProvider", "MetricsProvider", "LibraryProvider"]
            .into_iter()
            .filter_map(|name| text.find(name).map(|at| (at, name)))
            .collect();
    found.sort_unstable();
    found.into_iter().map(|(_, name)| name).collect()
}

#[test]
fn each_family_is_registered_when_its_source_is_there_in_a_fixed_order() {
    let all = ServicesBuilder::new()
        .insert::<dyn Store>(Arc::new(MemStore::new()))
        .insert::<dyn MetricsSource>(Arc::new(Numbers))
        .insert::<dyn Library>(Arc::new(MockLibrary::new()))
        .build();
    let grants = Grants::agent_default().with(Scope::Publish);
    let base = || McpOptions::with_grants(grants.clone());
    assert_eq!(
        providers(&with_resource_providers(base(), &all)),
        ["RenderProvider", "MeasureProvider", "MetricsProvider", "LibraryProvider"]
    );
    let store_only = ServicesBuilder::new().insert::<dyn Store>(Arc::new(MemStore::new())).build();
    assert_eq!(providers(&with_resource_providers(base(), &store_only)), ["RenderProvider", "MeasureProvider"]);
    let none = with_resource_providers(base(), &Services::none());
    assert_eq!(providers(&none), ["MeasureProvider"]);
    // The base's grants are kept.
    assert_eq!(none.grants, grants);
}

#[test]
fn an_op_reaches_the_asset_store_in_the_data_folder_and_an_agent_reads_the_render() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let core = AppCore::create(tmp.path().join("project"), "demo", options().with_data_dir(&data)).unwrap();
    assert!(!data.join("store").exists(), "the store opens on first use");

    let stored = core.run("test.store_render", json!({})).unwrap().plan.output.unwrap();
    let hash = stored["hash"].as_str().unwrap().to_owned();
    assert!(data.join("store").join("objects").is_dir(), "the person's asset store holds it");

    let link = core.connect_in_app(InAppSpec::assistant(Actor::Person { name: "pat".into() }, None)).unwrap();
    let client = core
        .runtime()
        .block_on(Client.serve_with_lifecycle(link.client_io, ClientLifecycleMode::Initialize))
        .expect("the assistant connects");
    let templates = core.runtime().block_on(client.list_all_resource_templates()).unwrap();
    let names: Vec<&str> = templates.iter().map(|t| t.uri_template.as_str()).collect();
    assert!(names.iter().any(|t| t.starts_with("omuzaki://render/")), "{names:?}");
    assert!(names.iter().any(|t| t.starts_with("omuzaki://measure/")), "{names:?}");

    let uri = format!("omuzaki://render/{hash}.png");
    let read = core.runtime().block_on(client.read_resource(ReadResourceRequestParams::new(uri))).unwrap();
    match &read.contents[0] {
        ResourceContents::BlobResourceContents { mime_type, .. } => assert_eq!(mime_type.as_deref(), Some("image/png")),
        other => panic!("expected a blob, got {other:?}"),
    }
    core.runtime().block_on(client.cancel()).unwrap();
    core.close().unwrap();
}

#[test]
fn without_a_data_folder_an_op_that_needs_the_store_says_which_service_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let error = core.run("test.store_render", json!({})).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unavailable, "{error}");
    assert!(error.to_string().contains("Store"), "{error}");
    core.close().unwrap();
}

#[test]
fn a_store_the_host_brings_wins_over_the_data_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let host = Arc::new(MemStore::new());
    let services = ServicesBuilder::new().insert::<dyn Store>(host.clone()).build();
    let options = options().with_data_dir(&data).with_services(services);
    let core = AppCore::create(tmp.path().join("project"), "demo", options).unwrap();
    core.run("test.store_render", json!({})).unwrap();
    assert_eq!(host.len(), 1);
    assert!(!data.join("store").exists());
    core.close().unwrap();
}
