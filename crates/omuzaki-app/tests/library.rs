//! The library in the running app, on a real kit character: an agent over MCP finds a colourway and applies it, the
//! person sees it and undoes it in one step, applies it again, and after a restart the lock still pins it; then an
//! update to the next version keeps the person's own colour and takes the rest. A hairstyle item creates a real hair
//! part that builds, beside or in place of the kit's own.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
use omuzaki_doc_api::DocPath;
use omuzaki_library::HostLibrary;
use omuzaki_library_api::{ItemManifest, ItemScope, Library};
use omuzaki_mcp::InAppSpec;
use omuzaki_ops_api::{Actor, ServicesBuilder};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

const RIGHTS: &str = "[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n";

fn manifest(head: &str, steps: &str) -> ItemManifest {
    let text = format!("schema = 1\n{head}\norigin = \"app test\"\ntags = [\"test\"]\n{RIGHTS}\n{steps}");
    ItemManifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn palette(slot: &str, colour: &str) -> String {
    format!(
        "[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\nslot = \"{slot}\"\ncolour = \"{colour}\"\n"
    )
}

/// The night-iris colourway of design 4.5 at 1.0.0, and a 1.1.0 that retunes the mid hair and adds the trim.
fn night_iris(version: &str) -> ItemManifest {
    let head = format!(
        "id = \"omuzaki/colourways/night-iris\"\nversion = \"{version}\"\nsection = \"colourways\"\nkind = \"setting\"\n\
         target = \"character\"\nslot = \"colourway\"\ntitle = \"Night iris\"\n\
         summary = \"Deep violet and ink blue with a cool silver accent.\""
    );
    let mut steps = palette("hair_root", "#2a2346") + &palette("outfit_a", "#1d2a4a");
    steps += &palette("hair_mid", if version == "1.0.0" { "#463a78" } else { "#4f4386" });
    if version != "1.0.0" {
        steps += &palette("trim", "#c8ccd8");
    }
    manifest(&head, &steps)
}

/// The twintails hairstyle of design 4.3, on the kit's default hair generators.
fn twintails() -> ItemManifest {
    let head = "id = \"omuzaki/hair/twintails\"\nversion = \"1.0.0\"\nsection = \"hair\"\nkind = \"part\"\n\
                target = \"character\"\nslot = \"hairstyle\"\ntitle = \"Twintails\"\n\
                summary = \"Two high tails tied above the ears.\"\n\
                compat = { kinds = [\"humanoid\"], templates = [\"*\"], requires = [\"head\"] }\n\
                creates.hair = { kind = \"hair\", id = \"hair\", layer = 5 }";
    let step = "[[step]]\nop = \"part.add\"\n[step.args]\ncharacter = \"$character\"\nkind = \"hair\"\n\
                id = \"$parts.hair\"\nlayer = 5\n";
    manifest(head, step)
}

fn library(versions: &[&str]) -> Arc<HostLibrary> {
    let library = HostLibrary::empty(None);
    for v in versions {
        library.insert(night_iris(v), ItemScope::FirstParty).unwrap();
    }
    library.insert(twintails(), ItemScope::FirstParty).unwrap();
    Arc::new(library)
}

fn options(library: &Arc<HostLibrary>) -> AppOptions {
    let services = ServicesBuilder::new()
        .insert::<dyn Library>(Arc::clone(library) as Arc<dyn Library>)
        .insert(Arc::clone(library))
        .build();
    AppOptions::studio(person()).with_services(services)
}

fn colour(core: &AppCore, slot: &str) -> Option<Value> {
    core.get(&DocPath::parse(&format!("characters/mia/palette/{slot}")).unwrap())
}

fn run(core: &AppCore, op: &str, args: Value) -> Value {
    core.run(op, args).unwrap_or_else(|e| panic!("{op}: {e}")).plan.output.unwrap_or_default()
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1"))
    }
}

async fn call<H: ClientHandler>(client: &RunningService<RoleClient, H>, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().unwrap().clone();
    client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
}

/// A tool call's `output`.
fn output(result: &CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let reply = result.structured_content.clone().unwrap_or_else(|| {
        let text: String = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    });
    reply["output"].clone()
}

#[test]
fn an_agent_applies_a_colourway_the_person_undoes_it_and_the_pin_and_update_survive_a_restart() {
    omuzaki_char::link();
    let tmp = tempfile::tempdir().unwrap();
    let lib = library(&["1.0.0"]);
    let core = AppCore::create(tmp.path(), "demo", options(&lib)).unwrap();
    run(&core, "character.create", json!({"id": "mia", "name": "Mia", "template": "soft-sd"}));
    // A new kit character has only its skin slots; the person adds the ones a colourway recolours.
    for (slot, colour) in
        [("hair_root", "#5a3b2e"), ("hair_mid", "#7a5240"), ("outfit_a", "#e8e2d4"), ("trim", "#a03030")]
    {
        run(&core, "palette.add_slot", json!({"character": "mia", "slot": slot, "colour": colour}));
    }
    let kit_root = colour(&core, "hair_root");

    // The agent finds the item for this character and applies it, over the same core the person works in.
    let link = core.connect_in_app(InAppSpec::assistant(person(), None)).unwrap();
    let client = core
        .runtime()
        .block_on(Client.serve_with_lifecycle(link.client_io, ClientLifecycleMode::Initialize))
        .expect("the assistant connects");
    let found = output(&core.runtime().block_on(call(
        &client,
        "library_search",
        json!({"text": "night iris", "compatible_with": {"character": "mia"}}),
    )));
    assert_eq!(found["items"][0]["id"], "omuzaki/colourways/night-iris", "{found}");
    let applied = output(&core.runtime().block_on(call(
        &client,
        "library_apply",
        json!({"item": "omuzaki/colourways/night-iris", "character": "mia"}),
    )));
    assert_eq!(applied["application"], "colourway", "{applied}");
    assert_eq!(colour(&core, "hair_root"), Some(json!("#2a2346")), "the person sees the agent's change at once");

    // The agent's one undo takes back every palette step and the record with them.
    output(&core.runtime().block_on(call(&client, "history_undo", json!({}))));
    assert_eq!(colour(&core, "hair_root"), kit_root, "one undo is the whole apply");
    assert!(core.get(&DocPath::parse("characters/mia/applied/colourway").unwrap()).is_none(), "the record goes too");
    core.runtime().block_on(client.cancel()).unwrap();

    run(&core, "library.apply", json!({"item": "omuzaki/colourways/night-iris", "character": "mia"}));
    run(&core, "palette.set", json!({"character": "mia", "slot": "hair_mid", "colour": "#ff00aa"}));
    core.close().unwrap();

    // Reopened with a library that has 1.1.0: the lock still pins 1.0.0 and offers 1.1.0.
    let lib = library(&["1.0.0", "1.1.0"]);
    let core = AppCore::open(tmp.path(), options(&lib)).unwrap();
    let lock = run(&core, "library.lock", json!({"character": "mia"}));
    let row = &lock["applications"][0];
    assert_eq!(
        (row["version"].clone(), row["resolves"].clone(), row["newest"].clone()),
        (json!("1.0.0"), json!("ok"), json!("1.1.0")),
        "{lock}"
    );

    let updated = run(&core, "library.update", json!({"character": "mia", "application": "colourway"}));
    assert_eq!(updated["version"], "1.1.0");
    assert_eq!(colour(&core, "hair_mid"), Some(json!("#ff00aa")), "the person's own colour is kept");
    assert_eq!(colour(&core, "trim"), Some(json!("#c8ccd8")), "what only the item changed lands");
    assert_eq!(updated["conflicts"], json!(["palette/hair_mid"]), "{updated}");
    core.close().unwrap();
}

#[test]
fn a_hairstyle_item_creates_a_real_hair_part_that_builds_and_can_replace_the_kits_own() {
    omuzaki_char::link();
    let tmp = tempfile::tempdir().unwrap();
    let lib = library(&["1.0.0"]);
    let core = AppCore::create(tmp.path(), "demo", options(&lib)).unwrap();
    run(&core, "character.create", json!({"id": "mia", "name": "Mia", "template": "soft-sd"}));
    let had_hair = core.get(&DocPath::parse("characters/mia/parts/hair").unwrap()).is_some();

    let args = json!({"item": "omuzaki/hair/twintails", "character": "mia"});
    let out = run(&core, "library.apply", args.clone());
    let part = out["created"]["hair"].as_str().unwrap().to_owned();
    if had_hair {
        assert_eq!(part, "hair-2", "the kit's own hair is kept and the new part named: {out}");
        assert_eq!(out["warnings"].as_array().map(Vec::len), Some(1));
        core.undo().unwrap();
        let replaced = run(
            &core,
            "library.apply",
            json!({"item": "omuzaki/hair/twintails", "character": "mia", "replace": ["hair"]}),
        );
        assert_eq!(replaced["created"]["hair"], "hair");
    }
    let node = core.get(&DocPath::parse("characters/mia").unwrap()).unwrap();
    assert_eq!(node["sources"]["hair"]["provenance"]["source"]["item"], "omuzaki/hair/twintails@1.0.0");
    let def = CharacterDef::from_node(&node).unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mia").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let hair_vertices: usize =
        built.meshes.iter().filter(|m| m.part.as_str() == "hair").map(|m| m.mesh.geometry.positions.len()).sum();
    assert!(hair_vertices > 0, "the item's hair part builds");
    core.close().unwrap();
}
