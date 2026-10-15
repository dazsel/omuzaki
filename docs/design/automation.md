---
title: "The automation graph: shows, graphs, nodes, the event envelope and variables"
summary: "shows, graphs, nodes, the event envelope and variables"
area: design
---

# The automation graph: shows, graphs, nodes, the event envelope and variables

**Status:** proposed. **Features:** BR-11, BR-26; the event
envelope is agreed with BR-10; AP-10 and AP-11 build on it.
**Decision record:** `docs/decisions/0027-automation-graph.md`.
**Spec:** v3 §3, §6 (ops, scopes), §7 (project layout), §8 (runtime, determinism),
§9 (crates), §10 (budgets), §14 (CN-05), §22 (PF-25), §23 (AP-10, AP-11), §25 (BR-10, BR-11, BR-14, BR-19, BR-21,
BR-26), §28 (LB-14), §30 (PL-03), §31 (SE-04), §39 (rules 1, 2, 3, 6, 9).
**Builds on:** `omuzaki-ops-api` (`Runner`, `Request`, `Actor`, `Cause`, `Grants`, `OpCtx::fs_access`, `register_op!`,
`OpError`), the sessions contract (`docs/design/agent-sessions.md`, ADR 0020: `Transport::Automation`, `Sessions`,
the approval rules), ADR 0009 (determinism), ADR 0013 (file roots), ADR 0015 (registration linking), ADR 0018
(automations can't open a microphone), ADR 0019 and `docs/design/library.md` (the reserved `show` target, recipes,
records), `docs/design/l10n-ops.md` (derived `node-` ids, `messages!`, `Text`), `docs/design/scenes.md` (the pattern
for a folder-shaped document), `omuzaki_core::{time::{Clock, FakeClock}, rng::Rng}`, and the decisions on
the crate names (the crates stay `omuzaki-live*`) and the `input` and `process` scopes.

This page explains the decisions so that an implementer or a reviewer can disagree with a reason. Names in the code
blocks are binding for the contract tasks; doc comments are the implementer's.

## 1. What this covers

| Asked for | Where |
|---|---|
| Where it lives (crates, spec §9) | D1 |
| The document: shows, graphs, nodes, wires, groups, variables; text-first and diffable | D2, §4 |
| How library templates (LB-14 `automations`) apply; the library `show` target | D14 |
| The node contract: a self-registered `NodeKind`, typed ports, parameters with JSON Schema, `node-` ids, the PL-03 seam | D3, D4, D15 |
| Execution: event-driven, off the frame thread, deterministic under `FakeClock` and a seed | D5, D6 |
| A loop guard and rate limits | D8 |
| Nodes that change anything call ops through the `Runner` in an `Automation` session; a generic run-op node | D7 |
| The event envelope that adapters, hotkeys, voice commands and the engine emit; the chat-message fields | D9 |
| Variables: types, scopes, where each persists, write debouncing, what the values monitor reads | D10 |
| Risky nodes (HTTP and UDP, files, processes, run a program, secrets) and their scopes; imported shows | D11 |
| Testing: a `NodeHarness` with scripted inputs and a fake `Runner` | D16 |
| The BR-26 re-cut, BR-11's tasks, the library `show` target, AP-10 and AP-11 re-checked | §6, §7 |

**Not covered here, on purpose:**
- **BR-10's adapter contract** (sign-in, reconnects, sending limits, the platform clients) and the fields of the
  non-chat payloads beyond what D9 fixes. BR-10's design adds fields; it doesn't rename these.
- **PF-25's triggers and hotkeys table** and the OS input source. They emit `hotkey` events (D9).
- **The chat reader** (queue, voices, bus) and **the voice-command listener** (AP-10 tasks). §7 says what they
  take from this page.
- **The effects themselves:** OBS control (BR-19), throws (BR-12), the soundboard (BR-21), overlays (BR-13, BR-20),
  scene switching (ST-04) and AI lines (AP-04) are ops of those features; the graph reaches them through `op.run` (D7).
- **PL-03's implementation** (phase 5): only the seam is fixed here (D15).
- **Recording a live run** to replay it later: D6 makes it possible; nothing here builds it.
- **Collab:** each person runs their own show on their own machine; nothing about a running show is synced.

## 2. The shape of it

```
 producers (any thread)                        omuzaki-live engine: one thread per running show
 BR-10 adapters ───┐                          ┌───────────────────────────────────────────────────────┐
 hotkeys (PF-25) ──┤                          │ inbox (bounded; chat may drop) ─┐                      │
 voice (AP-10) ────┤ EventSink::publish ────► │ wakes (a timer heap on Clock) ──┼─► one step per item: │
 OBS events ───────┤ (a LiveEvent)            │ op replies ─────────────────────┘   depth-first to     │
 automation.fire ──┘ (Source::Manual)         │                                     completion; tokens │
                                              │ CompiledShow: instances, wires,     carry values; pure │
 shows/<id>/... ── compile, hot reload ─────► │ needs, inert nodes, rng streams     nodes evaluated on │
 (document ops: show.*, automation.*)         │ variables, groups, limits           read               │
                                              └──────────────┬──────────────────────────▲─────────────┘
                                                             │ op requests              │ replies
                                                dispatcher: ordered lane (edits, queries, live actions)
                                                            job lane (net.*, file.*, exec.run)
                                                             ▼                          │
                                  Runner (omuzaki-ops), in the show's Automation session ──┘
                                  actor Automation { show#n }, on behalf of the person;
                                  grants = the base ∪ what the person approved for this show on this machine
```

## 3. Decisions

### D1. The contract is `omuzaki-live-api`, the engine `omuzaki-live`, the first-party nodes `omuzaki-live-nodes`

The graph stays where spec §9 puts it (decision 1). Within that family:

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-live-api` | API | ids; the show and graph document types, `paths`, keys and `check`; `Val`, ports, `NodeKind`, `register_node!`, `NodeCx`; `LiveEvent`, `register_event_kind!`, the normalised platform payloads; `Need`; the service traits `EventSink`, `AutomationControl`, `Approvals`, `VariableStore`, `WallClock`; limits. Depends on `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-doc-api`, `omuzaki-l10n-api`, `omuzaki-store-api` (`ContentHash`) |
| `omuzaki-live-mock` | Mock | `NodeHarness`, `FakeRunner`, `FakeWall`, `MemoryVariables`, `FakeApprovals`, event fixtures (synthetic names and messages only) |
| `omuzaki-live` | Implementation | the engine (compile, the step loop, dispatch, the automation session), the inbox behind `EventSink`, the variable and approval stores on disk, the `show.*` and `automation.*` ops, `ShowHarness`, and the library source for automation templates. BR-10's adapters and BR-19's OBS client join it or become `omuzaki-live-<x>` siblings, as their designs decide |
| `omuzaki-live-nodes` | Implementation | the first-party node catalogue, one folder per family, and the ops those nodes call (`net.*`, `file.*`, `exec.run`, `system.process_running`) |

- **Why the nodes are not in the engine crate:** `lint-deps` then guarantees that a node sees only the contract (an
  implementation crate can't depend on another), so the `NodeHarness` is the only way a node is tested and no node
  can reach into the engine. Dozens of parallel node PRs don't rebuild the engine or the adapters, and heavy
  dependencies (TLS, time zones, process listing) stay out of the engine.
- **Why an effect op lives in its node's folder:** a vertical slice (§39 rule 3). `net.http` the node and `net.http`
  the op are one folder, one PR and one reviewer, and agents and scripts get the op as an MCP tool and CLI command.
- **Nodes self-register from any crate** against `omuzaki-live-api`: `omuzaki-speech` registers its `voice.command`
  event kind, a feature crate may register a friendly node over its own op. Nothing lists them.
- **Native only.** The web player runs no shows, so none of these crates needs to build for wasm.
- Spec §9's `omuzaki-live` row names the four crates (this PR).

### D2. A show is a folder, a graph is a folder, a node is a file

```
shows/<show>/show.toml                               name, seed, [variables.<name>], [groups.<id>]
shows/<show>/automations/<graph>/graph.toml          name, enabled, group, summary
shows/<show>/automations/<graph>/nodes/<node>.toml   kind, label, pos, [params], [in], [set]
shows/<show>/applied/<app>.toml                      library records (ADR 0019); the show's only sibling key
project.toml: default_show = "<show>"
```

| Key | File | Holds |
|---|---|---|
| `show` | `shows/<id>/show.toml` | `name`, optional `seed` (D6), `variables.<name>` declarations (D10), `groups.<id>` (D12) |
| `automations` | `shows/<id>/automations/<graph>/` | one folder per graph |
| `graph` (in a graph) | `.../<graph>/graph.toml` | `name`, `enabled` (default `true`), optional `group`, optional `summary` |
| `nodes` (in a graph) | `.../<graph>/nodes/<node>.toml` | one node each: `kind`, optional `label`, `pos`, `params`, `in`, `set` |
| `applied` | `shows/<id>/applied/<app>.toml` | library application records |

- **Why a file per node:** spec §7 asks for one entity per file, and a node is the unit people, agents and the editor
  change: a drag moves one node, an agent sets one parameter, a person rewires one input. Separate files merge
  cleanly, and the stale-write guard (ADR 0020 decision 5) names only real conflicts. The store's own rules give this
  layout without any special case: a node holds `kind` (a plain value), so it is a file; `nodes` holds only objects,
  so it is a folder (`omuzaki-doc` rules 1 to 3). **One file per graph was rejected:** the store writes sub-tables as
  `[nodes.<id>.params]`, so a six-node graph becomes a hundred-line file in which every drag and every edit lands, and
  two people's edits to different nodes collide in one file.
- **Wires are stored at their destination,** in the receiving node's `in` table: `in.text = "line.text"` for a value
  input (exactly one source), `in.in = ["redeem.fired", "cmd.fired"]` for an event input (one or more; a single
  string is allowed for one). Why: "one source per value input" is then structural rather than a rule to check; a wire
  needs no id of its own; a node's file says where its inputs come from; and the op that removes a node removes every
  wire naming it in the same patch (D13).
- **`params` shape a node, inputs vary per event.** Parameters decide the node's ports, ids and templates and are
  validated against its JSON Schema; value inputs are what may change from one event to the next. An unwired value
  input takes a constant from `set` (`set.seconds = 10`), else its declared default. A port named in both `in` and
  `set` is refused.
- **`pos`** is the editor position in points. A drag is one transaction coalesced into one undo step.
- **Variables and groups are declared in `show.toml`:** they are show-wide and few.
- **Ids are stable slugs** (`ShowId`, `GraphId`, `NodeId`, `GroupId`, `VarName`, all `omuzaki_core::id::Slug`, so they
  never contain `.`, which wires use as the separator). Names are display text; renaming never changes an id, because
  ids appear in wires, records and history.
- **Typed views:** `ShowDef` (`NODE_KEYS = ["show", "automations"]`, `SIBLING_KEYS = ["applied"]`) and `GraphDef`
  (`NODE_KEYS = ["graph", "nodes"]`) are read and written like `SceneDef`: `from_node` refuses an unknown key with the
  nearest valid one, `to_node` leaves sibling keys alone. Other broadcast features (outputs, overlays) add their own
  keys to `ShowDef` as additive API changes.
- **Empty collections are removed,** not saved as empty files (as for scenes). A graph with no nodes is only its
  `graph.toml`.
- **The default show** is `project.default_show`; unset or dangling, the first show by id. With no show at all, the
  Broadcast workspace creates `main` in the same transaction as the first edit, and an op called directly on a missing
  show is refused with the hint `show.create {id: "main"}` (the scenes D11 pattern). Ops take `show?`, defaulting to it.
- **Runtime state is never in the document:** variable values, runtime group switches, loop-guard suspensions and the
  person's approvals. They change during a stream, belong to one machine, must not travel when a show is shared, and
  must not become undo steps. D10, D11 and D12 say where each lives.

### D3. Event ports carry control, value ports carry data, and a token carries each event's data along its path

Port types: `event`, `bool`, `number`, `text`, `list`, `json`.

- **An event wire carries a pulse.** A node runs only when a pulse reaches one of its event inputs, when an event it
  listens to arrives (triggers), or when its own wake or op reply comes back.
- **A value wire is read when its reader runs** (pull). Values never propagate by themselves, so nothing runs because
  a number changed, and the order of work is the order of events.
- **Tokens.** Each external event starts a token. The token travels along event wires, and every value a node sets
  while handling it is recorded in the token that its outgoing pulses carry. A read of an upstream output looks in the
  reader's own token first. **Why:** when forty raid messages each pass a two-second `flow.wait`, each must still read
  its own user and text after the wait. With latched outputs only (the Blueprint model), the fortieth message would
  overwrite what the first thirty-nine read. Node-RED gets the isolation by carrying a message object; tokens give the
  same isolation and keep the data wires people see in the editor.
- **Pure nodes** (math, compare, `text.format`, logic) have no event ports. Their outputs are **live**: evaluated
  when read, in the reader's token, and memoised for that activation. A stateful node may also declare a live output
  (a stopwatch's `elapsed`, a gate's `is_open`), computed from its state when read.
- **Resolving a wired value input:** the token's value for that (node, port); else the source's live output; else the
  source's latched value (the last value it set in any token); else the port's default; else the type's default
  (`false`, `0`, `""`, `[]`, `null`). **An unwired input:** its `set` constant, else the port default, else the type
  default.
- **Value wires form a DAG** (a cycle is refused when wiring). Event wires may loop; D8 guards them.
- **Coercions** on a wire or a `set` constant: `bool` and `number` to `text` (numbers shortest round-trip, integers
  without `.0`); anything to `json`; `json` to a typed input is checked when read, and a mismatch fails that read.
  `text` to `number` is never implicit, because it can fail: `text.to_number` makes the failure a visible output.
  Event and value ports never connect.
- **Numbers are `f64` and always finite.** `Val::number` refuses NaN and infinities, so a node that would produce one
  fires its `failed` output instead.

### D4. The node contract

`omuzaki-live-api`, modules `ids`, `show` (with `paths` and `check`), `graph`, `value`, `node`, `event`, `need`,
`services`, `limits` and `l10n`, all `pub mod` from the start so the contract tasks fill them in parallel.

```rust
// ── ids ─────────────────────────────────────────────────────────────────────────────────────────────
slug_id!(ShowId); slug_id!(GraphId); slug_id!(NodeId); slug_id!(GroupId); slug_id!(VarName);
slug_id!(Platform);                                   // twitch, youtube, bilibili, tiktok, or a plugin adapter's id
/// `family.name` (`flow.wait`, `cond.time_between`) or `plugin.<plugin>.<name>`:
/// ^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$, at most 96 bytes. The `node-` message id replaces `.` with `-`.
pub struct NodeKindId(Cow<'static, str>);
/// `chat.message`, `redeem`, `custom.<name>`, `plugin.<plugin>.<name>`: ^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$.
pub struct EventKind(Cow<'static, str>);
/// ^[a-z][a-z0-9_]{0,31}$.
pub struct PortName(Cow<'static, str>);
/// `<node>.<port>`, as written in a wire.
pub struct PortRef { pub node: NodeId, pub port: PortName }

// ── value ───────────────────────────────────────────────────────────────────────────────────────────
pub enum PortType { Event, Bool, Number, Text, List, Json }      // serde: "event", "bool", ...
pub enum Val { Bool(bool), Number(f64), Text(Arc<str>), List(Arc<[Val]>), Json(Arc<serde_json::Value>) }
impl Val {
    pub fn number(x: f64) -> Result<Val, ValError>;              // refuses NaN and infinities
    pub fn ty(&self) -> PortType;
    pub fn coerce(&self, to: PortType) -> Result<Val, ValError>; // the D3 rules
    pub fn to_json(&self) -> serde_json::Value;
    pub fn from_json(v: &serde_json::Value, ty: PortType) -> Result<Val, ValError>;
}

// ── ports ───────────────────────────────────────────────────────────────────────────────────────────
pub struct PortDecl {
    pub name: PortName,
    pub ty: PortType,
    pub summary: Option<&'static str>, // English; `node-<id>--in-<port>` / `--out-<port>`. None for dynamic ports
    pub label: Option<String>,         // dynamic ports: the person's own text (a switch case), never translated
    pub default: Option<Val>,          // value inputs
    pub live: bool,                    // value outputs computed by `Node::eval` when read
}
pub struct Ports { pub inputs: Vec<PortDecl>, pub outputs: Vec<PortDecl> }   // in display order
impl Ports {                                                     // builders, so a node's ports read as one line each
    pub fn new() -> Self;
    pub fn event_in(self, name: &'static str, summary: &'static str) -> Self;
    pub fn event_out(self, name: &'static str, summary: &'static str) -> Self;
    pub fn value_in(self, name: &'static str, ty: PortType, default: Val, summary: &'static str) -> Self;
    pub fn value_out(self, name: &'static str, ty: PortType, summary: &'static str) -> Self;
    pub fn live_out(self, name: &'static str, ty: PortType, summary: &'static str) -> Self;
    pub fn dynamic_in(self, name: PortName, ty: PortType, label: String) -> Self;   // and dynamic_out
}

// ── node ────────────────────────────────────────────────────────────────────────────────────────────
pub struct NodeMeta { pub id: &'static str, pub summary: &'static str, pub class: NodeClass, pub category: Category }
pub enum NodeClass {
    Trigger, // listens to event kinds (`listens`); starts tokens
    Pure,    // no event ports; every output is live; `handle` is never called
    Active,  // everything else
}
/// The editor palette's groups, fixed in the API like the library's `Section`.
pub enum Category {
    Triggers, Flow, Logic, Math, Text, Variables, Time, Conditions, Actions, Chat, Groups, Events, Plugin,
}

/// A node type. The type holds one instance's parameters and state.
pub trait NodeKind: Node + Sized + 'static {
    /// Typed parameters: `#[serde(deny_unknown_fields)]`, doc comments on every field (they are the `node-` text).
    type Params: Serialize + DeserializeOwned + JsonSchema + Default + Send + Sync + 'static;
    fn meta() -> NodeMeta;
    /// The ports for these parameters (switch cases, `text.format` placeholders and `op.run` arguments are dynamic).
    fn ports(params: &Self::Params) -> Ports;
    /// Event kinds a trigger listens to. Empty for other classes.
    fn listens(params: &Self::Params) -> Vec<EventKind> { Vec::new() }
    /// What running it needs beyond the base grants (D11): static, from the parameters only.
    fn needs(params: &Self::Params) -> Vec<Need> { Vec::new() }
    /// Range and consistency checks the schema can't express.
    fn check(params: &Self::Params) -> Result<(), ParamErrors> { Ok(()) }
    fn new(params: Self::Params) -> Self;
}

/// One running instance. The engine never calls it re-entrantly and never from two threads.
pub trait Node: Send + 'static {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError>;
    /// A live output's value, from the state and the inputs. Must not change anything.
    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> { Err(NodeError::not_live(port)) }
}

pub enum Input<'a> {
    Start,                                   // the show started, or this instance was (re)created while running
    Stop,                                    // the show is stopping; outputs fired now are ignored
    Event { port: &'a str },                 // a pulse on an event input, in the current token
    Trigger { event: &'a LiveEvent },        // an event this trigger listens to, in a new token
    Wake { id: WakeId },                     // a wake this node asked for, in the token that asked
    Reply { call: CallId, outcome: Result<Option<serde_json::Value>, OpError> },   // an op call came back
}

/// What a node may do. A thin wrapper over the engine (or the harness), so helpers stay out of the host trait.
pub struct NodeCx<'a> { host: &'a mut dyn NodeHost }
impl NodeCx<'_> {
    pub fn now(&self) -> Seconds;                                 // the step's time on the runner clock
    pub fn utc(&self) -> UtcMillis;                               // the step's wall time (WallClock)
    pub fn rng(&mut self) -> &mut Rng;                            // this node's own stream (D6)
    pub fn input(&mut self, port: &str) -> Result<Val, NodeError>;   // resolved in the current token (D3)
    pub fn input_bool(&mut self, port: &str) -> Result<bool, NodeError>;   // and input_number, input_text, input_list
    pub fn set(&mut self, port: &str, value: impl Into<Val>) -> Result<(), NodeError>;
    pub fn fire(&mut self, port: &str) -> Result<(), NodeError>;
    /// Sets `error` to the English text, records `why` for people, and fires `port` (usually `failed`).
    pub fn fail(&mut self, port: &str, why: Text) -> Result<(), NodeError>;
    pub fn wake_after(&mut self, after: Seconds) -> Result<WakeId, NodeError>;
    pub fn wake_at(&mut self, at: Seconds) -> Result<WakeId, NodeError>;
    pub fn cancel_wake(&mut self, id: WakeId);
    /// Asks for an op run in the show's session (D7); the reply arrives later as `Input::Reply` in this token.
    pub fn run_op(&mut self, op: &str, args: serde_json::Value) -> Result<CallId, NodeError>;
    pub fn var(&self, name: &VarName) -> Option<Val>;
    pub fn set_var(&mut self, name: &VarName, value: Val) -> Result<(), NodeError>;   // type-checked (D10)
    pub fn group_enabled(&self, group: &GroupId) -> Option<bool>;
    pub fn switch_group(&mut self, group: &GroupId, on: bool) -> Result<(), NodeError>; // takes effect at step end
    pub fn emit(&mut self, kind: EventKind, payload: serde_json::Value) -> Result<(), NodeError>;   // D8 hops
    pub fn event(&self) -> &LiveEvent;                            // the current token's root event
}
pub trait NodeHost { /* the object-safe form of the above; implemented by the engine and by NodeHarness */ }
pub struct EvalCx<'a> { host: &'a mut dyn EvalHost }           // input*, now, utc, var, group_enabled: reads only

pub struct WakeId(pub u64);
pub struct CallId(pub u64);
pub struct NodeError { pub code: NodeErrorCode, pub text: Text }   // a misuse or a fault, not an expected failure
pub enum NodeErrorCode { UnknownPort, Type, Limit, NotLive, Internal }
pub struct ParamErrors(pub Vec<ParamError>);                      // ParamError { path: String, text: Text }

// ── registration (ADR 0015) ─────────────────────────────────────────────────────────────────────────
pub struct NodeRegistration {                // fn pointers, so `register_node!` works at link time
    pub meta: fn() -> NodeMeta,
    pub schema: fn() -> Schema,              // the params' JSON Schema, inlined like op schemas
    pub defaults: fn() -> serde_json::Value,
    pub check: fn(&serde_json::Value) -> Result<serde_json::Value, ParamErrors>,   // normalised params
    pub ports: fn(&serde_json::Value) -> Result<Ports, ParamErrors>,
    pub listens: fn(&serde_json::Value) -> Result<Vec<EventKind>, ParamErrors>,
    pub needs: fn(&serde_json::Value) -> Result<Vec<Need>, ParamErrors>,
    pub new: fn(&serde_json::Value) -> Result<Box<dyn Node>, ParamErrors>,
}
impl NodeRegistration { pub const fn of<K: NodeKind>() -> Self; }
#[macro_export] macro_rules! register_node { ($ty:ty) => {
    $crate::__inventory::submit! { $crate::node::NodeRegistration::of::<$ty>() }
    $crate::__record_registration!("node", $ty);
}; }
pub fn node_kinds() -> impl Iterator<Item = &'static NodeRegistration>;
pub fn find_node_kind(id: &str) -> Option<&'static NodeRegistration>;
/// What the engine and the surfaces hold: a static registration or a plugin's kind (D15).
pub trait DynNodeKind: Send + Sync { /* the NodeRegistration functions, as &self methods */ }
impl DynNodeKind for NodeRegistration { /* ... */ }
```

**A whole node, as a small task writes it** (`flow.wait`):

```rust
/// Parameters of `flow.wait`: none; the duration is an input so it can vary per event.
#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {}

/// Delays each event on `in` by `seconds`; every event waits on its own, and `cancel` drops the waiting ones.
pub struct Wait { pending: Vec<WakeId> }

impl NodeKind for Wait {
    type Params = WaitParams;
    fn meta() -> NodeMeta {
        NodeMeta {
            id: "flow.wait",
            summary: "Wait, then pass the event on.",
            class: NodeClass::Active,
            category: Category::Flow,
        }
    }
    fn ports(_: &WaitParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to delay.")
            .event_in("cancel", "Drops every event still waiting.")
            .value_in("seconds", PortType::Number, Val::Number(1.0), "How long to wait.")
            .event_out("out", "The event, after the wait.")
    }
    fn new(_: WaitParams) -> Self { Self { pending: Vec::new() } }
}

impl Node for Wait {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in" } => {
                let seconds = cx.input_number("seconds")?.max(0.0);
                self.pending.push(cx.wake_after(Seconds(seconds))?);
            }
            Input::Event { port: "cancel" } => self.pending.drain(..).for_each(|id| cx.cancel_wake(id)),
            Input::Wake { id } => {
                self.pending.retain(|p| *p != id);
                cx.fire("out")?;
            }
            _ => {}
        }
        Ok(())
    }
}
register_node!(Wait);
```

**Port conventions,** so templates and tasks agree without asking:

| Port | Type | Meaning |
|---|---|---|
| `in` | event in | the main input of an active node |
| `out` | event out | passes the event on (flow nodes) |
| `fired` | event out | a trigger matched |
| `done` | event out | an action finished; its results are set before it fires |
| `failed` + `error` | event out + text out | an expected failure (a bad value, an op refused, a timeout); `error` holds the English text |
| `value` | value out | the main result |

**Localised text** (`docs/design/l10n-ops.md` §3.1): a kind's summary is `node-<id>` (`node-flow-wait`); parameters
follow the schema walk (`node-flow-wait--<key>`); ports are `node-<id>--in-<port>` and `node-<id>--out-<port>`
(parameter keys are snake_case, so a single `-` can't collide); event kinds are `event-<kind>` with their payload
walk; categories are `node-category-<name>`. Messages use the crate prefixes `live-api-`, `live-` and `live-nodes-`.
The crate check learns the `node-` and `event-` registries in its own task (§7).

### D5. Execution: one engine thread per running show; each item is a step run to completion, depth-first

- **One show runs at a time** per studio (`automation.start {show}` refuses while another runs: "stop `<show>`
  first"). Streams are one at a time, and one engine thread keeps ordering trivially exact.
- **Threads.** The engine thread does all node work; it never runs on the frame or audio thread and never takes the
  executor's lock. Op calls go to a dispatcher (D7). Persisted variables are written by a small writer thread (D10).
  The engine sleeps until its next wake or inbox item: an idle show costs nothing.
- **Items, in time order.** The next item is the earliest of: the earliest due wake (ties by the order they were
  asked for), and the next inbox item (events and op replies, in arrival order). A wake due at `t` runs before an
  inbox item stamped `t`. The step's time is the later of the item's time and the previous step's, so a node never
  sees time go backwards.
- **A step:**
  1. *Seed the stack.* An event: every active trigger listening to its kind, in ascending (graph id, node id), each
     with a new token rooted at the event (and, under the per-graph rate limit, D8). A wake or a reply: its node, with
     the token that asked.
  2. *Run depth-first.* Pop an activation, call `handle`. The pulses it fired are collected in the order fired; each
     goes to its wire destinations in ascending (node id, port); they are pushed so that the first fired pulse's whole
     subtree runs before the second's. So "do this, then that" means what people expect, and `flow.then` is just a node
     that fires its outputs in order. A node is never inside `handle` twice.
  3. *Close the step.* Apply group switches (D12), coalesce variable changes into one `var.changed` event per variable
     for the next step (D10), hand queued op requests to the dispatcher in order, refresh the values snapshot if due.
- **Async work parks the token.** `wake_after` and `run_op` keep the current token with the wake or the call; the
  later `Wake` or `Reply` runs in it, so a reply's `done` still carries the user who redeemed.
- **A firing outside any token** (from `Start`, or a timer's own wake) starts a new token rooted at a synthetic
  `engine.tick` event naming the graph and node.
- **Faults:** a `NodeError` from `handle` drops that token's branch and is counted and shown (`automation.status`);
  the node keeps running. A panic inside a node is caught at the activation, the node becomes **faulted** (inert
  until the show restarts) and a finding says so. Nothing a node does can stop the show (BR-23).
- **Hot reload.** The engine follows `Runner` events for patches under its show, waits 100 ms for the edits to settle,
  recompiles, and swaps in the new `CompiledShow` between steps. An instance whose kind, parameters and wiring are
  unchanged keeps its state; a changed one is rebuilt (`Start` again) and its parked tokens are dropped and counted.

### D6. Determinism: same show, same seed, same items, same results

**The guarantee.** Given the same show document, the same seed and the same sequence of items (each event, wake and
reply with its time), the engine produces byte-identical results on the same machine and build (ADR 0009 rule 1):
the op requests in the same order with the same arguments, the same node firings with the same values, the same
variable values and the same findings. `NodeHarness` and `ShowHarness` assert it by running a script twice and
comparing the JSON logs byte for byte.

How each source of variation is pinned:
- **Time.** Nodes read only `cx.now()` and `cx.utc()`, which are the step's time. The engine takes a `&dyn Clock`
  (the runner's clock, `FakeClock` in tests) and a `&dyn WallClock`. `FakeWall` derives wall time from a start
  instant plus the fake clock, so a schedule test can start on a DST day.
- **Randomness.** The show's seed is `show.seed` when set; otherwise the engine draws one from the OS at start and
  reports it (`automation.status`), so a run can be reproduced by pinning it. Each node draws from its own stream,
  `Rng::new(u64::from(seed)).fork(stream)` on a fresh root (so the order nodes are created in doesn't matter), with
  `stream` = FNV-1a 64 of `"<graph>/<node>"`: adding or removing a node never shifts another node's numbers. A rebuilt
  instance restarts its stream. `std` hashers never reach an output.
- **Order.** Every ordering rule is in D5. The engine iterates `BTreeMap`s and sorted vectors only.
- **The outside world** is in the items. Under real time, when a reply or an event arrives is not deterministic, but
  given the items as they arrived, the rest is. That is what makes a run reproducible from a recording later.
- **Floats** are `f64`; across platforms the ADR 0009 tolerances apply (libm `powf` in `math.op pow`, `math.ease`).

### D7. Effects only through ops, in the show's own automation session

**The boundary.** A node changes the engine's own state directly: its own fields, wakes, variables (D10), group
switches (D12) and emitted events. **Anything outside the engine** (the document, the avatar, OBS, the network, files,
programs, speech) **changes only through an op**, by `cx.run_op`. Pure nodes and triggers never call ops. So "every
node is an op" holds in the sense that matters: every effect a graph has is a typed, permission-checked, logged op,
the same op an agent or a script would call. **Variables are not ops** on purpose: a counter that ticks for every chat
message would otherwise put thousands of records into the op log and the journal during a raid, count against limits
meant for edits, and take the executor's lock for state nothing else reads (the values monitor reads them through a
query, D10).

**The automation session.** `automation.start` opens one session for the run (`Sessions::open`, ADR 0020):

| Field | Value |
|---|---|
| `transport` | `Transport::Automation` |
| actor | `Actor::Automation { id: "<show>#<session>" }`, built by the executor (a new variant, its own task): history, the Theatre and the History filter "automation" tell it apart from agents, and it isn't `System`, which stays Omuzaki's own |
| `on_behalf_of` | the person whose approvals the show runs with: the person on this machine (VC-10 later: the one who approved) |
| `client` | `automation:<show>` |
| grants (ceiling) | the base `read` + `perform`, plus what the person approved for this show on this machine (D11); never `native`, `input`, `edit:show` or `edit:library` |
| `budget` | `usd_micros` = the approved spend cap for the show (default 0) |
| `limits` | edits 1,200 per minute (burst 200), runs 6,000 per minute (burst 600; polling conditions use queries), max pending approvals 16 |

The session ends on `automation.stop`, on project close and on exit. **An automation never opens a transaction:** each
call is its own one-op transaction.

**Requests the engine builds:** `actor` = the session's actor; `cause` = `Cause { kind: Rule, text: "<show>/<graph>/
<node> on <event kind> #<event id>" }`. The cause names the rule and the event, **never the message text or the
viewer's name**, because the op log and the journal are written to disk and chat is personal data (SE-04); the
running engine's in-memory ring has the details while the show runs. `base` is not set (standing rules aren't
stale-write guarded).

**Live actions never enter undo.** Most automation calls change live state (an expression, an OBS scene, a sound), not
the document. Such ops are `OpKind::Edit` with an empty patch and `Undo::None`; the executor logs and announces them
but never puts them in an undo history and never bumps the document version (its own task, §7). Automation steps are
also **excluded from the person's per-principal Ctrl+Z** (agent-sessions §5.4): an automation is a standing rule, not
an assistant working beside the person. A document edit an automation makes (with `edit:scene` approved) shows in
History with its cause and is reverted explicitly from there.

**Who acts for a manual event.** `automation.fire` (the Test buttons, an agent trying a graph) publishes an event with
`Source::Manual { actor }` and `acting = Some(actor)` (D9). Every op requested in that event's tokens runs **as that
actor**, with that actor's own grants, never the show's. So a person's Test does what a real redeem would, and an agent
can never use a show's approvals to do what its own session may not. `EventSink::publish` refuses `Source::Manual` and
any event with `acting` set; only `automation.fire` makes the first, and only the engine's `event.emit` copies the
second.

**The actor is a property of the token, and it survives derived events.** A token's *acting actor* is the `acting` of its
root event (none for an event from outside: the show's session acts). It is fixed when the token is created and never
changes, so:
- a wake or a reply resumes the token and its actor, so a `flow.wait` after a manual event still acts as the caller;
- `event.emit` publishes the new event with `acting` copied from the emitting token and `Source::Graph { graph, node }`
  (the source says where it came from, `acting` says for whom), and the tokens it starts act as that same actor,
  through any number of hops (D8's limit of 8 still applies);
- the actor never widens: an event with `acting = Some(a)` is never run with the show's grants, and `acting` is never
  set to anything but the caller's actor;
- a `var.changed` or `engine.tick` event started by such a token is likewise stamped with its `acting`, so a handler of
  a variable that a Test changed also runs as the caller.

The permission test: a caller whose grants lack `perform` fires `custom.a` into a graph
`on.event(custom.a) → event.emit(custom.b)`, and `on.event(custom.b) → op.run(<perform op>)` is **denied**, with the
show approved for it; the same chain from a real platform event, and from a caller who holds `perform`, runs.

**The generic action: `op.run`.** One node runs any op allowed for automations, so every OBS action, VTS hotkey,
scene switch, expression, sound or AI line is an action without a node per op:

```toml
kind = "op.run"
[params]
op = "obs.set_scene"
args = { scene = "$in.scene", transition_ms = 300 }
[in]
in = "cmd.fired"
scene = "pick.value"
```

- A string argument that is exactly `"$in.<name>"` becomes a value input `<name>`, typed from the op's JSON Schema at
  that path (`string` to text, `number` and `integer` to number, `boolean` to bool, `array` to list, else json).
  Placeholders are never spliced into longer strings (the library's rule, D3 of `docs/design/library.md`); `$$` writes
  a literal `$`; text is built with `text.format`.
- Outputs: `done` with `output` (json), `failed` with `error`.
- **Arguments are checked in two stages.** *At compile*, the engine checks everything that is static against the op's
  schema: every field name, every literal value, and that each placeholder's path exists and its input type is
  compatible with the schema there. A placeholder's position is **left out of the decode** (the field is treated as
  present, its value unknown), not replaced by a default: the type's default would make `character =
  "$in.character"` an empty string, which `CharId` refuses, and a valid graph would not compile. A misspelt field or a
  literal that doesn't decode is a finding, not a stream-time failure. *At activation*, the engine substitutes the
  real values and decodes the whole argument object through the op registry; a mismatch (a `character` that isn't an
  id, a number out of range) makes the node fire `failed` with `invalid_args` and the field's path, and nothing is
  sent to the executor. The tests: a placeholder for a typed id compiles; the same graph with a bad value at
  run time fires `failed`; a misspelt static field is a finding.
- **Allowed ops:** scope `read`, `perform`, `broadcast`, `render`, `edit:character` or `edit:scene`, any kind.
  **Refused:** `edit:show`, `edit:library`, `publish`, `spend`, `net`, `fs`, `process`, `native`, `input`. Those
  effects have their own nodes, whose parameters the engine can analyse statically (D11); a graph that could build
  any URL or program couldn't be approved honestly. (A feature that needs `publish` or `spend` from a show,
  like hosted voices for the chat reader, adds its own node with its own needs.)
- **Scope is not enough: ops that take a path, a URL or a program are refused whatever their scope.** `file.read_text`
  and `file.stat` are `read` ops (D11), but `op.run` with a dynamic `path` would read any file under the approved roots
  with no `Need::File` for it and no literal path to show on the approval card. So `op.run` also refuses: every
  `OpKind::Job` op; every op whose registration lists `fs`, `net` or `process` among its scopes (`file.read_text` lists
  `read` or `fs`); and by name the effect ops of D13 (`net.http`, `net.udp_send`, `file.read_text`, `file.write_text`,
  `file.append_text`, `file.stat`, `exec.run`, `system.process_running`). The name list is the backstop for an op
  registered without the right scopes. A refused op is a compile finding naming the dedicated node ("use `file.read`,
  whose path is literal and approved"). Those nodes are the only way a show reaches files, the network, programs and
  processes. The test: `op.run` aimed at each of the eight names, at a job op and at an `fs`-scoped op is a
  finding, and `file.read` with an unapproved path stays inert.
- Its needs are the target op's scope and the op itself (`Need::Op`), so the approval card can say "switches OBS
  scenes".
- An op that asks for a person (`require_person`) from the automation session queues an approval like any other
  (ADR 0020 §7.1); the node fires `failed` with `waiting` and the card appears in Omuzaki.

**Dispatch.** Requests leave the engine at the end of each step, in order, into two lanes: the **ordered lane** (one
worker; edits, queries and live actions, in request order, so "switch scene, then show the overlay" lands in that
order) and the **job lane** (four workers; the `OpKind::Job` ops `net.*`, `file.*`, `exec.run`, which wait on the
outside world and run off the executor's lock through the job path). Replies come back to the inbox. The engine
never retries: `Limited`, `Waiting` and `Denied` become the node's `failed`.

### D8. The loop guard and the rate limits

| Guard | Default | When it trips |
|---|---|---|
| Activations in one step | 2,000 | The step stops; the rest of its work is dropped; the graph that ran most is **suspended** (inert until `automation.resume_graph` or a restart) with a finding naming the last eight nodes on the stack, so the loop is visible |
| Stack depth in one step | 128 | as above |
| `event.emit` hops | 8 | An emitted event's `hops` is its emitting token's root event's `hops` + 1 (events from outside have 0); one past 8 is dropped and the emitting graph suspended |
| New tokens per graph | 500 per second, burst 2,000 (token bucket on the engine clock) | New tokens for that graph are dropped and counted (`dropped.rate`); the graph keeps running |
| Op calls in flight per show | 64 | `run_op` makes the node fire `failed` with `limited` |
| Session rate limits | D7 table (executor-enforced, ADR 0020 §8.2) | `failed` with `limited` and the retry time |
| `net.*` requests in flight | 8 per show | `failed` |
| `exec.run` | 4 running, 30 per minute per show | `failed` |
| Inbox | droppable queue 10,000; other kinds 100,000 pending | a droppable kind (`chat.message`) is dropped and counted when its queue is full. Any other kind is accepted up to the hard cap of 100,000 pending, ten times the droppable queue, which a show doesn't reach in practice. Past it `publish` returns `Full`, counted (`dropped.inbox_full`), and **the producer owns the retry** (D9) |
| Pending wakes per node | 1,000 | `wake_*` returns `Limit` |
| Values | text 64 KiB, lists 1,000 items, event payloads 64 KiB | the setter or the publisher gets `Limit` |
| Show size | 256 graphs, 512 nodes per graph, 64 ports per node, 1,024 variables, 64 KiB of params per node | the document op refuses |
| Per-key state in a node (per-user cooldowns, repeat windows) | 10,000 keys, oldest expired first | the node's own doc says what it forgets |

All guards run on the engine's clock, so they are exact under `FakeClock`. A suspension, a drop and a fault are
counted in `automation.status` and shown on the Rules panel; none of them stops the show.

### D9. The event envelope

```rust
pub struct LiveEvent {
    pub id: EventId,                  // u64 in arrival order, set by the inbox
    pub kind: EventKind,
    pub source: Source,
    pub origin_id: Option<String>,    // the platform's id for this event: dedup, replies, moderation
    pub at: Seconds,                  // stamped by the producer from the runner clock
    pub utc: Option<UtcMillis>,       // the platform's own timestamp, when it sends one
    pub payload: serde_json::Value,   // an object, validated against the kind's schema at publish
    pub hops: u8,                     // D8
    pub acting: Option<Actor>,        // the caller a manual event runs as; copied by event.emit, else None (D7)
}
pub struct EventId(pub u64);
pub struct UtcMillis(pub i64);        // milliseconds since the Unix epoch, UTC
pub enum Source {
    Platform { platform: Platform, channel: String },   // BR-10 adapters, plugin adapters (PL-03)
    Input,                                              // hotkeys, MIDI, Stream Deck (PF-05, PF-25)
    Voice,                                              // voice commands (AP-10)
    Obs,                                                // BR-19
    Engine,                                             // show lifecycle, var.changed, engine.tick
    Graph { graph: GraphId, node: NodeId },             // event.emit; for whom it acts is `LiveEvent::acting`, not here
    Manual { actor: Actor },                            // automation.fire only (D7); `acting` carries the same actor
    Plugin { id: String },
    Test,                                               // harnesses
}
/// A typed payload; `LiveEvent::of` and `decode` convert.
pub trait EventPayload: Serialize + DeserializeOwned + JsonSchema { const KIND: &'static str; }
pub struct EventKindRegistration {
    pub kind: &'static str,
    pub summary: &'static str,          // English; `event-<kind>`
    pub schema: fn() -> Schema,
    pub droppable: bool,                // may be dropped when the inbox is full (D8)
}
// register_event_kind!(ChatMessage, droppable); register_event_kind!(Redeem);
pub trait EventSink: Send + Sync {
    /// Never blocks. `Full` for a droppable kind whose queue (10,000) is full (counted, not queued) and for any other
    /// kind past the hard cap of 100,000 pending (D8); `Refused` for Source::Manual, an event with `acting` set, an
    /// unknown kind or a bad payload.
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError>;
}
```

- **Producers publish; nothing polls.** BR-10's adapters, the input source, `omuzaki-speech` and the OBS client get the
  `EventSink` as a service and call `publish` from their own threads. The inbox is lock-free and bounded (D8): droppable kinds are shed first, the rest are held to a cap no show reaches,
  and a producer retries what it could not hand over.
- **Kinds self-register** (`register_event_kind!`, §39 rule 2) with a payload schema, a summary and whether they may
  be dropped under load. `on.event` derives its output ports from the schema (§6), the editor lists them, and plugin
  adapters add theirs at run time through the plugin host (D15). Adapters for other platforms (a Kick plugin) publish
  the **normalised** kinds below with their own `platform`, so a chat command works on every platform at once.
- **Backpressure: who keeps what.** `publish` never blocks, so it cannot promise to accept everything; the contract is:
  - a **droppable** kind is best-effort: `Full` means the event is gone and counted, and the producer does nothing;
  - any **other** kind (a redeem, a raid, a sub, a tip) is never dropped by the inbox below 100,000 pending. On `Full`
    (the engine is stalled or far behind) the **producer retains and retries**: each adapter keeps its own bounded
    buffer of undelivered non-droppable events (1,000 per adapter, oldest first), retries on a 100 ms backoff capped at
    1 s, and delivers in order. If the buffer overflows it drops the oldest and counts `dropped.producer`. The engine
    shows both counters and raises a finding ("events are being lost: the show can't keep up");
  - the engine's own events (`event.emit`, `var.changed`, `engine.tick`, lifecycle) don't go through `publish`: the
    engine queues them itself under the loop guard (D8), so they never compete with producers.
  This is the contract of the inbox task (10,000 droppable, 100,000 hard cap, then `Full`); the adapter buffer
  belongs to each adapter's task (BR-10, the input source, the listener, BR-19) and is tested by
  publishing into a stalled sink.
- **Dedup:** the inbox drops an event whose (`platform`, `kind`, `origin_id`) it saw among the last 1,024 of that
  platform within 10 minutes. EventSub and YouTube redeliver; graphs must not double-fire.
- **Privacy:** events live in memory only. The engine keeps the last 50 for the values monitor and never writes them;
  nodes that write files or send requests do so only with the person's approval (D11).

**Normalised kinds** (registered by `omuzaki-live-api`; BR-10 adds fields, never renames):

| Kind | Payload | Fields BR-11 relies on |
|---|---|---|
| `chat.message` (droppable) | `ChatMessage` | below |
| `follow` | `Follow` | `platform`, `channel`, `user` |
| `sub` | `Sub` | `platform`, `channel`, `user`, `tier` (text: `1000`, `2000`, `3000`, `prime`, or the platform's level name), `months`, `message`, `gift` (bool), `gifter` (optional `ChatUser`) |
| `sub_gift` | `SubGift` | `platform`, `channel`, `gifter` (optional: anonymous), `count`, `tier` |
| `cheer` | `Cheer` | `platform`, `channel`, `user` (optional), `bits`, `message` |
| `tip` | `Tip` | `platform`, `channel`, `user`, `amount` (`Money { micros: i64, currency: String }`), `message`: YouTube Super Chat and Super Stickers, tip services through plugins |
| `redeem` | `Redeem` | `platform`, `channel`, `user`, `reward` (`id`, `title`, `cost`), `input` (optional text), `redemption_id` |
| `raid` | `Raid` | `platform`, `channel`, `from` (`ChatUser` of the raiding channel), `viewers` |
| `power_up` | `PowerUp` | `platform`, `channel`, `user`, `kind`, `bits` |
| `poll`, `prediction`, `hype_train` | `Poll`, `Prediction`, `HypeTrain` | `platform`, `channel`, `phase` (`begin`, `progress`, `lock`, `end`), and the choices, outcomes or level BR-10 defines |
| `hotkey` | `Hotkey` (PF-25) | `binding` (its id), `phase` (`down`, `up`). Never the key itself |
| `voice.command` | (`omuzaki-speech`, AP-10) | `command`, `phrase`, `score`. Never the transcript |
| `obs.*` | (BR-19) | as BR-19 defines |
| `show.started`, `show.stopping` | `ShowLifecycle` | `show` |
| `var.changed` | `VarChanged` | `name`, `scope`, `old`, `new` |
| `engine.tick` | | the synthetic root of tokens started by a node (D5) |
| `custom.<name>` | any object | `event.emit` and `automation.fire` |

```rust
pub struct ChatMessage {
    pub platform: Platform,            // twitch, youtube, bilibili, tiktok, or a plugin adapter's id (kick)
    pub channel: ChannelRef,           // { id, name }: whose chat (a collab may read several)
    pub message_id: String,            // the platform's id (replies, deletes, moderation)
    pub user: ChatUser,
    pub text: String,                  // as the platform sent it; emotes as their names
    pub emotes: Vec<EmoteSpan>,        // { start, end (byte offsets in text), id, name }
    pub reply_to: Option<String>,      // the parent message id
    pub action: bool,                  // /me
    pub first_message: bool,           // the user's first message in this channel, when the platform says so
}
pub struct ChatUser {
    pub id: String,                    // the platform's stable user id: the key for cooldowns, voices, memory
    pub login: String,                 // the handle
    pub name: String,                  // the display name
    pub roles: BTreeSet<Role>,
    pub badges: Vec<String>,           // the platform's raw badge ids, for platform-specific conditions
    pub colour: Option<String>,        // #rrggbb, when the platform has one
}
pub enum Role { Broadcaster, Moderator, Vip, Subscriber }   // rank for "at least": that order; nobody = everyone
```

Roles are normalised by the adapters: YouTube's owner is `Broadcaster` and its members are `Subscriber`; Bilibili's
room admins are `Moderator` and its guards `Subscriber`. A user's rank is their highest role. Timers are nodes that
wake themselves (D5), not events.

### D10. Variables

```toml
# in shows/<show>/show.toml
[variables.bonks]
type = "number"        # bool | number | text | list
scope = "show"         # session | show | global
default = 0
summary = "How many times the bonk redeem landed."
```

| Scope | Shared by | Lives | Persisted in |
|---|---|---|---|
| `session` | the graphs of the running show | from `automation.start` to `stop` | nowhere |
| `show` | the graphs of this show | across runs | `<project>/.omuzaki/automation/<show>/values.toml` (this machine; not in the document) |
| `global` | every show of every project of this OS user | across runs and projects | `<data>/omuzaki/automation/globals.toml` |

- **Declarations are document, values are state.** A declaration (type, scope, default, summary) is shown, shared and
  undoable; a value changes during a stream, belongs to this machine and must not travel in a shared show. A shared
  show starts from its defaults. The project's `.omuzaki/` folder is not committed, like the op log, so a counter
  follows the project folder but not a git clone (said in the Rules panel's help).
- **Writes are debounced:** at most one write per second per file, atomically (temporary file, then rename), by a
  writer thread, plus a final flush on stop and on exit. A crash loses at most the last second. 10,000 `var.add` in
  one second write once, then once more at the flush.
- **Types are checked on every write.** A mismatch (`var.add` on text) fails the node with a message naming the
  variable. A `global` declared with two types by two shows: the second show's declaration is a finding and its
  nodes on it are inert.
- **Change events are coalesced per step:** one `var.changed` per changed variable, with the first old value and the
  last new value, delivered in the next step. So `var.set` inside a `var.changed` handler can't spin within a step, and
  the loop guard sees any cross-step loop.
- **Secrets are never variables** (D11).
- **The values monitor** reads `automation.values`: every variable (name, scope, type, value, changed at, changed by
  which node), group states, each graph's state (active, suspended, inert nodes, faults), the last 200 firings (node,
  port, time, values truncated to 200 characters), the last 50 events (kind, source, a one-line summary), the drop
  counters and the pending approvals. The engine refreshes this snapshot into a one-slot mailbox at most four times a
  second while someone read it in the last two seconds, so the query never waits on the engine and an unwatched show
  pays nothing. `automation.set_variable` is how a person edits a value there.

### D11. Risky nodes: static needs, approved per show on this machine

**The threat.** A show can come from a library template, a friend's file, a collaborator's git push or an agent's
edit. Its graphs may try to call any URL, write files, run programs, read secrets or switch a stream. So a show runs
with only the base grants (`read`, `perform`: the avatar moves, nothing leaves Omuzaki) until the person approves what
it needs, **here, for this show, on this machine.**

- **Needs are static.** Each node declares them from its parameters (`NodeKind::needs`); the engine unions them per
  show at compile. `Variable` and `Group` needs are checked against the show's own declarations; every other need
  against the person's approvals. A node whose needs aren't all approved is **inert**: it never activates, events
  that reach it are counted, and the Rules panel shows one approval card for the show listing what is missing.

```rust
pub enum Need {
    Scope(Scope),                                  // e.g. broadcast, through op.run's target op
    Op { op: String },                             // op.run targets, so the card can say what they do
    Host { scheme: NetScheme, host: String, port: u16 },   // net.http, net.udp_send; NetScheme { Http, Https, Udp }
    File { path: String, write: bool },            // file.* nodes and file conditions
    Program(Invocation),                           // exec.run
    Secret { name: String, host: String },         // a secret sent to a host
    Process { name: String },                      // cond.process_running
    Variable { name: VarName, ty: VarType },       // var.* nodes: checked against the show's declarations
    Group { group: GroupId },                      // group.* nodes: checked against the declared groups
}
pub struct Invocation {
    pub program: String,                // an absolute path
    pub script: Option<String>,         // a script the program runs, hashed with it
    pub args: Vec<ArgTemplate>,
    pub cwd: Option<String>,
}
pub enum ArgTemplate { Literal(String), Input(PortName) }

pub struct ShowKey { pub project: String, pub show: ShowId }   // the project folder's real path, the show id
pub struct ShowApprovals {
    pub granted: BTreeSet<Need>,          // what the person allowed; Program entries are pinned by ApprovedProgram
    pub programs: Vec<ApprovedProgram>,   // { invocation, program_hash, script_hash: Option<ContentHash> }
    pub roots: Vec<String>,               // the fs roots the person chose for File needs (ADR 0013)
    pub spend_cap_usd_micros: u64,
    pub autostart: bool,                  // start this show when the project opens, on this machine only
}
pub trait Approvals: Send + Sync {
    fn show(&self, key: &ShowKey) -> ShowApprovals;
    /// Whether this exact invocation (program and script content hashed) was approved on this machine.
    fn program_trusted(
        &self,
        invocation: &Invocation,
        program_hash: &ContentHash,
        script_hash: Option<&ContentHash>,
    ) -> bool;
}
```

- **Approvals are machine-local and never in the project:** `<data>/omuzaki/automation/approvals.toml`, keyed by
  `ShowKey`. A shared or copied project carries none, so an imported show starts with the base only. A pull that
  adds a node with a new need makes that node inert until approved; a pull that only reuses approved needs (the same
  host, the same program and hash) runs.
- **Only a person approves, in Omuzaki's window.** The card's "Allow on this machine" writes through the approvals
  store's own method, which the app wires into the UI and which no op calls. There is no CLI or MCP way to approve,
  for the reason ADR 0020 §7.2 gives: an agent controls both. Agents can still edit graphs; anything risky they add is
  inert until the person looks. Settings lists every approval per show with a revoke button.
- **The session ceiling** is the base plus the approved scopes, the approved roots as fs roots (`Grants::with_fs_root`,
  so `OpCtx::fs_access` enforces them in the op too) and the spend cap. The executor intersects it with the person's
  grants as for any session.

Per node family:

| Node | Op (kind, scope) | Rules |
|---|---|---|
| `net.http` | `net.http` (job, `net`) | scheme, host and port are literal (no placeholders), so the host is known statically and approved as `Need::Host`; placeholders `{name}` in the path and query are percent-encoded; the body is `json` (an object whose string values may be exact `$in.` placeholders) or `text` (a `{name}` template); methods GET, POST, PUT, PATCH, DELETE; timeout 10 s default, 60 s max; redirects only to the same host, at most 3; the response body is cut at 64 KiB with `truncated = true`; TLS through rustls with the OS's certificate store |
| `net.udp_send` | `net.udp_send` (job, `net`) | host and port literal; payload text or hex; one datagram of at most 1,200 bytes |
| `file.read`, `file.write`, `file.append` | `file.read_text`, `file.write_text`, `file.append_text` (jobs; `read` or `fs`) | the path is literal and inside a root the person chose (`Need::File`); `fs_access` checks it again in the op; reads at most 1 MiB of UTF-8; writes are atomic (temporary file, rename); typical use is a text file OBS shows |
| `cond.file_exists`, `cond.file_changed` | poll `file.stat` (job, `read`) | the same path rules; poll every 2 s by default (0.5 s minimum); a change is (size, modified time) and settles for `debounce_ms` |
| `cond.process_running` | poll `system.process_running` (query, `process`) | answers only for the names configured in the graph (`Need::Process`); it never lists or reports other processes (privacy) |
| `exec.run` | `exec.run` (job, `process`) | below |
| secrets in `net.http` headers | resolved inside the op | `{secret:<name>}` in a header value; `Need::Secret { name, host }`, so a secret only ever goes to the host it was approved for; resolved at send time through the secret store (`SecretStore` CN-05 pulled into Phase 2; label `automation/<name>`), never written to a document, an output, a log, the values monitor or an op record (the record keeps `{secret:<name>}`), and any echo of the secret's value in a response is replaced by `{secret:<name>}` before the outputs are set |

**Running a program** (the strictest rule):
- The program is an **absolute path** (no `PATH` lookup), so "the exact program" means one file. The approval pins its
  BLAKE3 content hash. The engine re-hashes at compile, and the op re-hashes right before it spawns (cached by path,
  size, modified time and file id). A changed program file is a new program: inert until approved again.
- **Scripts** (`.bat`, `.cmd`, `.ps1`, `.sh`, `.command`) may be the program: Omuzaki runs them with their interpreter
  and hashes the script itself, so the approval is for the script's content. `.bat` and `.cmd` take no placeholders in
  their arguments, because `cmd.exe`'s quoting can't carry untrusted text safely.
- **Arguments** are a list of literals and whole-argument `$in.<name>` placeholders. A placeholder becomes exactly one
  argument and is never split or joined, and there is no shell (`sh -c`, `cmd /c`) anywhere, so chat text can't add
  arguments or commands. The card shows the template and where each placeholder's value comes from.
- `cwd` is literal or the program's folder. The program inherits Omuzaki's environment.
- With `wait`: `exit_code`, `stdout` and `stderr` (64 KiB each, truncated with a flag); the timeout (30 s default,
  600 s max) kills the whole process tree. Without `wait`, `started` fires once it runs; a detached program is not
  killed when the show stops (launching OBS is the use).
- **Defence in depth:** the `exec.run` op itself runs an invocation for a non-person actor only if
  `Approvals::program_trusted` says so. An agent session holding `process` gets a person-confirmation card instead
  (`require_person`, run once if approved); an automation session gets `Denied` with the hint to approve it in the
  Rules panel, because a show waiting on a card in the middle of a stream helps nobody.

**What is never possible from a show:** loading native code (`native`), capturing OS input (`input`; shows consume
`hotkey` events, which the person's input source makes), opening a microphone (ADR 0018; `cond.audio_level` reads
meters only), editing shows or the library, and approving anything.

**A shared or imported show never starts by itself:** autostart is in the machine-local approvals, not the show.

### D12. Groups, enabling and the panic button

```toml
# in shows/<show>/show.toml
[groups.redeems]
name = "Redeems"
enabled = true         # the state when the show starts
```

- A graph belongs to at most one group (`graph.group`). It is **active** when `graph.enabled`, its group (if any) is
  on, it isn't suspended (D8), and the show runs.
- **Two layers:** `enabled` in the document is the person's design-time choice (the Rules table's checkbox, undoable).
  At run time `group.enable`, `group.disable`, `group.toggle` and `automation.switch_group` switch a group's runtime
  state only. A switch takes effect at the end of the current step, so the chain that asked for it completes. A "stream
  starting" group that turns itself off is the typical use. Runtime switches reset at the next start.
- **Deactivating** a graph drops its parked tokens and pending wakes and ignores its replies (counted); reactivating
  sends `Start` again. Simple and deterministic, and nothing half-finished resumes later by surprise.
- **The panic button** (BR-14) calls `automation.pause`: no new tokens start, wakes and replies are dropped and counted,
  and the automation session is paused so in-flight calls are refused. `automation.resume` continues; variables and
  group switches survive a pause, not a stop.

### D13. The ops

**Document ops** (`omuzaki-live`; edit, scope `edit:show`, `Undo::Auto`; the queries need `read`; each edit checks the
resulting show with `ShowDef::check` and the catalogue before returning its patch; consequences ride in the same
patch, as scenes D8):

| Op | Does | Writes |
|---|---|---|
| `show.create {id, name?}` | a new show (`name` from the id); becomes the default if there is none | `shows/<id>`, maybe `project.default_show` |
| `show.list` | query: ids, names, graph counts, which is the default and which runs | |
| `show.describe {show?}` | query: settings, graphs, variables, groups, the show's needs and what is approved | |
| `show.rename {show?, name}`, `show.set_default {show}` | | `show.name`, `project.default_show` |
| `show.delete {show}` | refused while it runs; moves or clears the default in the same edit | `shows/<id>`, `project.default_show` |
| `automation.add_graph {show?, id, name?, group?, graph?}` | a graph; `graph` is a whole `GraphDef` to copy in (templates, paste, import), checked as a unit | `automations/<id>` |
| `automation.set_graph {show?, graph, name?, enabled?, group?, summary?}` | `group: null` leaves the group | `graph` |
| `automation.remove_graph {show?, graph}` | | `automations/<id>` |
| `automation.add_node {show?, graph, kind, id?, params?, pos?, label?, set?}` | `id` defaults to the kind's last segment (`wait`, then `wait-2`); params are checked against the kind | `nodes/<id>` |
| `automation.set_node {show?, graph, node, params?, pos?, label?, set?}` | `params` and `set` merge by top-level key (`null` removes a key), so an agent changes one parameter without restating the rest; wires whose ports no longer exist are removed in the same edit and listed | that node, maybe others' `in` |
| `automation.remove_node {show?, graph, node}` | also removes every wire that names it | that node and the wires |
| `automation.connect {show?, graph, from, to, replace?}` | `"a.out"` to `"b.in"`; refuses a type mismatch, a value input that already has a source (unless `replace`), and a value cycle | `b`'s `in` |
| `automation.disconnect {show?, graph, to, from?}` | without `from`, every wire into `to` | `b`'s `in` |
| `automation.add_group {show?, id, name?, enabled?}`, `automation.set_group {show?, group, name?, enabled?}`, `automation.remove_group {show?, group}` | removing a group clears its graphs' membership in the same edit | `show.groups`, graphs |
| `automation.declare_variable {show?, name, type, scope, default?, summary?}`, `automation.remove_variable {show?, name}` | removing one leaves the nodes that use it to report a finding | `show.variables` |
| `automation.describe {show?, graph?}` | query: the compiled ports of every node, the findings (D18), the needs and their approval state | |
| `automation.node_kinds {category?, search?}` | query: every node kind with its summary, class, category, params schema and its ports for the default params; what an agent builds graphs from | |
| `automation.event_kinds` | query: every event kind with its summary and payload schema | |

**Runtime ops** (`omuzaki-live`; live actions: edit with an empty patch and `Undo::None`, D7; they call the
`AutomationControl` service, which the engine implements):

| Op | Scope | Does |
|---|---|---|
| `automation.start {show?}` | `broadcast` | compiles and starts the show; opens its session |
| `automation.stop` | `perform` | `show.stopping`, at most 2 s for in-flight calls, `Stop` to every node, flush, end the session |
| `automation.pause`, `automation.resume` | `perform`, `broadcast` | D12. Quieter never asks; louder needs what starting needs |
| `automation.status` | `read` (query) | running show, seed, session, counts, suspended graphs, faulted and inert nodes, missing approvals |
| `automation.values` | `read` (query) | D10 |
| `automation.set_variable {name, value}` | `perform` | type-checked; announced as `var.changed` |
| `automation.switch_group {group, on}` | `perform` | D12 |
| `automation.resume_graph {graph}` | `perform` | clears a loop-guard suspension |
| `automation.fire {kind, payload}` | `perform` | a test event; its effects run as the caller (D7) |

**Effect ops** (`omuzaki-live-nodes`, with their nodes, D11): `net.http`, `net.udp_send`, `file.read_text`,
`file.write_text`, `file.append_text`, `file.stat`, `exec.run`, `system.process_running`. Agents and scripts may call
them with their own grants.

### D14. Templates and the library `show` target

- **The target:** `show` binds `$show` to `shows/<id>`: the caller's `show`, else the project's default show (D2).
  Records go to `shows/<id>/applied/<app>.toml`; `applied` is `ShowDef`'s only sibling key. Safety rule 1 of the library
  (D3 of `docs/design/library.md`) adds `edit:show` to the scopes a recipe step may need; rule 2 confines every change
  to `shows/<id>/`.
- **An automation template is a `member` item** in the `automations` section: one `automation.add_graph` step whose
  `graph` argument is the whole graph, with `$params.<key>` placeholders as exact string values (a reward title, a
  cooldown) and `$name` as the graph id:

```toml
schema = 1
id = "omuzaki/automations/bonk"
version = "1.0.0"
section = "automations"
kind = "member"
target = "show"
name = "bonk"
title = "Bonk"
summary = "A channel-points redeem throws a pillow at your avatar, with a cooldown and a running count."
origin = "BR-11 template list (spec v3 §25); values designed in docs/design/automation.md §4"
rights = { licence = "CC0-1.0", commercial = "ok" }
params.reward = { type = "text", default = "Bonk", summary = "The reward's title, as set on the platform." }
params.cooldown = { type = "number", default = 10, min = 0, max = 600, summary = "Seconds between bonks." }

[[step]]
op = "automation.add_graph"
args = { show = "$show", id = "$name", graph = { graph = { name = "Bonk" }, nodes = { ... } } }
```

  (`nodes` holds the six nodes of §4, with `"$params.reward"` and `"$params.cooldown"` where the values go; the
  library's placeholders are exact strings, but both layers read `$`: the library replaces every string that starts
  with `$` (D3 of `docs/design/library.md`; a literal one is written `$$...`), and `op.run` reads `$in.<name>` from the
  stored node. So a template writes the engine's placeholders **escaped**, `"$$in.text"`, and applying turns that into
  the stored `"$in.text"` the engine then reads. Unescaped, the library would refuse the step (`in` is no parameter).
  The seed-template task tests this by **applying each template through the library and running the
  result**: the applied nodes contain `"$in.…"` exactly, and the golden run passes.)
- **Applying is one undo step;** removing the application removes the graph folder it created (ADR 0019 D6).
- **A template with risky nodes** applies like any other: applying only writes the document, and its risky nodes are
  inert until the person approves them (D11). So a "Streamer.bot bridge" template is safe to ship.
- **First-party templates** live in `crates/omuzaki-live/library/seed/automations/<name>.toml`, one file each,
  embedded and registered by the crate's library source (`docs/design/library.md` D14; `omuzaki-live` owns
  `automation.add_graph`, so it owns the items). The seed set (§7): bonk, hat redeem, raid welcome and a death counter;
  AP-10's and AP-11's templates add files to the same folder.

### D15. Plugins (PL-03): the seam

- **Kinds at run time.** The engine's catalogue is the static registrations plus `DynNodeKind` objects the plugin host
  (`omuzaki-plugin`, PL-01) adds and removes as plugins load. Plugin node ids are `plugin.<plugin>.<name>` and plugin
  event kinds `plugin.<plugin>.<kind>`, so they never collide with first-party ones; plugins ship their own Fluent
  resources (`docs/design/l10n-ops.md` §3.1).
- **Same contract.** The WIT interface mirrors `NodeKind`, `Node`, `Input` and `NodeCx`. A plugin node runs on the
  engine thread through wasmtime with a fuel cap per activation (1 ms of work by default); running out faults the node
  (D5). It has no host calls beyond `NodeCx`, so its only effects are ops, through the show's session, intersected with
  the plugin's own grants (ADR 0020: plugins have sessions).
- **Needs are declared** in the plugin's manifest per node and approved per show like first-party needs (D11).
- **Adapters** (Kick, tip services, sensors) publish through `EventSink` from the plugin host, with
  `Source::Plugin { id }` and, for chat, the normalised kinds (D9).
- A missing plugin leaves its nodes inert with a finding naming the plugin; the graph keeps its files.

### D16. Testing: `NodeHarness`, `FakeRunner`, `ShowHarness`

`omuzaki-live-mock` (the node tests) and `omuzaki-live` (the whole-show tests):

```rust
pub struct NodeHarness { /* one instance, a FakeRunner, FakeClock + FakeWall, MemoryVariables, groups */ }
impl NodeHarness {
    pub fn of<K: NodeKind>(params: K::Params) -> Self;
    pub fn by_id(kind: &str, params: serde_json::Value) -> Result<Self, ParamErrors>;  // through the registry
    pub fn seed(self, seed: u32) -> Self;
    pub fn starting_at(self, at: Seconds, utc: UtcMillis) -> Self;
    pub fn with_var(self, name: &str, def: VariableDef, value: Val) -> Self;
    pub fn with_group(self, group: &str, on: bool) -> Self;
    pub fn start(&mut self);                                      // Input::Start
    pub fn set_input(&mut self, port: &str, value: impl Into<Val>);   // what the input reads from now on
    pub fn send(&mut self, port: &str);                           // a pulse at the current time, in a fresh token
    pub fn trigger(&mut self, event: LiveEvent);                  // to a trigger
    pub fn advance(&mut self, by: Seconds);                       // runs due wakes and scripted replies, in order
    pub fn advance_to(&mut self, at: Seconds);
    pub fn respond(&mut self, op: &str, latency: Seconds,
                   answer: impl FnMut(&serde_json::Value) -> Result<Option<serde_json::Value>, OpError> + 'static);
    pub fn reply(&mut self, call: CallId, outcome: Result<Option<serde_json::Value>, OpError>);
    pub fn read(&mut self, port: &str) -> Result<Val, NodeError>; // a live output
    pub fn fired(&self) -> &[Fired];                              // Fired { at, port, values: BTreeMap<PortName, Val> }
    pub fn calls(&self) -> &[Call];                               // Call { id, at, op, args }
    pub fn failures(&self) -> &[Failure];                         // Failure { at, port, text: Text }
    pub fn emitted(&self) -> &[LiveEvent];
    pub fn var(&self, name: &str) -> Option<Val>;
    pub fn group(&self, group: &str) -> Option<bool>;
    pub fn log_json(&self) -> String;                             // everything above, for the determinism check
    pub fn stop(&mut self);
}
```

- **One node, one PR.** A node task's tests are `NodeHarness` tests: scripted inputs at fake times, the op calls it
  makes, scripted replies, the outputs it fires. No engine, no executor, no network.
- **`FakeRunner`** answers `run_op` calls from `respond` scripts (default: unanswered, so a test sees the call);
  replies arrive at `now + latency` as inbox items, so they are ordered exactly like the engine orders them.
- **`ShowHarness`** (`omuzaki-live`) runs a whole `ShowDef` on the real engine with a `FakeRunner`, `FakeApprovals` and
  scripted events: `publish(event)`, `advance(..)`, `requests()` (the op requests as golden JSON), `values()`. The
  engine tests use it with test node kinds. Template runs need the real nodes too, and an implementation crate can't
  depend on another (not even for tests), so they live in `omuzaki-test`, the app crate that links `omuzaki-live` and
  `omuzaki-live-nodes` together. Running the same script twice and comparing `log_json` is the D6 test, required in the
  engine and template tasks.
- **Fixtures** are synthetic (invented names and messages). Recorded chat from a real stream is never a fixture.

### D17. Editing: the graph editor, the Rules table, accessibility

- **The graph editor** is a center tab of the Broadcast workspace, built on `egui-snarl` 0.12.0 (MIT OR Apache-2.0,
  for egui 0.36 like the workspace; its dependencies `ahash`, `egui-scale`, `smallvec` (MIT OR Apache-2.0) and `slab`
  (MIT); already cleared in `docs/licensing/register.md`). The document is the truth: the tab rebuilds its `Snarl`
  from the graph when the document version changes, and every edit is an op (`automation.add_node`, `connect`, ...).
  A drag sends `automation.set_node {pos}` per frame inside one transaction, Esc rolls it back. Pins are
  coloured by port type; incompatible pins don't accept a wire, and the reason shows on hover. While the show runs,
  nodes that just fired glow briefly (from `automation.values`), suspended graphs and inert or faulted nodes carry a
  badge, and hovering a wire shows its last value.
- **The inspector** shows the selected node's parameters as a form generated from its JSON Schema (the Inspector's
  usual widgets) and its unwired value inputs as `set` constants.
- **The Rules table** (Broadcast's right panel, `docs/design/ux/shell.md` §3) lists the show's graphs: name, trigger
  summary, group, enabled, state (running, suspended, needs approval). A graph that is a straight chain (one trigger,
  then one node after another with one event wire each, value inputs from constants or the trigger) is shown as a row
  "When <trigger> → <actions>" and edited inline; anything else opens in the editor. One storage model serves both
  (a table for simple tasks, the graph as the power layer). The show's
  approval card and Start, Pause and Stop live here.
- **Accessibility (SE-02):** every node and wire is reachable by keyboard; nodes are added from a searchable palette and
  wired through a "Connect to…" list; the Rules table and `automation.describe` are the screen-reader view of a graph.
- **Agents** build graphs with the same ops, read `automation.node_kinds` and `automation.describe`, and try them with
  `automation.fire` under their own grants.

### D18. Validation and findings

| Refused by the document ops | Reported as a finding (the edit goes through) |
|---|---|
| unknown keys; ids that aren't slugs; a wire naming a node or port that doesn't exist | a node kind not registered in this build (a missing plugin): the node is inert |
| a value input with two sources; a port in both `in` and `set`; an event wired to a value port | params that no longer pass the kind's check (after an app update): inert |
| a type mismatch on a wire or a `set` constant | needs not approved on this machine: inert |
| a value cycle | an `op.run` whose op isn't registered, isn't allowed (D7) or whose arguments don't decode: inert |
| params failing the kind's schema or check; limits (D8) | a variable a node names isn't declared, or a global's type conflicts: inert |
| a graph in a group that doesn't exist | a trigger on an event kind nobody registered: inert |
| | an event input nothing is wired to (the node never runs) |

The engine compiles with the same checks, so a show edited by hand on disk loads with findings instead of failing.

### D19. Performance

Automations are off the real-time path: no frame or audio thread waits on them, and the frame thread never takes the
executor's lock. Their CPU counts inside `cpu.total` (§10). Budgets, each with a criterion bench on `Dev` pinned to one
E-core (the §10 rule for `R`):

| Measure | Budget |
|---|---|
| One step of §4's bonk graph (a trigger, five nodes, two op requests) | ≤ 15 µs median |
| 100 chat messages per second into a show of 20 graphs with `chat.command` triggers | ≤ 10 ms of engine CPU per second (0.125 % of 8 cores) |
| Event published to its op request handed to the dispatcher | ≤ 0.5 ms p95, the engine thread's wake-up included |
| An idle show (no timers due, no events) | 0 steps and no wake-ups over 60 s of fake time |
| A compiled show of 50 graphs × 20 nodes | ≤ 8 MB, compiled in ≤ 20 ms |

The first measurement may tune a number once, with the maintainers' sign-off.

### D20. Compatibility

- Every type here is new, so nothing is migrated. `ShowDef` and `GraphDef` follow `omuzaki-doc`'s migrations like any
  content; node parameters follow their kind: a kind may accept an older parameter shape and normalise it in `check`,
  and a breaking change gets a new kind id (`flow.wait2`) while the old one keeps working for saved shows.
- Op arguments used by templates follow ADR 0019 D16 (aliases, defaults, step migrations).
- Event payloads are additive: new optional fields only. A renamed field is a new kind.
- Spec §7's `shows/<id>.toml` never existed on disk, so the folder layout needs no conversion.

## 4. Worked example: a show with a bonk redeem and a Streamer.bot bridge

What `show.create {id: "main"}`, `automation.declare_variable {name: "bonks", type: "number", scope: "show", default:
0}`, `automation.add_group {id: "redeems", name: "Redeems"}`, `library.apply {item: "omuzaki/automations/bonk"}` and
an agent's five edits building the bridge leave behind:

```
shows/main/show.toml
shows/main/automations/bonk/graph.toml
shows/main/automations/bonk/nodes/redeem.toml    on.event   (trigger: the "Bonk" redeem)
shows/main/automations/bonk/nodes/cool.toml      flow.cooldown
shows/main/automations/bonk/nodes/count.toml     var.add
shows/main/automations/bonk/nodes/throw.toml     op.run     (BR-12's throw)
shows/main/automations/bonk/nodes/line.toml      text.format
shows/main/automations/bonk/nodes/say.toml       op.run     (BR-10's chat send)
shows/main/automations/lurk/graph.toml
shows/main/automations/lurk/nodes/cmd.toml       chat.command
shows/main/automations/lurk/nodes/post.toml      net.http   (to Streamer.bot on this PC)
shows/main/applied/bonk.toml                     the library record (pin, params, what it wrote)
project.toml: default_show = "main"
```

```toml
# shows/main/show.toml
name = "Main"

[groups.redeems]
enabled = true
name = "Redeems"

[variables.bonks]
default = 0
scope = "show"
summary = "How many times the bonk redeem landed."
type = "number"
```

```toml
# shows/main/automations/bonk/graph.toml
group = "redeems"
name = "Bonk"
```

```toml
# shows/main/automations/bonk/nodes/redeem.toml
kind = "on.event"
pos = [0.0, 0.0]

[params]
kind = "redeem"
where = { "reward.title" = "Bonk" }   # case-insensitive after NFKC, like chat commands
```

```toml
# shows/main/automations/bonk/nodes/cool.toml
kind = "flow.cooldown"
pos = [220.0, 0.0]

[in]
in = "redeem.fired"

[set]
seconds = 10.0
```

```toml
# shows/main/automations/bonk/nodes/count.toml
kind = "var.add"
pos = [440.0, 80.0]

[in]
in = "cool.out"

[params]
variable = "bonks"
```

```toml
# shows/main/automations/bonk/nodes/throw.toml
kind = "op.run"
pos = [440.0, -80.0]

[in]
in = "cool.out"

[params]
op = "throw.item"                    # BR-12's op; its name is BR-12's to fix
args = { item = "pillow", at = "head", strength = 0.8 }
```

```toml
# shows/main/automations/bonk/nodes/line.toml
kind = "text.format"
pos = [660.0, 80.0]

[in]
n = "count.value"
user = "redeem.user_name"

[params]
template = "Bonk #{n} from {user}!"
```

```toml
# shows/main/automations/bonk/nodes/say.toml
kind = "op.run"
pos = [880.0, 80.0]

[in]
in = "count.done"
text = "line.text"

[params]
op = "chat.send"                     # BR-10's op
args = { text = "$in.text" }
```

**What happens when Kiri redeems "Bonk" at t = 12.0 s** (one step; the order is D5's):
1. `redeem` (the only trigger on `redeem`) matches the title, starts token T rooted at the event, sets `user_name =
   "Kiri"`, `reward_title = "Bonk"` and the rest of the payload's ports into T, and fires `fired`.
2. `cool` gets `in`; no bonk in the last 10 s, so it starts its cooldown and fires `out`.
3. `out` goes to `count` and `throw`, in node-id order: `count` first. `count` adds 1 to `bonks` (now 4), sets `value =
   4` in T and fires `done`, whose whole subtree runs next: `say` gets `in`, reads `text`, which pulls the pure `line`
   in T (`n = 4` from `count`, `user = "Kiri"` from `redeem`) and gets "Bonk #4 from Kiri!", then requests `chat.send
   {text: "Bonk #4 from Kiri!"}`.
4. Back up the stack, `throw` requests `throw.item {item: "pillow", at: "head", strength: 0.8}`.
5. Step end: the two requests go to the ordered lane in that order, with cause `main/bonk/say on redeem` and
   `main/bonk/throw on redeem`. `var.changed {name: "bonks", old: 3, new: 4}` is queued for the next step, and
   the show's values file is written within the second.

A second redeem at t = 15.0 s reaches `cool` while it cools down: `cooling` fires with `left = 7`, nothing is wired to
it, and the firing is in the values monitor. At t = 22.0 s the next one goes through.

```toml
# shows/main/automations/lurk/nodes/cmd.toml
kind = "chat.command"
pos = [0.0, 0.0]

[params]
name = "lurk"
user_cooldown_s = 60
```

```toml
# shows/main/automations/lurk/nodes/post.toml
kind = "net.http"
pos = [260.0, 0.0]

[in]
in = "cmd.fired"
user = "cmd.user"

[params]
method = "POST"
url = "http://127.0.0.1:7474/DoAction"
headers = { Authorization = "Bearer {secret:streamerbot}" }
body = { json = { action = { name = "Lurk" }, args = { user = "$in.user" } } }
```

The agent that added `post` could not make it run: compiling the show finds three needs nobody approved, so `post` is
inert, and the Rules panel shows one card: "**Main** wants to: send HTTP requests to `127.0.0.1:7474`; send your
secret `streamerbot` to `127.0.0.1:7474`." The person clicks Allow on this machine; the approvals file (outside the
project) records `Host { http, 127.0.0.1, 7474 }`, `Secret { streamerbot, 127.0.0.1 }` and `Scope(net)`; the next
compile arms the node and the session's ceiling gains `net`. A friend who copies this project gets the graph and an
inert `post` until they approve it themselves.

## 5. Crates and contract changes

| Crate | Change |
|---|---|
| `omuzaki-live-api` (new, API) | D4, D9, D10, D11 types; the normalised payloads; the service traits; `paths`; limits. Needs maintainer review |
| `omuzaki-live-mock` (new, mock) | D16 |
| `omuzaki-live` (new, implementation) | the engine, stores, ops, `ShowHarness`, templates |
| `omuzaki-live-nodes` (new, implementation) | the catalogue (§6) and the effect ops |
| `omuzaki-ops-api` | `Actor::Automation { id }` (`System` stays Omuzaki's own); the `edit:show` scope, in `Grants::agent_default()` like the other edit scopes. Both additive |
| `omuzaki-ops` | live actions (edit, empty patch, `Undo::None`) are logged and announced but never enter an undo history or bump the version; automation sessions use `Actor::Automation` and their steps are left out of per-principal undo |
| `omuzaki-l10n-api` | the crate check walks node kinds and event kinds (`node-`, `event-`) |
| `omuzaki-library-api` | the `show` target is lifted from reserved; `$show`; `edit:show` joins safety rule 1 |
| `omuzaki-ui` | the graph editor tab, the inspector form for node params, the Rules panel with the approval card, the values monitor |
| Spec | §6 lists `edit:show`, `process` and `input`; §7 shows the show folder and `default_show`; §9's `omuzaki-live` row names the four crates (this PR) |

New dependencies, each checked against `docs/licensing/policy.md` in its task and added to the register there:
`ureq` 3 (MIT OR Apache-2.0) with `rustls` (Apache-2.0 OR ISC OR MIT), its `ring` provider (Apache-2.0 AND ISC) and
`rustls-platform-verifier` (MIT OR Apache-2.0), never `webpki-roots` (CDLA-Permissive-2.0 is not on our allow list);
`jiff` (Unlicense OR MIT) for time zones; `sysinfo` (MIT) for the process condition; `egui-snarl` 0.12.0 (MIT OR
Apache-2.0, already in the register). Versions and licences read from crates.io on 2026-10-12. Why `ureq` and not
`reqwest` (in the workspace for the MCP tests only, without TLS): the HTTP op is a blocking job on a job worker
thread, so a blocking client keeps an async runtime out of the nodes crate. A loopback host (`127.0.0.1:7474`, the
Streamer.bot bridge) needs approval like any other: local services can do as much as remote ones.

## 6. The node catalogue

Every BR-26 family in the spec row, with its node ids and the task that builds it (folders are
`crates/omuzaki-live-nodes/src/<folder>/`):

| Folder | Node ids | Spec words | Task |
|---|---|---|---|
| `flow` | `flow.wait`, `flow.random`, `flow.sequence` (the next output on each event), `flow.switch` | waits; random, sequence and switch |       |
| `flow` | `flow.branch` (on a bool), `flow.then` (outputs in order), `flow.cooldown`, `flow.gate` | (building blocks the templates need) |       |
| `logic` | `logic.and`, `logic.or`, `logic.not` | conditions |       |
| `math` | `math.op`, `math.map_range`, `math.ease`, `math.compare` (pure: a domain error such as a zero divisor gives `value = 0`, `ok = false` and an `error` text, never NaN); `math.random_range` (on an event, so pure nodes stay pure) | math and easing |       |
| `text` | `text.format`, `text.to_number`, `text.contains` | (templates) |       |
| `vars` | `var.get` (pure), `var.set`, `var.add`, `var.toggle`, `var.changed` (trigger); each names `variable` and its `type`, so ports are typed without reading the show | variables with persistence |, engine |
| `time` | `timer.every`, `timer.once`, `timer.stopwatch`, `schedule.at`, `cond.time_between`, `cond.date` | timers and schedules; date and time |       |
| `audio` | `cond.audio_level` (polls `audio.meters`) | audio level |       |
| `watch` | `cond.file_exists`, `cond.file_changed`, `cond.process_running` (and the op `system.process_running`) | file, process |       |
| `file` | `file.read`, `file.write`, `file.append` (and `file.*_text`, `file.stat`) | file |       |
| `net` | `net.http`, `net.udp_send` (and their ops); secret headers | HTTP, UDP |              |
| `exec` | `exec.run` (and its op) | run a program |       |
| `chat` | `chat.command` (`fired`, `denied`, `cooling`; `user`, `user_id`, `login`, `roles`, `platform`, `args`, `rest`, `left`) | chat commands with permissions and cooldowns |       |
| `groups` | `group.enable`, `group.disable`, `group.toggle`, `group.is_enabled` | group enable and disable |       |
| `op` | `op.run` | (every op as an action: BR-11) |       |
| `event` | `on.event` (any kind, `where` and `min` filters), `event.emit` | (BR-11 event → condition → action) |       |
| `text/filters`, `tts` | the chat reader's nodes | AP-11 | (decides) |
| | the values monitor | a values monitor showing live variables |, engine |

**`on.event`'s ports** come from the kind's payload schema, deterministically: each top-level property that is a
string, number, boolean or array becomes a port of that type; an object property contributes its own scalar and array
properties one level down as `<property>_<field>` (so `redeem` gives `user_name`, `user_id`, `user_roles`,
`reward_title`, `reward_cost`, `input`, ...), and a deeper object becomes a json port. The value ports are sorted by
name (so schemars' property order never matters), then come `event` (the whole payload, json) and `fired`. A missing
optional field reads as its type's default. `where` compares a dotted path to a literal (text case-insensitively
through `common::fold`: NFKC, then lowercase; every case-insensitive match in the crate uses it, so chat commands,
filters and `where` agree); `min` compares a number path (`min = { bits = 100 }`).

## 7. The task cut

Milestone: Phase 2 (perform + broadcast), as BR-11
and BR-26, except where the parent's milestone differs.

**The contract**

| Task |
|---|
| `omuzaki-live-api` skeleton (every module declared), ids, `ShowDef`, `GraphDef`, `NodeDef`, wires, `VariableDef`, `GroupDef`, `paths`, keys, structural `check`, limits |
| `omuzaki-live-api` events: `LiveEvent`, `Source`, `EventPayload`, `register_event_kind!`, `EventSink`, the normalised payloads and `ChatMessage` |
| `omuzaki-live-api` nodes: `Val`, ports, `NodeKind`, `Node`, `Input`, `NodeCx` and `NodeHost`, `register_node!`, `DynNodeKind`, `Need`, `Invocation`, errors |
| `omuzaki-live-api` services: `AutomationControl` with its status and values types, `Approvals` and `ShowApprovals`, `VariableStore`, `WallClock` |
| `omuzaki-live-mock`: `NodeHarness`, `FakeRunner`, `FakeWall`, `MemoryVariables`, `FakeApprovals`, fixtures |
| ops-api: `Actor::Automation` |
| ops-api: the `edit:show` scope |
| omuzaki-ops: live actions (edit, empty patch, `Undo::None`) are logged but never enter an undo history |
| l10n: `node-` and `event-` registries in the crate check |
| CN-05's secret store, **pulled into Phase 2** (§8 answer 5): `omuzaki-secrets-api`, the OS keychain implementation, an in-memory mock |

**The engine and the ops** (`omuzaki-live`)

| Task |
|---|
| the crate skeleton; compile a show (catalogue, params, ports, wires and types, value cycles, needs against approvals, findings); hot-reload diff |
| the step loop: items in time order, tokens, depth-first run to completion, value resolution, wakes, rng streams, faults, hot-reload swap; `ShowHarness`; the §4 example and the determinism test |
| the loop guard and the engine's limits (D8), groups and enabling at run time (D12), the D19 benches |
| dispatch: the automation session, request building (actor, cause), the ordered and job lanes, in-flight limits, replies, manual events as their actor, pause with the session |
| the inbox behind `EventSink`: bounds, droppable kinds, dedup, the recent-events ring, refusing `Source::Manual` |
| variables: values per scope, the disk stores (show, global), debounced atomic writes, flush, load, type checks, global conflicts, `var.changed` coalescing |
| approvals: the machine-local store, `program_trusted`, the session ceiling from approvals, autostart, revoke, the UI-only allow |
| runtime ops and `AutomationControl`; the values snapshot mailbox |
| `show.create`, `show.list`, `show.describe`, `show.rename`, `show.set_default`, `show.delete`; `default_show` |
| `automation.add_graph`, `set_graph`, `remove_graph`; groups; variable declarations |
| `automation.add_node`, `set_node`, `remove_node`, `connect`, `disconnect` |
| `automation.describe`, `automation.node_kinds`, `automation.event_kinds` |

**The nodes** (`omuzaki-live-nodes`), as §6, each unless noted:

| Task |
|---|
| the crate, a stub file per node family (so node tasks never share a file), `common::fold`, the l10n test; `flow.wait`, `flow.random`, `flow.sequence`, `flow.switch` |
| `flow.branch`, `flow.then`, `flow.cooldown`, `flow.gate`; `logic.and`, `logic.or`, `logic.not` |
| math and easing |
| `text.format`, `text.to_number`, `text.contains` |
| the variable nodes (persistence is) |
| timers, schedules, date and time (jiff) |
| `cond.audio_level` over the `audio.meters` query |
| file and process conditions; the `system.process_running` op |
| the `file.*` ops and nodes |
| the `net.http` and `net.udp_send` ops and nodes |
| secret headers |
| the `exec.run` op and node |
| `chat.command` |
| the group nodes |
| `op.run` |
| `on.event`, `event.emit` |

**The editor, the panels and the library**

| Task |
|---|
| the graph editor tab on egui-snarl |
| the node inspector: params form from the schema, `set` constants, findings |
| the Rules panel: the graph table, inline simple rules, the approval card, Start, Pause, Stop |
| the values monitor (re-pointed at `automation.values`) |
| the library's `show` target, and `omuzaki-live`'s library source for the seed templates |
| seed templates: bonk, hat redeem, raid welcome, death counter, with golden runs in `omuzaki-test`; needs the ops they call (BR-10, BR-12, BR-21, ST-03's props) |

**Re-checked elsewhere:** AP-10's listener publishes `voice.command` through `EventSink` without the
transcript, and its phrase list is a `User` setting in the person's profile, not the show (§8 answer 3); its template
needs the secret store (§8 answer 5). AP-11's nodes move to `omuzaki-live-nodes`; the reader's speech is the `tts.say`
op, not a node with audio inside (§8 answer 4), whose arguments and queue the TTS host designs. The Rules panel also
takes a missing secret (§8 answer 5).

## 8. Answers to the open questions (2026-10-12)

The five questions this design asked, answered and folded into the decisions above:

1. **An unapproved show gets `read` + `perform` only.** It can animate the avatar (expressions, props, throws) and
   read state, because those stay inside Omuzaki's output and stop with the show. It can't edit the project or
   settings, broadcast, publish, spend, or use `net`, `fs`, `process` or `input` until the person approves that on
   this machine (D7's ceiling, D11).
2. **Variable values stay on this machine** (`.omuzaki/` and the user data folder). A show carries only its declared
   variables and their defaults, never values, so a counter doesn't follow a git clone or a shared show (D10).
3. **AP-10's phrase lists live in the person's profile,** as a `User` setting beside the voice calibration (the
   settings registry): voice and language are personal, so the same show works in each person's language and
   accent. Shows and templates refer to command ids (`wave`, `nod`, ...); templates ship default phrases per id.
   The listener design is updated to match.
4. **AP-11's speech is an op,** `tts.say`, registered next to the TTS host with the queue there: scoped, logged and
   rate-limited like every action (D7). The node is a thin caller of it, or plain `op.run`. The TTS host's design owns the op's
   arguments and the queue, and the reader nodes move with it.
5. **CN-05's secret store lands no later than the first node that needs a secret.** Phase 2 needs tokens (the
   Streamer.bot and Home Assistant headers of `net.http` and BR-19's OBS password), so the keychain store is
   **pulled into Phase 2** (`omuzaki-secrets-api`, the OS keychain implementation and an in-memory mock).
   CN-05's key-management UI for AI providers stays in its own phase. A show names a secret by slug
   (`{secret:streamerbot}`), stored under the label `automation/<name>`; a person enters a missing one in the Rules
   panel's approval card, never through an op, because an op's arguments are logged.

## 9. Sources

Read on 2026-10-12: spec v3 §3, §4, §6, §7, §8, §9, §10, §22, §23, §25, §30, §31, §39;
`docs/design/{scenes,ops-api,agent-sessions,library,l10n-ops,audio-api}.md`, `docs/design/ux/{shell,principles}.md`;
ADRs 0009, 0013, 0015, 0018, 0019, 0020, 0024; `crates/omuzaki-ops-api/src/{actor,runner,scope,
ctx,registry,lib}.rs`, `crates/omuzaki-core/src/{time,rng,id}.rs`, `crates/omuzaki-doc/src/lib.rs`,
`crates/omuzaki-char-api/src/kind.rs` (the registration pattern), the library-api branch (`Target::Show`, `check_ops`),
`omuzaki-stage-api`'s layout; crates.io metadata for egui-snarl, jiff, ureq, rustls, ring, aws-lc-rs,
rustls-platform-verifier, webpki-roots, sysinfo; the VNyan, Warudo and Node-RED execution models
(their public docs).
