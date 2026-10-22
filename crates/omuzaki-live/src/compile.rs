//! Compiling a [`ShowDef`] against the node catalogue and this machine's approvals (design D5, D11, D18): every node
//! gets its ports, the event kinds it listens to, its needs and a fresh instance; every wire is resolved to node
//! indices and checked; and a node that can't run is kept **inert** with a reason and a finding, so one bad node
//! never stops the show and a show edited by hand on disk loads instead of failing.
//!
//! A node is inert when its kind isn't in this build, its parameters no longer pass, its wiring breaks a rule the
//! document ops would refuse (a missing node or port, a type that never converts, two sources on a value input, an
//! event wired to a value, a value cycle), a variable or group it names isn't declared with that type, it listens to
//! an event kind nobody registered, or a need isn't approved for this show on this machine.
//!
//! Nodes are numbered in (graph id, node id) order, so ascending index *is* the ordering the engine's rules use:
//! triggers run in ascending (graph, node), and a pulse goes to its destinations in ascending (node, port). Nothing
//! here iterates a hash map, so the same inputs compile to the same output.
//!
//! **Hot reload** (D5): compiled against the previous run, each node whose kind, parameters and wiring are unchanged
//! is listed in [`Compiled::keep`], so the engine carries its instance (and its state) across.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use omuzaki_l10n_api::Text;
use omuzaki_live_api::event::find_event_kind;
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, NodeId, PortName, ShowId, VarName};
use omuzaki_live_api::need::{Need, NetScheme};
use omuzaki_live_api::node::{DynNodeKind, Node, NodeClass, ParamErrors, Ports, find_node_kind};
use omuzaki_live_api::services::{ApprovedProgram, InertReason, ShowApprovals};
use omuzaki_live_api::show::{Finding, ShowDef};
use omuzaki_live_api::value::{Coercion, PortType, Val};
use omuzaki_ops_api::Scope;
use omuzaki_store_api::ContentHash;
use serde_json::{Value, json};

use crate::core::english;
use crate::l10n;

/// Hashes a program or script file for the program rule (design D11): an approved program runs only while its file
/// still hashes as it did when the person approved it.
pub trait ProgramHasher: Send + Sync {
    /// The BLAKE3 hash of the file at `path`, or `None` when it can't be read.
    fn hash(&self, path: &str) -> Option<ContentHash>;
}

/// [`ProgramHasher`] over the real file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileHasher;

impl ProgramHasher for FileHasher {
    fn hash(&self, path: &str) -> Option<ContentHash> {
        // The program rule hashes the exact file the person approved; nothing else is read.
        #[allow(clippy::disallowed_methods)]
        let bytes = std::fs::read(path).ok()?;
        Some(ContentHash::of(&bytes))
    }
}

/// What arms nodes on this machine: the show's approvals and how program files are hashed (design D11).
#[derive(Clone, Copy)]
pub(crate) struct Arming<'a> {
    pub approvals: &'a ShowApprovals,
    pub hasher: &'a dyn ProgramHasher,
}

impl Arming<'_> {
    /// Nothing approved: what a show gets before a person looks at it, and what tests that need nothing use.
    pub(crate) fn none() -> Arming<'static> {
        static NONE: std::sync::LazyLock<ShowApprovals> = std::sync::LazyLock::new(ShowApprovals::default);
        Arming { approvals: &NONE, hasher: &FileHasher }
    }

    /// Whether `need` is met. Declarations are checked against the show elsewhere.
    fn met(&self, need: &Need) -> bool {
        let granted = &self.approvals.granted;
        match need {
            Need::Scope(scope) => matches!(scope, Scope::Read | Scope::Perform) || granted.contains(need),
            Need::File { path, .. } => granted.contains(need) || self.approvals.roots.iter().any(|r| inside(path, r)),
            Need::Program(invocation) => {
                self.approvals.programs.iter().any(|p| &p.invocation == invocation && self.current(p))
            }
            Need::Variable { .. } | Need::Group { .. } => true,
            _ => granted.contains(need),
        }
    }

    /// Whether an approved program's files still hash as approved.
    fn current(&self, program: &ApprovedProgram) -> bool {
        let invocation = &program.invocation;
        if self.hasher.hash(&invocation.program) != Some(program.program_hash) {
            return false;
        }
        match (&invocation.script, &program.script_hash) {
            (None, None) => true,
            (Some(script), Some(hash)) => self.hasher.hash(script).as_ref() == Some(hash),
            _ => false,
        }
    }
}

/// Whether the literal `path` is inside the folder `root`, by whole components and with no `..` anywhere.
fn inside(path: &str, root: &str) -> bool {
    let (path, root) = (Path::new(path), Path::new(root));
    let plain = |p: &Path| p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir));
    plain(path) && plain(root) && path.starts_with(root)
}

/// One node as the engine runs it: everything but the instance, which the engine holds apart so a node can be
/// borrowed mutably while the rest of the show is read.
#[derive(Debug)]
pub(crate) struct CompiledNode {
    pub graph: GraphId,
    pub id: NodeId,
    pub ports: Ports,
    pub class: NodeClass,
    /// Event kinds a trigger listens to.
    pub listens: Vec<EventKind>,
    /// What running it needs (design D11), from its parameters.
    pub needs: Vec<Need>,
    /// The variables it reads or writes, from its needs: a global whose type another show settled makes it inert.
    pub vars: Vec<VarName>,
    /// Each wired value input's source: (node index, output port).
    pub value_wires: BTreeMap<PortName, (usize, PortName)>,
    /// Constants for unwired value inputs.
    pub set: BTreeMap<PortName, Value>,
    /// Each event output's destinations, ascending (node index, input port).
    pub outs: BTreeMap<PortName, Vec<(usize, PortName)>>,
    /// Why it can't run, if it can't.
    pub inert: Option<InertReason>,
    /// Whether its graph runs: enabled and its group on (design D12).
    pub active: bool,
    /// Its kind, parameters and wiring both ways, as one string: equal signatures keep the instance on hot reload.
    pub signature: String,
}

/// A compiled show: the nodes, their instances (`None` for an inert node), the trigger index and the findings.
pub(crate) struct Compiled {
    pub show: ShowId,
    pub def: ShowDef,
    pub nodes: Vec<CompiledNode>,
    pub instances: Vec<Option<Box<dyn Node>>>,
    /// For each event kind, the triggers listening to it, ascending.
    pub triggers: BTreeMap<EventKind, Vec<usize>>,
    /// What the structural check and the catalogue checks found, in (graph, node) order after the structural ones.
    pub findings: Vec<Finding>,
    /// Per node, the index in the previous compile of an instance to carry over unchanged (design D5's hot reload).
    pub keep: Vec<Option<usize>>,
}

/// Whether a graph runs at start: enabled, and in no group or in a group that is on.
fn graph_active(def: &ShowDef, graph: &GraphDef) -> bool {
    graph.enabled && graph.group.as_ref().is_none_or(|g| def.groups.get(g).is_none_or(|group| group.enabled))
}

/// `errors` in English, for a finding.
fn reasons(errors: &ParamErrors) -> String {
    let texts: Vec<String> = errors.0.iter().map(|e| english(&e.text)).collect();
    if texts.is_empty() { errors.to_string() } else { texts.join("; ") }
}

/// Whether anything produces events of `kind`: a registered kind, or a show's own `custom.<name>`.
fn known_event(kind: &EventKind) -> bool {
    kind.as_str().starts_with("custom.") || find_event_kind(kind.as_str()).is_some()
}

/// A need as people read it on a finding (`https://example.com:443`, `the op obs.scene.set`).
fn need_text(need: &Need) -> String {
    match need {
        Need::Scope(scope) => format!("the {scope} scope"),
        Need::Op { op, .. } => format!("the op {op}"),
        Need::Host { scheme, host, port, .. } => {
            let scheme = match scheme {
                NetScheme::Http => "http",
                NetScheme::Https => "https",
                _ => "udp",
            };
            format!("{scheme}://{host}:{port}")
        }
        Need::File { path, write: true, .. } => format!("writing {path}"),
        Need::File { path, write: false, .. } => format!("reading {path}"),
        Need::Program(invocation) => format!("running {}", invocation.program),
        Need::Secret { name, host, .. } => format!("the secret {name} for {host}"),
        Need::Process { name, .. } => format!("asking whether {name} runs"),
        Need::Variable { name, .. } => format!("the variable {name}"),
        Need::Group { group, .. } => format!("the group {group}"),
        _ => "something this build doesn't describe".to_owned(),
    }
}

/// A node being compiled: what the catalogue said, before the wires are checked.
struct Built {
    class: NodeClass,
    ports: Ports,
    listens: Vec<EventKind>,
    needs: Vec<Need>,
    instance: Option<Box<dyn Node>>,
    inert: Option<(InertReason, Text)>,
}

/// Asks the catalogue about one node.
fn build(graph: &GraphId, id: &NodeId, kind_id: &str, params: &Value) -> Built {
    let inert = |reason: InertReason, text: Text| Built {
        class: NodeClass::Active,
        ports: Ports::new(),
        listens: Vec::new(),
        needs: Vec::new(),
        instance: None,
        inert: Some((reason, text)),
    };
    let Some(kind) = find_node_kind(kind_id) else {
        return inert(InertReason::UnknownKind, l10n::node_unknown_kind(graph.as_str(), id.as_str(), kind_id));
    };
    let asked = (|| {
        let normal = DynNodeKind::check(kind, params)?;
        Ok::<_, ParamErrors>((
            DynNodeKind::ports(kind, &normal)?,
            kind.listens(&normal)?,
            kind.needs(&normal)?,
            kind.instantiate(&normal)?,
        ))
    })();
    match asked {
        Ok((ports, listens, needs, instance)) => {
            Built { class: DynNodeKind::meta(kind).class, ports, listens, needs, instance: Some(instance), inert: None }
        }
        Err(errors) if kind_id == "op.run" => {
            inert(InertReason::OpNotAllowed, l10n::node_op_not_allowed(graph.as_str(), id.as_str(), reasons(&errors)))
        }
        Err(errors) => {
            inert(InertReason::BadParams, l10n::node_bad_params(graph.as_str(), id.as_str(), reasons(&errors)))
        }
    }
}

/// The nodes on a value cycle, by index: each node's wired value inputs are its edges.
fn on_cycles(nodes: &[CompiledNode]) -> BTreeSet<usize> {
    // Tarjan's strongly connected components, iteratively; a component of two or more, or a node reading itself, is a
    // cycle.
    let n = nodes.len();
    let edges: Vec<Vec<usize>> =
        nodes.iter().map(|node| node.value_wires.values().map(|(src, _)| *src).collect()).collect();
    let (mut index, mut low, mut on_stack) = (vec![usize::MAX; n], vec![0; n], vec![false; n]);
    let (mut stack, mut next, mut cyclic) = (Vec::new(), 0, BTreeSet::new());
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work = vec![(root, 0usize)];
        while let Some(&mut (v, ref mut edge)) = work.last_mut() {
            if *edge == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = edges[v].get(*edge) {
                *edge += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut component = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() > 1 || edges[v].contains(&v) {
                    cyclic.extend(component);
                }
            }
        }
    }
    cyclic
}

/// Compiles `def` with nothing approved and no previous run.
pub(crate) fn compile(show: &ShowId, def: &ShowDef) -> Compiled {
    compile_with(show, def, Arming::none(), None)
}

/// Compiles `def`. Never fails: whatever can't run is inert, and the findings ride along for the caller to show (or
/// refuse to start on). With `previous`, unchanged nodes are listed in [`Compiled::keep`].
#[allow(clippy::too_many_lines)] // One pass per rule, in D18's order, reads best in one place.
pub(crate) fn compile_with(
    show: &ShowId,
    def: &ShowDef,
    arming: Arming<'_>,
    previous: Option<&[CompiledNode]>,
) -> Compiled {
    let mut findings = def.check();
    let mut index: BTreeMap<(&GraphId, &NodeId), usize> = BTreeMap::new();
    let mut nodes = Vec::new();
    let mut instances = Vec::new();
    let mut texts: Vec<Option<Text>> = Vec::new();
    for (graph_id, graph) in &def.graphs {
        let active = graph_active(def, graph);
        for (node_id, node) in &graph.nodes {
            index.insert((graph_id, node_id), nodes.len());
            let params = Value::Object(node.params.clone());
            let built = build(graph_id, node_id, node.kind.as_str(), &params);
            // A structural rule the document ops refuse, broken by hand on disk, makes the node it names inert.
            let structural = findings
                .iter()
                .any(|f| f.inert && f.graph.as_ref() == Some(graph_id) && f.node.as_ref() == Some(node_id));
            let (inert, text) = match built.inert {
                Some((reason, text)) => (Some(reason), Some(text)),
                None if structural => (Some(InertReason::BadParams), None),
                None => (None, None),
            };
            let vars = built
                .needs
                .iter()
                .filter_map(|need| match need {
                    Need::Variable { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect();
            nodes.push(CompiledNode {
                graph: graph_id.clone(),
                id: node_id.clone(),
                ports: built.ports,
                class: built.class,
                listens: built.listens,
                needs: built.needs,
                vars,
                value_wires: BTreeMap::new(),
                set: node.set.clone(),
                outs: BTreeMap::new(),
                inert,
                active,
                signature: String::new(),
            });
            instances.push(built.instance);
            texts.push(text);
        }
    }

    // Wires are written at their destination; resolve each to indices, check it, and file event wires under their
    // source. A wire to a node that isn't there is the structural check's finding; one into an inert node is skipped.
    let mut event_wires: Vec<(usize, PortName, usize, PortName)> = Vec::new();
    for (graph_id, graph) in &def.graphs {
        for (node_id, node) in &graph.nodes {
            let Some(&dest) = index.get(&(graph_id, node_id)) else { continue };
            if nodes[dest].inert.is_some() {
                continue;
            }
            let mut bad: Option<Text> = None;
            for (port, wire) in &node.inputs {
                let Some(input) = nodes[dest].ports.input(port.as_str()).cloned() else {
                    bad.get_or_insert(l10n::wire_no_input(graph_id.as_str(), node_id.as_str(), port.as_str()));
                    continue;
                };
                if !input.ty.is_event() && wire.sources().len() > 1 {
                    bad.get_or_insert(l10n::wire_two_sources(graph_id.as_str(), node_id.as_str(), port.as_str()));
                    continue;
                }
                for source in wire.sources() {
                    let Some(&src) = index.get(&(graph_id, &source.node)) else { continue };
                    if nodes[src].inert.is_some() {
                        // Its ports are unknown; reading it gives the input's default, and it never fires.
                        continue;
                    }
                    let Some(output) = nodes[src].ports.output(source.port.as_str()).cloned() else {
                        bad.get_or_insert(l10n::wire_no_output(
                            graph_id.as_str(),
                            node_id.as_str(),
                            port.as_str(),
                            source.to_string(),
                        ));
                        continue;
                    };
                    if PortType::coercion(output.ty, input.ty) == Coercion::Refused {
                        bad.get_or_insert(l10n::wire_type(
                            graph_id.as_str(),
                            node_id.as_str(),
                            port.as_str(),
                            output.ty.as_str(),
                            input.ty.as_str(),
                        ));
                        continue;
                    }
                    if input.ty.is_event() {
                        event_wires.push((src, source.port.clone(), dest, port.clone()));
                    } else {
                        nodes[dest].value_wires.insert(port.clone(), (src, source.port.clone()));
                    }
                }
            }
            for (port, constant) in &node.set {
                let fits = nodes[dest].ports.input(port.as_str()).is_some_and(|input| {
                    !input.ty.is_event()
                        && Val::from_json(constant, input.ty).is_ok_and(|v| v.ty() == input.ty && v.check().is_ok())
                });
                if !fits {
                    bad.get_or_insert(l10n::set_unfit(graph_id.as_str(), node_id.as_str(), port.as_str()));
                }
            }
            if let Some(text) = bad {
                nodes[dest].inert = Some(InertReason::BadWiring);
                texts[dest] = Some(text);
            }
        }
    }
    for i in on_cycles(&nodes) {
        if nodes[i].inert.is_none() {
            nodes[i].inert = Some(InertReason::BadWiring);
            texts[i] = Some(l10n::value_cycle(nodes[i].graph.as_str(), nodes[i].id.as_str()));
        }
    }

    // Declarations, event kinds and approvals (D11, D18).
    for (i, node) in nodes.iter_mut().enumerate() {
        if node.inert.is_some() {
            continue;
        }
        let (graph, id) = (node.graph.as_str(), node.id.as_str());
        let undeclared = node.needs.iter().find_map(|need| match need {
            Need::Variable { name, ty, .. } => match def.variables.get(name) {
                Some(declared) if declared.ty == *ty => None,
                Some(declared) => {
                    Some(l10n::variable_type(graph, id, name.as_str(), declared.ty.as_str(), ty.as_str()))
                }
                None => Some(l10n::undeclared(graph, id, format!("the variable {name}"))),
            },
            Need::Group { group, .. } => {
                (!def.groups.contains_key(group)).then(|| l10n::undeclared(graph, id, format!("the group {group}")))
            }
            _ => None,
        });
        if let Some(text) = undeclared {
            node.inert = Some(InertReason::Undeclared);
            texts[i] = Some(text);
            continue;
        }
        if let Some(kind) = node.listens.iter().find(|kind| !known_event(kind)) {
            node.inert = Some(InertReason::UnknownEventKind);
            texts[i] = Some(l10n::unknown_event(graph, id, kind.as_str()));
            continue;
        }
        let missing: Vec<Need> = node.needs.iter().filter(|need| !arming.met(need)).cloned().collect();
        if !missing.is_empty() {
            let list: Vec<String> = missing.iter().map(need_text).collect();
            texts[i] = Some(l10n::not_approved(graph, id, list.join(", ")));
            node.inert = Some(InertReason::not_approved(missing));
        }
    }

    // Inert nodes get no instance; each says why.
    for (i, node) in nodes.iter().enumerate() {
        if node.inert.is_some() {
            instances[i] = None;
        }
        if let Some(text) = texts[i].take() {
            findings.push(Finding::inert(text).at_node(&node.graph, &node.id));
        }
    }

    for (src, out, dest, input) in event_wires {
        if nodes[src].inert.is_none() && nodes[dest].inert.is_none() {
            nodes[src].outs.entry(out).or_default().push((dest, input));
        }
    }
    for node in &mut nodes {
        for dests in node.outs.values_mut() {
            dests.sort();
            dests.dedup();
        }
    }

    // Notes: an event input nothing is wired to, and a trigger that listens to nothing (D18, second column).
    let mut wired: BTreeSet<(usize, &PortName)> = BTreeSet::new();
    for node in &nodes {
        for (dest, port) in node.outs.values().flatten() {
            wired.insert((*dest, port));
        }
    }
    for (i, node) in nodes.iter().enumerate() {
        if node.inert.is_some() {
            continue;
        }
        let (graph, id) = (node.graph.as_str(), node.id.as_str());
        if node.class == NodeClass::Trigger && node.listens.is_empty() {
            findings.push(Finding::note(l10n::never_fires(graph, id)).at_node(&node.graph, &node.id));
        }
        let unwired: Vec<&str> = node
            .ports
            .inputs
            .iter()
            .filter(|p| p.ty.is_event() && !wired.contains(&(i, &p.name)))
            .map(|p| p.name.as_str())
            .collect();
        let event_inputs = node.ports.inputs.iter().filter(|p| p.ty.is_event()).count();
        if node.class != NodeClass::Trigger && event_inputs > 0 && unwired.len() == event_inputs {
            findings.push(Finding::note(l10n::never_runs(graph, id)).at_node(&node.graph, &node.id));
        }
    }

    let mut triggers: BTreeMap<EventKind, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if node.class == NodeClass::Trigger && node.inert.is_none() {
            for kind in &node.listens {
                triggers.entry(kind.clone()).or_default().push(i);
            }
        }
    }

    // Signatures, then what hot reload keeps.
    let signatures: Vec<String> = nodes.iter().map(|node| signature(def, &nodes, node)).collect();
    for (node, signature) in nodes.iter_mut().zip(signatures) {
        node.signature = signature;
    }
    let keep = nodes
        .iter()
        .map(|node| {
            let previous = previous?;
            let old = previous.binary_search_by(|o| (&o.graph, &o.id).cmp(&(&node.graph, &node.id))).ok()?;
            let same =
                previous[old].signature == node.signature && previous[old].inert.is_none() && node.inert.is_none();
            same.then_some(old)
        })
        .collect();
    Compiled { show: show.clone(), def: def.clone(), nodes, instances, triggers, findings, keep }
}

/// A node's kind, parameters, constants and wires in and out, by ids: what decides whether its instance survives a
/// reload. The editor position and label aren't in it, so a drag never rebuilds a node.
fn signature(def: &ShowDef, nodes: &[CompiledNode], node: &CompiledNode) -> String {
    let Some(written) = def.graphs.get(&node.graph).and_then(|g| g.nodes.get(&node.id)) else { return String::new() };
    let outs: Vec<(&str, &str, &str)> = node
        .outs
        .iter()
        .flat_map(|(out, dests)| {
            dests.iter().map(move |(d, port)| (out.as_str(), nodes[*d].id.as_str(), port.as_str()))
        })
        .collect();
    // serde_json's maps are ordered here (no `preserve_order`), so the string is canonical.
    json!({
        "kind": written.kind.as_str(),
        "params": written.params,
        "in": written.inputs,
        "set": written.set,
        "out": outs,
        "inert": node.inert,
    })
    .to_string()
}
