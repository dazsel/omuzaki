//! The show and graph documents (#730): the worked example of design §4 as files, round-tripped byte for byte through
//! the document tree; feature keys kept and misspelt keys refused (design D21); every structural check with a failing
//! show; and property tests of the round trip and the ids.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_doc_api::{DocRead, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_live_api::graph::{GraphDef, NodeDef, Wire};
use omuzaki_live_api::ids::{GraphId, GroupId, NodeId, NodeKindId, PortName, PortRef, ShowId, VarName};
use omuzaki_live_api::limits::{MAX_GRAPHS, MAX_NODES_PER_GRAPH, MAX_PARAMS_BYTES, MAX_PORTS_PER_NODE, MAX_VARIABLES};
use omuzaki_live_api::show::{Finding, GroupDef, ShowDef, VarScope, VarType, VariableDef, paths};
use proptest::prelude::*;
use serde_json::json;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/main")
}

/// A folder as the store reads it: a folder is a table of its entries, a `<key>.toml` file a table at `<key>`.
fn read_tree(dir: &Path) -> Value {
    let mut map = serde_json::Map::new();
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if path.is_dir() {
            map.insert(name, read_tree(&path));
        } else {
            let key = name.strip_suffix(".toml").unwrap().to_owned();
            map.insert(key, toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap());
        }
    }
    Value::Object(map)
}

/// The files the store writes for a tree (relative path, text): a table holding a plain value is a file, a table of
/// tables a folder.
fn write_tree(value: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    let Value::Object(map) = value else { panic!("{prefix} is a table") };
    for (key, child) in map {
        let path = format!("{prefix}{key}");
        match child {
            Value::Object(inner) if inner.values().all(Value::is_object) => write_tree(child, &format!("{path}/"), out),
            _ => {
                out.insert(format!("{path}.toml"), toml::to_string(child).unwrap());
            }
        }
    }
}

fn fixture_files() -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if path.is_dir() {
                walk(&path, &format!("{prefix}{name}/"), out);
            } else {
                out.insert(format!("{prefix}{name}"), fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&fixtures(), "", &mut out);
    out
}

fn main_show() -> (MemDoc, ShowId) {
    let id = ShowId::new("main").unwrap();
    (MemDoc::from_value(json!({"shows": {"main": read_tree(&fixtures())}})), id)
}

#[test]
fn the_worked_example_loads_and_writes_back_byte_identically() {
    let (doc, id) = main_show();
    let node = doc.get(&paths::show(&id)).unwrap();
    let show = ShowDef::from_node(node).unwrap();
    assert_eq!(show.name, "Main");
    assert_eq!(show.graphs.len(), 2);
    let bonk = &show.graphs[&GraphId::new("bonk").unwrap()];
    assert_eq!(bonk.group, Some(GroupId::new("redeems").unwrap()));
    assert_eq!(bonk.nodes.len(), 6);
    let say = &bonk.nodes[&NodeId::new("say").unwrap()];
    assert_eq!(say.inputs[&PortName::from_static("text")], Wire::One("line.text".parse().unwrap()));
    assert!(show.features.is_empty(), "applied is a sibling key, not a feature key");
    assert_eq!(show.check(), Vec::<Finding>::new());

    let mut written = node.clone();
    show.to_node(&mut written).unwrap();
    assert_eq!(&written, node);
    let mut files = BTreeMap::new();
    write_tree(&written, "", &mut files);
    assert_eq!(files, fixture_files());
}

#[test]
fn reading_a_graph_by_its_path_gives_the_same_graph() {
    let (doc, id) = main_show();
    let lurk = GraphId::new("lurk").unwrap();
    let graph = GraphDef::from_node(doc.get(&paths::graph(&id, &lurk)).unwrap()).unwrap();
    let cmd = doc.get(&paths::node(&id, &lurk, &NodeId::new("cmd").unwrap())).unwrap();
    assert_eq!(graph.nodes[&NodeId::new("cmd").unwrap()], serde_json::from_value(cmd.clone()).unwrap());
    assert_eq!(doc.get(&paths::graph_settings(&id, &lurk)).unwrap(), &json!({"name": "Lurk"}));
}

#[test]
fn feature_keys_are_kept_byte_identically_and_reported_unchecked() {
    let (doc, id) = main_show();
    let mut node = doc.get(&paths::show(&id)).unwrap().clone();
    let map = node.as_object_mut().unwrap();
    map.insert("overlay".into(), json!({"layouts": {"main": {"w": 1920, "h": 1080, "scale": 0.5}}}));
    map.insert("plugin-x-layout".into(), json!({"slots": [1, 2, 3], "note": "ことり ✨"}));
    let show = ShowDef::from_node(&node).unwrap();
    assert_eq!(show.features.keys().collect::<Vec<_>>(), ["overlay", "plugin-x-layout"]);
    let mut written = node.clone();
    show.to_node(&mut written).unwrap();
    assert_eq!(serde_json::to_string(&written).unwrap(), serde_json::to_string(&node).unwrap());

    let checked = show.check();
    let notes: Vec<(Option<&str>, &str, bool, bool)> =
        checked.iter().map(|f| (f.key.as_deref(), f.text.id.as_ref(), f.inert, f.refuses)).collect();
    assert_eq!(
        notes,
        [
            (Some("overlay"), "live-api-key-kept-unchecked", false, false),
            (Some("plugin-x-layout"), "live-api-key-kept-unchecked", false, false)
        ]
    );
}

/// The owner's check of `live-api-probe`, a key this test binary registers as its owner would.
fn probe_check(value: &Value) -> Vec<Finding> {
    if value.is_object() { Vec::new() } else { vec![Finding::note(omuzaki_live_api::l10n::params_not_object())] }
}

omuzaki_live_api::register_show_key!("live-api-probe", check = probe_check);

#[test]
fn a_registered_key_is_checked_by_its_owner() {
    use omuzaki_live_api::show::keys::{self, KeyKind};
    assert!(keys::registered().contains("live-api-probe") && keys::refused().is_empty());
    assert!(matches!(keys::classify("live-api-probe"), Ok(KeyKind::Registered(_))));
    let node = json!({"show": {"name": "x"}, "live-api-probe": 3});
    let show = ShowDef::from_node(&node).unwrap();
    let checked = show.check();
    assert_eq!(checked.len(), 1);
    assert_eq!(
        (checked[0].key.as_deref(), checked[0].text.id.as_ref()),
        (Some("live-api-probe"), "live-api-params-not-object")
    );
    let fine = ShowDef::from_node(&json!({"show": {"name": "x"}, "live-api-probe": {"a": 1}})).unwrap();
    assert_eq!(fine.check(), []);
}

#[test]
fn a_misspelt_key_names_the_one_it_resembles() {
    let (doc, id) = main_show();
    for (wrong, right) in
        [("automatons", "automations"), ("automation", "automations"), ("shw", "show"), ("aplied", "applied")]
    {
        let mut node = doc.get(&paths::show(&id)).unwrap().clone();
        let map = node.as_object_mut().unwrap();
        let moved = map.remove(right).unwrap();
        map.insert(wrong.into(), moved);
        let error = ShowDef::from_node(&node).unwrap_err().to_string();
        assert!(error.contains(&format!("`{wrong}`")) && error.contains(&format!("`{right}`")), "{error}");
    }
    let error = ShowDef::from_node(&json!({"show": {"name": "x"}, "Overlay": {}})).unwrap_err().to_string();
    assert!(error.contains("Overlay"), "{error}");
}

fn kind(id: &'static str) -> NodeKindId {
    NodeKindId::from_static(id)
}

fn port(name: &'static str) -> PortName {
    PortName::from_static(name)
}

fn graph_id(s: &str) -> GraphId {
    GraphId::new(s).unwrap()
}

fn node_id(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}

/// The message ids of a show's findings, with what they name.
fn findings(show: &ShowDef) -> Vec<(String, Option<String>, Option<String>, bool)> {
    show.check()
        .into_iter()
        .map(|f| (f.text.id.to_string(), f.graph.map(|g| g.to_string()), f.node.map(|n| n.to_string()), f.refuses))
        .collect()
}

fn one_node_show(node: NodeDef) -> ShowDef {
    ShowDef::new("Test").with_graph(
        graph_id("g"),
        GraphDef::new().with_node(node_id("a"), NodeDef::new(kind("flow.wait"))).with_node(node_id("b"), node),
    )
}

#[test]
fn each_structural_rule_has_a_failing_show() {
    let named = |id: &str| (id.to_owned(), Some("g".to_owned()), Some("b".to_owned()), true);

    let wire_to_nowhere =
        NodeDef::new(kind("flow.wait")).with_input(port("in"), Wire::One("ghost.out".parse().unwrap()));
    assert_eq!(findings(&one_node_show(wire_to_nowhere)), [named("live-api-wire-missing-node")]);

    let empty_list = NodeDef::new(kind("flow.wait")).with_input(port("in"), Wire::Many(Vec::new()));
    assert_eq!(findings(&one_node_show(empty_list)), [named("live-api-wire-empty")]);

    let wired_and_set = NodeDef::new(kind("flow.wait"))
        .with_input(port("seconds"), Wire::One("a.value".parse().unwrap()))
        .with_set(port("seconds"), json!(2));
    assert_eq!(findings(&one_node_show(wired_and_set)), [named("live-api-port-wired-and-set")]);

    let ports = (0..=MAX_PORTS_PER_NODE).fold(NodeDef::new(kind("text.format")), |node, i| {
        node.with_set(PortName::new(format!("p{i}")).unwrap(), json!(i))
    });
    assert_eq!(findings(&one_node_show(ports)), [named("live-api-too-many-ports")]);

    let big = NodeDef::new(kind("text.format")).with_param("template", json!("x".repeat(MAX_PARAMS_BYTES)));
    assert_eq!(findings(&one_node_show(big)), [named("live-api-params-too-large")]);

    let ungrouped =
        ShowDef::new("Test").with_graph(graph_id("g"), GraphDef::new().with_group(GroupId::new("nope").unwrap()));
    assert_eq!(findings(&ungrouped), [("live-api-graph-unknown-group".to_owned(), Some("g".to_owned()), None, true)]);
    let grouped = ungrouped.clone().with_group(GroupId::new("nope").unwrap(), GroupDef::new());
    assert_eq!(findings(&grouped), []);

    let bad_default = ShowDef::new("Test").with_variable(
        VarName::new("n").unwrap(),
        VariableDef::new(VarType::Number, VarScope::Show).with_default(json!("0")),
    );
    assert_eq!(findings(&bad_default), [("live-api-variable-default-type".to_owned(), None, None, true)]);

    let too_many_nodes = (0..=MAX_NODES_PER_GRAPH)
        .fold(GraphDef::new(), |graph, i| graph.with_node(node_id(&format!("n{i}")), NodeDef::new(kind("flow.wait"))));
    let show = ShowDef::new("Test").with_graph(graph_id("g"), too_many_nodes);
    assert_eq!(findings(&show), [("live-api-too-many-nodes".to_owned(), Some("g".to_owned()), None, true)]);

    let too_many_graphs = (0..=MAX_GRAPHS)
        .fold(ShowDef::new("Test"), |show, i| show.with_graph(graph_id(&format!("g{i}")), GraphDef::new()));
    assert_eq!(findings(&too_many_graphs), [("live-api-too-many-graphs".to_owned(), None, None, true)]);

    let too_many_variables = (0..=MAX_VARIABLES).fold(ShowDef::new("Test"), |show, i| {
        show.with_variable(VarName::new(format!("v{i}")).unwrap(), VariableDef::new(VarType::Bool, VarScope::Session))
    });
    assert_eq!(findings(&too_many_variables), [("live-api-too-many-variables".to_owned(), None, None, true)]);
}

#[test]
fn a_finding_serialises_without_the_fields_it_leaves_empty() {
    let show =
        ShowDef::new("Test").with_graph(graph_id("g"), GraphDef::new().with_group(GroupId::new("nope").unwrap()));
    let finding = serde_json::to_value(&show.check()[0]).unwrap();
    assert_eq!(finding["graph"], json!("g"));
    assert!(finding.get("node").is_none() && finding.get("key").is_none());
    assert_eq!(serde_json::from_value::<Finding>(finding).unwrap(), show.check()[0]);
}

// ── Property tests ─────────────────────────────────────────────────────────────────────────────────────────────

fn slug() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,10}"
        .prop_filter("not a Windows device name", |s| !["con", "prn", "aux", "nul"].contains(&s.as_str()))
}

fn json_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::from),
        any::<i32>().prop_map(Value::from),
        (-1.0e6..1.0e6_f64).prop_map(Value::from),
        "\\PC{0,12}".prop_map(Value::from),
    ]
}

fn node_def(sources: Vec<String>) -> impl Strategy<Value = NodeDef> {
    let wire = proptest::sample::select(sources).prop_flat_map(|n| {
        prop_oneof![
            Just(Wire::One(format!("{n}.out").parse().unwrap())),
            Just(Wire::Many(vec![format!("{n}.fired").parse().unwrap(), format!("{n}.out").parse().unwrap()])),
        ]
    });
    (
        proptest::sample::select(vec!["flow.wait", "op.run", "on.event", "text.format"]),
        proptest::option::of("\\PC{0,12}"),
        (-4000..4000_i32, -4000..4000_i32),
        proptest::collection::btree_map("[a-z][a-z_]{0,6}", json_scalar(), 0..3),
        proptest::collection::btree_map("[a-z][a-z_]{0,6}", wire, 0..3),
        proptest::collection::btree_map("[a-z][a-z_]{0,6}", json_scalar(), 0..3),
    )
        .prop_map(|(kind, label, (x, y), params, inputs, set)| {
            #[allow(clippy::cast_precision_loss)] // whole and half points: exact in f32
            let pos = [x as f32 / 2.0, y as f32 / 2.0];
            let mut node = NodeDef::new(NodeKindId::new(kind).unwrap()).with_pos(pos);
            node.label = label;
            node.params = params.into_iter().collect();
            for (port, wire) in inputs {
                node = node.with_input(PortName::new(port).unwrap(), wire);
            }
            for (port, value) in set {
                node = node.with_set(PortName::new(format!("c_{port}")).unwrap(), value);
            }
            node
        })
}

fn graph_def() -> impl Strategy<Value = GraphDef> {
    proptest::collection::btree_set(slug(), 0..4).prop_flat_map(|ids| {
        let ids: Vec<String> = ids.into_iter().collect();
        let sources = if ids.is_empty() { vec!["x".to_owned()] } else { ids.clone() };
        let nodes = proptest::collection::vec(node_def(sources), ids.len());
        (
            proptest::option::of("\\PC{0,12}"),
            any::<bool>(),
            proptest::option::of(slug()),
            proptest::option::of("\\PC{0,20}"),
            nodes,
        )
            .prop_map(move |(name, enabled, group, summary, nodes)| {
                let mut graph = GraphDef::new().with_enabled(enabled);
                (graph.name, graph.summary) = (name, summary);
                graph.group = group.map(|g| GroupId::new(g).unwrap());
                for (id, node) in ids.iter().zip(nodes) {
                    graph = graph.with_node(NodeId::new(id.as_str()).unwrap(), node);
                }
                graph
            })
    })
}

fn variable_def() -> impl Strategy<Value = VariableDef> {
    (
        proptest::sample::select(vec![VarType::Bool, VarType::Number, VarType::Text, VarType::List]),
        proptest::sample::select(vec![VarScope::Session, VarScope::Show, VarScope::Global]),
        any::<bool>(),
        proptest::option::of("\\PC{0,20}"),
    )
        .prop_map(|(ty, scope, with_default, summary)| {
            let mut def = VariableDef::new(ty, scope);
            def.default = with_default.then(|| ty.default_json());
            def.summary = summary;
            def
        })
}

fn show_def() -> impl Strategy<Value = ShowDef> {
    (
        "\\PC{1,16}",
        proptest::option::of(any::<u32>()),
        proptest::collection::btree_map(slug(), variable_def(), 0..3),
        proptest::collection::btree_map(slug(), (proptest::option::of("\\PC{0,12}"), any::<bool>()), 0..3),
        proptest::collection::btree_map(slug(), graph_def(), 0..3),
        proptest::collection::btree_map(
            proptest::sample::select(vec!["overlay", "outputs", "plugin-x-layout"]),
            json_scalar(),
            0..2,
        ),
    )
        .prop_map(|(name, seed, variables, groups, graphs, features)| {
            let mut show = ShowDef::new(name);
            show.seed = seed;
            for (id, def) in variables {
                show = show.with_variable(VarName::new(id).unwrap(), def);
            }
            for (id, (name, enabled)) in groups {
                let mut group = GroupDef::new().with_enabled(enabled);
                group.name = name;
                show = show.with_group(GroupId::new(id).unwrap(), group);
            }
            for (id, graph) in graphs {
                show = show.with_graph(GraphId::new(id).unwrap(), graph);
            }
            show.features = features.into_iter().map(|(k, v)| (k.to_owned(), json!({ "v": v }))).collect();
            show
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_show_round_trips_through_its_node(show in show_def()) {
        let mut node = json!({"applied": {"x": {"item": "kept"}}});
        show.to_node(&mut node).unwrap();
        prop_assert_eq!(&node["applied"], &json!({"x": {"item": "kept"}}));
        prop_assert_eq!(ShowDef::from_node(&node).unwrap(), show);
    }

    #[test]
    fn a_graph_round_trips_through_its_node_and_as_an_argument(graph in graph_def()) {
        let mut node = Value::Null;
        graph.to_node(&mut node).unwrap();
        prop_assert_eq!(&GraphDef::from_node(&node).unwrap(), &graph);
        prop_assert_eq!(serde_json::from_value::<GraphDef>(serde_json::to_value(&graph).unwrap()).unwrap(), graph);
    }

    #[test]
    fn ids_refuse_dots_upper_case_and_long_names(stem in "[a-z][a-z0-9]{0,20}", upper in "[A-Z]") {
        let dotted = format!("{stem}.x");
        prop_assert!(ShowId::new(dotted.as_str()).is_err() && NodeId::new(dotted.as_str()).is_err());
        prop_assert!(PortName::new(dotted.as_str()).is_err());
        let shouting = format!("{stem}{upper}");
        prop_assert!(ShowId::new(shouting.as_str()).is_err() && PortName::new(shouting.as_str()).is_err());
        let (upper_kind, long_port, long_kind) =
            (format!("{stem}.{upper}"), format!("{stem}{}", "x".repeat(32)), format!("{stem}.{}", "y".repeat(96)));
        prop_assert!(NodeKindId::new(upper_kind).is_err());
        prop_assert!(GraphId::new(stem.repeat(70)).is_err());
        prop_assert!(PortName::new(long_port).is_err() && NodeKindId::new(long_kind).is_err());
    }

    #[test]
    fn a_port_ref_parses_what_it_prints(node in slug(), port in "[a-z][a-z0-9_]{0,31}") {
        let text = format!("{node}.{port}");
        let parsed: PortRef = text.parse().unwrap();
        prop_assert_eq!(parsed.to_string(), text.clone());
        prop_assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(text));
    }
}
