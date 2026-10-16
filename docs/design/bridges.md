---
title: "Compatibility bridges: the VTube Studio-compatible API shim, and how VMC and OSC fit (PL-05)"
summary: "the VTube Studio-compatible API shim (ADR 0066): where it lives, which requests it answers or refuses and with which error ids, the tool's session, pairing, token and revocation, injected parameters in the tracker layer, safety caps, naming, ops and settings; how VMC (PF-04) and OSC (PF-05) fit; tests and the task cut."
area: design
---

# Compatibility bridges: the VTube Studio-compatible API shim, and how VMC and OSC fit (PL-05)

**Status:** proposed. **Feature:** PL-05 (#529). **Decision record:** [ADR 0066](../decisions/0066-bridges.md).
**Spec:** v3 §30 (PL-05), §14 (CN-13), §22 (PF-04, PF-05, PF-23), §6 (ops, scopes), §31 (SE-04), QG-10.
**Builds on:** ADR 0020 and `agent-sessions.md` (sessions, `Need::Pairing`, revocation), `plugins.md` §3
(`Transport::Plugin` sessions, live revocation), `settings.md` (`SettingScope`, `AgentWrite`), `anim-api.md` and
`omuzaki-anim-api::blend` (PF-23's layer order), `input-motion.md` §5 (`BindingHit`), `automation.md` D7 and D9 (live
actions; the event envelope, pending #729).
**Research (the clean-room sources):** `docs/research/2026-10-vts-shim-terms.md` (the shim note: terms, wire identity,
what tools send, the refusal table) and `docs/research/2026-10-vts-plugin-api-terms.md` (the CN-13 note).

A **bridge** is a listener that lets a program written for another app's protocol drive Omuzaki. PL-05 has one bridge
of its own, the shim: a server that answers the documented VTube Studio plugin API, so a stream tool written for it
(Streamer.bot, Karasubonk-style tools) can trigger hotkeys and inject parameters into an Omuzaki character. VMC and OSC
are PF-04 and PF-05's; this design states only the rules they share with the shim (§9).

## 1. Decisions in one screen

| Question | Decision | § |
|---|---|---|
| Where the shim lives | `crates/omuzaki-vts/src/server/**`, beside CN-13's client; one WebSocket crate (#726's) for the client, the shim and BR-19 | 2 |
| Who a tool is | One `Transport::Plugin` session per authenticated connection, client `vts-<name>`, actor `<client>#<id>` as the executor builds it, principal the person who paired (stored with the token), ceiling `perform` and nothing else | 4 |
| Pairing | `Sessions::request_pairing` (new, additive, task S0) pauses the session `Pairing` and queues `Need::Pairing`; only a person in Omuzaki's window pairs; the token's keyed hash is kept machine-wide | 4 |
| What reaches Omuzaki | Only ops through the `Runner`, as the tool's session: live actions for hotkeys and expressions, `vts_server.catalog` for lists, `vts_server.feed` to claim parameters, `vts_server.param_create` and `param_delete` | 3, 5 |
| Contract change | One additive change to `omuzaki-ops-api` (S0, Director's approval): `Sessions::request_pairing` and `Sessions::gate`, both with default bodies that return `Unsupported` | 4, 5.1 |
| Injected parameters | The existing `Source::Tracker` layer, merged on the frame thread from a wait-free table; set, add, weight, `faceFound` and the 1 s keep-alive honoured; under 10 µs a frame, no allocation | 5 |
| Safety | Off by default; person-only start; loopback only, `Origin` and `Host` checked; no UDP discovery; 64 KiB messages, 4 clients, rate caps; no `read` | 6 |
| Naming | "VTube Studio-compatible API"; Omuzaki's own name in every product field; the non-affiliation line; default port 8011, not 8001 | 7 |
| VMC and OSC | PL-05 adds no tasks; PF-04 and PF-05 build them under the same bridge rules; QG-10's targets in §9 | 9 |

## 2. Where it lives, and the layering

The shim is a module of `omuzaki-vts` (an Implementation crate, `wasm = false`): it shares the message types, limits
and validators CN-13 already has (`protocol`, `limits`) and the WebSocket crate #726 adds. A separate crate would
duplicate the types or depend on `omuzaki-vts`, which implementation crates may not do. The modules stay disjoint:
CN-13 owns `src/client/**`, `src/testing/**`, `src/ops/**`; PL-05 owns `src/server/**` and `src/shim_ops.rs`.

```
 tool ──ws──► omuzaki-vts::server ─┬─► Runner::run (session actor)  ─► executor ─► live-action ops (PF-25), catalog, feed
   (loopback)  listener, session,  ├─► Sessions (open, pause, end)       (scope, limits, approvals, audit)
               auth, requests      ├─► PairingStore (keyed hashes)
                                   └─► InjectWriter ──wait-free──► stage frame thread ─► Tracker layer (PF-23)
```

`omuzaki-vts` depends on `omuzaki-ops-api` (`Runner`, `Sessions`, `Request`, `Grants`) and `omuzaki-anim-api`
(`ChannelBuf`, `ChannelMask`, `Channel`): both API crates. The App (`omuzaki-ui`'s desktop shell) builds the `ShimLink`
and hands the `InjectReader` to the stage; the stage may depend on `omuzaki-vts` because it is an App.

**One WebSocket crate.** #726 chooses it for the mock and the client; the shim needs its server side too, and must be
able to inspect the upgrade request's headers before accepting (§6). BR-19's `omuzaki-obs` reuses the same crate. A
crate that cannot do a header-checked server accept is not acceptable for #726.

## 3. What the shim answers, maps and refuses

Error ids are `Files/ErrorID.cs` ([DenchiSoft/VTubeStudio](https://github.com/DenchiSoft/VTubeStudio/blob/master/Files/ErrorID.cs),
read 2026-10-15); request semantics are the [README](https://github.com/DenchiSoft/VTubeStudio#api-details) (read
2026-10-15). The refusal table of the shim note §5 is adopted as written, with the changes below.

**The mapping.**

| VTube Studio | Omuzaki | How |
|---|---|---|
| model | the live scene's subject cast member (`scenes.md` D4) | `modelName` and `vtsModelName` = the character's display name; `modelID` = an opaque 32-hex id, a keyed hash of the `MemberId` (never the id itself); Live2D fields empty or zero |
| hotkey | a PF-25 trigger, an expression toggle, or a clip | `hotkeyID` = an opaque stable id (keyed hash of the target's id); `type` = `ToggleExpression` for triggers and expressions, `TriggerAnimation` for clips; `name` = the target's name |
| expression file | an expression of the character | `file` = `<name>.exp3.json` (a name the tool can send back, not a path); `active` from the live trigger state |
| input parameter | a channel, the head pose or the gaze | the fixed table `server::params::DEFAULT_PARAMS` (§5.2) |

**Answered (phase 1).** `APIStateRequest`; `AuthenticationTokenRequest`, `AuthenticationRequest` (§4);
`CurrentModelRequest`, `AvailableModelsRequest` (the subject only); `HotkeysInCurrentModelRequest`,
`HotkeyTriggerRequest` (by id or name); `ExpressionStateRequest`, `ExpressionActivationRequest` (fade clamped to
0..=2 s, `FadeTime`); `InputParameterListRequest`, `ParameterValueRequest`, `ParameterCreationRequest`,
`ParameterDeletionRequest`, `InjectParameterDataRequest` (§5); `EventSubscriptionRequest` (§8); `StatisticsRequest`
(uptime, frame rate, `allowedPlugins` = paired tools, `connectedPlugins`; window fields zero); `PermissionRequest`
(lists both documented permissions as not granted).

**Every request that changes anything runs one op** through `Runner::run` with `actor` = the session's actor and
`grants` = `{perform}`, so the executor checks the live session, scope, limits and pause:

| Request | Op | Kind |
|---|---|---|
| `HotkeyTriggerRequest` | PF-25's "press a trigger" or "play a clip" live action (#478) | live action (automation D7: edit, empty patch, `Undo::None`), `perform` |
| `ExpressionActivationRequest` | PF-25's "set an expression on or off, with a fade" live action | same |
| lists, model, state | `vts_server.catalog` | query, `perform` (§10) |
| first injection of a parameter set, a set that grows, or a mode change | `vts_server.feed` | live action, `perform` (§5.1) |
| `ParameterCreationRequest`, `ParameterDeletionRequest` | `vts_server.param_create`, `vts_server.param_delete` | live action, `perform` (§5.2) |

**Op errors map to documented ids.** `Waiting` (paused, pairing pending) → 8 `RequestRequiresAuthetication` while
pairing, else 205 `HotkeyExecutionFailedBecauseBadState` for hotkeys and 0 `InternalServerError` otherwise, with a
`message` saying "paused in Omuzaki"; `Denied` / `Ended` → 50 `TokenRequestDenied` (the README's revoked state);
`Limited` → 0 with `message` "rate limited, retry after N ms"; unknown target → 202 `HotkeyIDNotFoundInModel` or 651
`ExpressionActivationRequestFileNotFound`; no subject → 201 or 652.

**Changes to the shim note's table.**
- `ModelLoadRequest` → 154 `CannotCurrentlyChangeModel` (changing the subject is a scene edit, not `perform`).
- `MoveModelRequest` → 0 with `message` "moving the model is not supported" (no MoveModel id exists; open question 2).
- `ColorTintRequest`, `ArtMesh*`, `Item*`, `PostProcessing*`, `SetCurrentModelPhysicsRequest`, `NDIConfigRequest`:
  exactly as the note's §5 table (task S6).
- Unknown `messageType` → 7; malformed envelope → 2 to 6; disabled → 1, then close.

## 4. Identity, pairing, tokens and revocation

**The session.** One per connection that asks for a token or authenticates, opened through `Sessions::open`:

| `SessionSpec` field | Value |
|---|---|
| `transport` | `Transport::Plugin` (no new variant: a tool is outside code acting on a person's behalf, which is what the variant means) |
| `client` | `vts-<name>`: `pluginName` lowercased, every character outside `[a-z0-9-]` replaced by `-`, at most 36 characters. The shim builds it in that alphabet so the executor's sanitising leaves it unchanged |
| actor | built by the executor as ADR 0020 says, `<client>#<session id>` (for example `vts-bonktool#7`); the shim never builds one. The chip shows the declared name and developer from the shim's status |
| `on_behalf_of` | for a token request, the studio's person (the only one who can resolve the card, 7.2); for an authentication, the `principal` stored in the token's record |
| `grants` | `{perform}`. Never widened: the shim offers no "approve for this session" path for other scopes, and an op needing another scope is refused, not queued |
| `budget`, `limits` | `usd_micros` 0; `Limits::default()` (agent-sessions 8.2) |

Before authentication a connection has **no session**: only `APIStateRequest`, `AuthenticationTokenRequest` and
`AuthenticationRequest` are answered; everything else is 8.

**The contract change (task S0, first and alone, the Director's approval).** `Sessions` has no way today to open a
session waiting for pairing, and transports may not build a `PendingApproval`. Two additive methods, each with a
default body returning `Unsupported` (as `request_budget` has), close the gap for the shim and for the MCP bridge of
agent-sessions 10.2 (#405) alike:
- `fn request_pairing(&self, id: SessionId) -> Result<ApprovalId, OpError>`: under the executor's lock, pauses the
  session `Paused { by: <its principal>, reason: Pairing }` and queues `Need::Pairing { client }`. `Conflict` if the
  session has ended or has already been paired in this run.
- `fn gate(&self, id: SessionId) -> Result<Arc<SessionGate>, OpError>`: `SessionGate` holds a live flag and an
  **epoch** counter. Under its lock, **before** the call returns, the executor clears the flag and increments the epoch
  on every transition that leaves `Active`, wherever it is made: `pause`, `stop`, `end`, `set_grants` (when it drops
  `perform`), `undo_session` (which pauses with `PauseReason::Undo`) and the budget pauses inside `charge` and
  `request_budget`. The code that moves a session to `Paused` or `Ended` is one function, so no path can skip it. It
  sets the flag true again on `resume`; the epoch never goes back. Readers call `SessionGate::state()`, which returns
  `(live, epoch)`; nothing else writes either.

**Pairing (agent-sessions 10.2, 7.1).**
1. `AuthenticationTokenRequest` opens the session and calls `request_pairing`. The card (built by the UI from
   `Need::Pairing { client }` and the shim's status) reads: "‹pluginName› by ‹pluginDeveloper› wants to control your
   avatar through the VTube Studio-compatible API. Tools choose their own names." Pair or Refuse; an icon, if sent, is
   shown only after a size check (≤ 64 KiB decoded, PNG 128 × 128, as the API states) and is never stored.
2. **Only a person in Omuzaki's window resolves it** (7.2). Agents cannot, and no op, CLI or MCP command pairs.
3. The shim learns the outcome from `ApprovalResolved` and `SessionEnded`, which the App forwards for the shim's own
   sessions (`ShimLink::session_events`). Pair (`Approved`, `by` = the person): the shim draws 32 bytes from the OS
   CSPRNG, sends them base64url (43 characters, inside the API's 64) and stores the record with `principal` = `by`.
   Refuse: the executor ends the session `Revoked`; the shim replies 50.
4. Timeouts, on the shim's housekeeping tick (every 100 ms, its own thread): no answer in **120 s** → the shim calls
   `Sessions::end(id, Closed)` (which withdraws the card) and replies 50. A refused or timed-out `ToolKey` gets 50 at
   once for 60 s; the hold is an entry with its expiry time, checked on each token request.
5. One card at a time across all clients: a second token request while one is open gets 51.

**The token record.** `PairingRecord { tool: ToolKey { name, developer }, principal: Actor, hash: [u8; 32],
paired_at, last_used }`. `hash` = HMAC-SHA256 of the token under a per-machine shim key kept in the OS keychain
(CN-05). `principal` binds the credential to the person who paired it: authentication opens its session on their
behalf, and if `Sessions::open` refuses that principal (a VC-10 key revoked later, `Denied`), the reply is
`authenticated: false`. **Revocation does not wait for an event.** The shim keeps, per record, the ids of the sessions
it opened under it in this run; `AuthenticationRequest` first reads each with `Sessions::session` (a synchronous read
of the executor's registry, which keeps ended sessions) and, if any is `Ended { reason: Revoked { credential: true } }`,
deletes the record and answers `authenticated: false`. It then opens its session and **reads them all again**; if
the recheck finds a credential revocation, it ends the session it just opened (`Closed`) and answers `false`. The
executor's registry is the single ordering point: an authentication that passes both reads is ordered before the
revoke, one that misses it is refused, and none can succeed after `Sessions::end` returned. So a revoke from the Agent
panel invalidates the token the moment `Sessions::end` returns; the `SessionEnded` event then ends every other session
of the record and closes their connections, and persists the deletion. The records live in the machine data dir (`bridges/vts-pairings.toml`, owner-only), not in
settings (settings are listed to agents) and not in a project. Pairing is **machine-wide**: a stream tool is not tied
to a project (open question 1). The token is never logged, put in an op record, event, status or error text.

**Authentication.** `AuthenticationRequest` matches name, developer and the keyed hash (constant-time compare); a
match opens an `Active` session with no card; a mismatch is `authenticated: false`, as the README documents.

**Every transition, and who triggers it.**

| Transition | Triggered by | Effect |
|---|---|---|
| connected → authenticated or pairing | the tool's request | as above; a connection that has neither within **10 s** of the upgrade is closed by the housekeeping tick. The 10 s is suspended while the token request waits on the card and **restarts when the token is sent**, so a person who approves late still leaves the tool 10 s to send `AuthenticationRequest` |
| active → paused, and back | a person (Agent panel `pause`, `stop`, `resume`) or the budget | the executor flips the gate before returning; triggers get `Waiting` |
| any → ended `Revoked { credential: true }` | `vts_server.revoke` (the shim calls `Sessions::end`), or a person in the Agent panel (the executor ends it) | the gate flips under the lock; the shim deletes the record on `SessionEnded` with `credential: true`; the live connection gets 50 to every request and its token authenticates `false` |
| any → ended `Disconnected` | the socket closes (the shim calls `Sessions::end`) | slot and claims released |
| any → ended `Closed` | `vts_server.stop`, the setting turned off (the App calls `stop` when it sees the change), pairing timeout | stop answers 1 to every open connection, then closes it |
| any → ended `ProjectClosed` | the App closing the project | when the next project opens, the App calls `ShimHandle::project_opened`, and the shim opens a new session for each authenticated connection, without a card |

## 5. Injected parameters

### 5.1 Authority through an op, values through a wait-free table

`InjectParameterDataRequest` is sent every frame. One op per frame per tool would exceed the session's run limit
(3,000 a minute) and put thousands of records into the op log, the reason automation D7 keeps variables out of ops. So
the **authority** is an op and the **values** are data under it:

- A slot is armed for one `(parameter set, mode)`. A request whose `mode` differs from the armed mode, or that names a
  parameter outside the armed set, first runs `vts_server.feed { params, mode }` as the session: it checks `perform`,
  the session's state and limits and the `set` claims (454), and is logged once. It returns the tool's slot (one of
  `MAX_CLIENTS`) with the session's `SessionGate` and the epoch it read, which the slot stores.
- **A slot's authorization is cached only while nothing it rests on has changed.** The slot is disarmed, so that the
  next request runs `feed` again, when: its `set` claim is released (keep-alive expiry, below); its connection's
  session is replaced (`project_opened`), which also installs the new session's gate; or `param_delete` removes a
  parameter in its armed set. Disarming is done by the same code that releases the claim or replaces the session, on
  the shim's thread, before it answers anything else on that connection. **A gate change needs no event:** the frame
  thread and the request path both treat a slot as armed only while the gate is live **and its epoch equals the epoch
  stored at arming**. A pause and a resume that both happen before the shim sees any event bump the epoch, so the old
  authorization and the old values are dead the moment the gate is live again, and the next request runs `feed`.
- Values go to the slot in an `InjectTable` (a triple buffer per slot, the pattern of `omuzaki-track-api::mailbox`),
  each with its arrival time on the frame clock: newest wins, nothing queues, both sides wait-free.
- **Disarm is synchronous with the session controls.** The frame thread reads the slot's `SessionGate` every frame.
  Because the executor clears it before `pause`, `stop`, `end` or a narrowing `set_grants` returns, no frame after
  that call uses the tool's values, whoever made the call (the shim, the Agent panel, the budget).

### 5.2 The parameter map

`DEFAULT_PARAMS` maps the README's default input parameters to Omuzaki: `FaceAngleX/Y/Z` → head yaw, pitch, roll
(degrees); `FacePositionX/Y/Z` → head position; `EyeOpenLeft/Right` → `eyeBlinkLeft/Right` as `1 − v`;
`EyeLeftX/Y`, `EyeRightX/Y` → gaze; `MouthOpen` → `jawOpen`; `MouthSmile` → `mouthSmileLeft` and `Right`;
`BrowLeftY`, `BrowRightY` → `browOuterUpLeft/Right`; the rest of the README's list where a channel exists. A default
parameter with no channel is accepted and ignored (its value is still answered by `ParameterValueRequest`).
**Custom parameters** change only through ops run as the session: `ParameterCreationRequest` runs
`vts_server.param_create { name, min, max, default }` and `ParameterDeletionRequest` runs `vts_server.param_delete
{ name }`, so pause, limits and the audit apply. They are kept per `ToolKey` in memory until the shim stops, at most
`MAX_PLUGIN_PARAMS` (100) per tool, named by `CustomParamName`, and drive nothing until the person maps one to a
channel on the Settings page (task S8). An unknown name is 453.

**Reading values back.** `InputParameterListRequest` and `ParameterValueRequest` return names, ranges, defaults and
this tool's own injected values, **never live tracking values**: those are derived from the camera, and SE-04 keeps the
camera's output on the device unless a person routes it (open question 3).

### 5.3 Which layer, and the rules honoured

Injected values enter **`Source::Tracker`** (PF-23's order: rest, score, tracker, audio, trigger). They are tracking
input by the API's own definition (they replace the face tracker's parameter values), so they should lose to triggers
and to audio's `Max` mouth exactly as the camera does, and the life layer must treat them as human. A new `Bridge`
source would change `omuzaki-anim-api` (`Source::COUNT`, `ALL`, every `Ownership` array) for no behaviour the tracker
layer lacks. The stage merges the live slots into the tracker's `ChannelBuf` before `LayerStack::resolve`, **in an
order that does not depend on connection order**: per channel, first the one `set` value (claims make it unique),
then the sum of the `add` values taken **in ascending order of value** (`f32::total_cmp`), so the sum depends only on
the multiset of values and not on which connection sent which or in what order they connected: identical inputs give
identical output across reconnects, including three connections that share one token.

| Rule (README) | Omuzaki |
|---|---|
| `set` mode, `weight` w (0..=1, default 1) | `t = lerp(tracker[c], v, w)`; the channel joins the tracker layer's mask |
| `add` mode (weight ignored) | `tracker[c] = t + Σ add[c]` |
| one owner per parameter in `set` mode | a second tool's `set` on a claimed parameter is 454; `add` is allowed. A claim ends when its owner's value for that parameter expires (1 s after its last value, below), its session ends or pauses, or its mode changes. If another tool then claims the parameter while the old value is still easing out, the old contribution is **dropped at the frame the new claim begins** (takeover is deterministic), so the merge never has two `set` values |
| re-send at least once a second | a channel whose newest value is older than `KEEPALIVE` (1 s) on the frame clock leaves the mask, eased out over 100 ms (no pop). The frame thread applies it; the shim's housekeeping tick releases the `set` claim at the same deadline, and the ease-out never outlives a takeover |
| `faceFound: true` | stored with its arrival time; the tracker counts as `Tracking` only while that flag is under 1 s old **and** the gate is live, so a tool that goes quiet lets the PF-23 hand-off start after 1 s |
| values within ±1,000,000 | 451 beyond (`ParamValue`); then clamped to `Channel::range()` (or ±90° head, ±1 gaze) |
| weight outside 0..=1 | 452 |

**Budget.** The merge is at most `CHANNEL_CAPACITY` (128) channels × 4 slots: under **10 µs a frame** at the 99th
percentile on the CI runner, no allocation, no lock (a `criterion` bench in task S5).

## 6. Safety and privacy (SE-04)

- **Nothing listens unless a person turned it on.** `bridges.vts_server.enabled` defaults to false; only
  `vts_server.start` (person only) binds. At launch the App (S10) runs `start` as the person only if the setting is
  still on, which only a person can have set (`AgentWrite::Never`).
- **Loopback only.** `ShimConfig::bind` must be `127.0.0.1` or `::1` (`ShimError::NotLoopback`); there is no LAN
  option. The upgrade is refused (HTTP 403) when `Host` is not `localhost`, `127.0.0.1` or `[::1]` with the bound
  port, or when an `Origin` header is present and not a loopback origin, so a web page or a DNS-rebinding page cannot
  reach it.
- **No discovery.** The shim never opens a UDP socket and never broadcasts; a test asserts it opens one port.
- **Caps.** `max_clients` 4 (a fifth is refused at the upgrade), of which at most 2 may be unauthenticated; an
  unauthenticated connection is closed 10 s after the upgrade unless its token request is waiting on the card (then
  the 120 s pairing deadline applies, and a fresh 10 s starts when the token is sent), so idle sockets cannot hold
  every slot; a message over 64 KiB closes the socket (1009);
  per client, injection 120 messages a second (burst 240; above it values coalesce, newest wins, each still answered)
  and other requests 20 a second (burst 40; above it error 0 "rate limited"); any client past 1,000 messages in one
  second is closed (1008); hotkeys one per target every 5 frames (203) with a queue of `HOTKEY_QUEUE` (32, then 200); the shim's
  queue worker runs one queued trigger each time the stage's published frame counter (in the `InjectTable`) has
  advanced 5 frames.
  The session's executor limits apply on top.
- **What a tool can never do:** read the camera, audio, files, the project, settings or other sessions; hold any
  scope but `perform`; change the scene, the character or what is broadcast; pair itself. It sees only names and
  opaque ids.
- **Threads.** The listener and connections run on the shim's own thread pool, never on the frame thread; the frame
  thread only reads the `InjectTable`.

## 7. Naming, licence and trademark

- The feature is "VTube Studio-compatible API" in the UI and docs; never "VTube Studio" as the product, never "VTS"
  alone, no "drop-in replacement" claim, no logos (shim note §7).
- **On the wire** `apiName` `VTubeStudioPublicAPI` and `apiVersion` `1.0` echo the protocol; every field that names a
  product says Omuzaki: `vTubeStudioVersion` = `Omuzaki <version>`, error `message` text, log lines.
- **The non-affiliation line**, on the Settings page and in `vts_server.status`: "Not affiliated with or endorsed by
  DenchiSoft or Live2D Inc. VTube Studio is a product of DenchiSoft. Tools may behave differently with Omuzaki."
- **Default port 8011**, not 8001, so a running VTube Studio and the shim never fight over a port; the person may
  choose 8001. A taken port is `ShimError::PortInUse { port, suggestion }`.
- **Licence.** The API docs are MIT (DenchiSoft, 2019); we copy no text or code, so no notice ships. The EULA v5.2
  binds VTube Studio's licensees and has no clause on compatible servers (shim note §1). Clean-room: implementers use
  the two research notes and `omuzaki-vts`'s types only, never VTube Studio binaries, traffic captures or other
  compatible servers' code. Legal review before any public release stays a condition (owner; shim note,
  "Verdict", condition 2).

## 8. Events

`EventSubscriptionRequest` accepts the README's event names. Phase 1 produces `TestEvent`, `ModelLoadedEvent` (the
subject changes or unloads), `HotkeyTriggeredEvent` (any trigger of the subject fires, from any source; its id and
name, never the key) and `TrackingStatusChangedEvent`. A documented event Omuzaki never produces is accepted and
never fires (status lists it as "not produced"; open question 4); an unknown name is 950.

The shim's source is a `ShimEvent` channel (`ShimLink::events`) that the App's adapter (task S10) fills: subject
changes from `Runner` events (the App holds `read`), tracking status from the stage, and trigger firings from the
automation envelope's `hotkey` kind once #729 lands (`automation.md` D9, pending). The same adapter forwards the
shim's own sessions' `ApprovalResolved`, `SessionChanged` and `SessionEnded` (`ShimLink::session_events`, §4).
`ShimEvent` is the shim's own small enum, so envelope changes in #729 touch only the adapter. The shim publishes no `LiveEvent` in
phase 1; if it later does, it uses `Source::Plugin { id: "vts-<name>" }` and sets no `acting`, which D9 already allows.

## 9. VMC and OSC: the shared bridge rules, and what PL-05 adds (nothing)

PF-04 (VMC in and out, #468) and PF-05 (OSC, MIDI, gamepad, keyboard, Stream Deck, #469) are P0 features with their
own tasks to come; PL-05 adds **no tasks** for them. What it settles is the rule set every bridge follows:

1. **Off by default, started by a person** (a person-only op; settings `Machine`, `AgentWrite::Never`).
2. **A session per bridge**, `Transport::Plugin`, principal the person who started it, ceiling `perform`; actions run
   as ops through the `Runner` in that session; per-frame data flows under a feed op and the session's `SessionGate`, and every value expires 1 s after it arrives (§5).
3. **Loopback by default.** VMC is the exception the shim is not: phones and mocap PCs send over the LAN (mocopi,
   XR Animator), so PF-04 may offer a LAN bind behind a person's choice with a sender allowlist.
4. **Caps and clean refusals** as §6; no discovery broadcast unless a person enables it.

**VMC** ([protocol](https://github.com/sh-akira/VirtualMotionCaptureProtocol/blob/master/docs/english.markdown), MIT,
read 2026-10-15): OSC over UDP, Marionette on 39539 and Performer on 39540 by convention. VMC in is a tracker: `/VMC/Ext/Bone/Pos`
gives the tracked `BoneMask` (`input-motion.md` §6.5), `/VMC/Ext/Blend/Val` + `/VMC/Ext/Blend/Apply` drive channels
through the same tracker merge as §5.3, `/VMC/Ext/OK` drives face-found, with the same 1 s expiry. VMC out sends the solved pose; it is an output
the person starts. **OSC** ([1.0 specification](https://github.com/CNMAT/OpenSoundControl.org/blob/master/spec-1_0.md),
read 2026-10-15): an OSC address bound to an action is one more source of `BindingHit` (input-motion §5). One change
recommended to PF-05: network OSC actions run in the OSC receiver's session, not as the person, since a UDP packet is
not a person's hand (open question 5).

**QG-10 conformance targets** (PF-04 and PF-05 own them): golden OSC packets written from the specifications (every
VMC address above, a bundle with a time tag, string padding at each of the 4 alignments), byte-exact round trips;
owner runs against reference senders and receivers (VirtualMotionCapture, VSeeFace, XR Animator, the mocopi app,
a generic OSC sender), recorded as evidence.

## 10. Ops and settings

| Op | Kind | Scope | Agents | Args → result |
|---|---|---|---|---|
| `vts_server.status` | query | `read` | may | `{}` → running, bound address, tools (name, developer, paired, connected, last request), refused counts by id, events not produced, the non-affiliation line; never a token |
| `vts_server.catalog` | query | `perform` | may | `{}` → the subject (name, opaque id), hotkeys, expressions, the parameter table; what a tool may see |
| `vts_server.feed` | live action | `perform` | refused unless the calling session id is in the shim's own table of the sessions it opened (the shim registers these ops and checks that table; the client name is never consulted) | `{params, mode}` → the slot and gate; refused 454 / 453 as §5.3 |
| `vts_server.param_create` | live action | `perform` | as `feed` | `{name, min, max, default}` → created; over 100 refused |
| `vts_server.param_delete` | live action | `perform` | as `feed` | `{name}` → deleted, its claims released, every armed slot that names it disarmed |
| `vts_server.start` | live action | `perform` | never (`OpError::person_only`) | `{port?}` → binds if `bridges.vts_server.enabled` |
| `vts_server.stop` | live action | `perform` | never | `{}` |
| `vts_server.revoke` | live action | `perform` | never | `{tool: {name, developer}}` → forgets the record, ends its sessions |

| Setting | Scope | Agent write | Profile | Default |
|---|---|---|---|---|
| `bridges.vts_server.enabled` | `Machine` | `Never` | Excluded | `false` |
| `bridges.vts_server.port` | `Machine` | `Never` | Excluded | `8011` |

## 11. Worked example: a tool connects, triggers a hotkey and injects a parameter

Tool "BonkTool" by "Example Dev". Shim on `127.0.0.1:8011`; character "Mika" is the subject; the person has paired
nothing. `S` = session, `G` = effective grants.

| # | Tool → shim | Shim does | Reply | S / G |
|---|---|---|---|---|
| 1 | upgrade, no `Origin`, `Host: 127.0.0.1:8011` | checks, accepts (1 of 4) | 101 | none / none |
| 2 | `APIStateRequest` | | `active: true`, `vTubeStudioVersion: "Omuzaki 0.4.0"`, `currentSessionAuthenticated: false` | none / none |
| 3 | `HotkeysInCurrentModelRequest` | not authenticated | `APIError` 8 | none / none |
| 4 | `AuthenticationTokenRequest {pluginName: "BonkTool", pluginDeveloper: "Example Dev"}` | `Sessions::open {client: "vts-bonktool", on_behalf_of: the studio's person}` → `S7`, actor `vts-bonktool#7`; `request_pairing(S7)` → `Paused { Pairing }`, `Need::Pairing { client: "vts-bonktool" }`; the card shows | (waits, at most 120 s) | S7 paused / `{perform}` |
| 5 | (the person clicks Pair in Omuzaki) | `ApprovalResolved {by: person}` forwarded; token drawn; record stored with `principal` = person; S7 resumed by the executor | `authenticationToken: "<43 chars>"` | S7 active / `{perform}` |
| 6 | `AuthenticationRequest {…, authenticationToken}` | hash matches | `authenticated: true` | S7 / `{perform}` |
| 7 | `HotkeysInCurrentModelRequest` | `Runner::run(vts_server.catalog)` as `vts-bonktool#7` | `modelID: "9f3a…"`, hotkeys `[{name: "Smug", type: "ToggleExpression", hotkeyID: "c41e…"}, …]` | S7 / `{perform}` |
| 8 | `HotkeyTriggerRequest {hotkeyID: "c41e…"}` | `Runner::run(<PF-25 press>, {trigger: smug})` as S7; journal: live action, actor `vts-bonktool#7`, `on_behalf_of` the person | `HotkeyTriggerResponse {hotkeyID}`; `HotkeyTriggeredEvent` to subscribers | S7 / `{perform}` |
| 9 | `InjectParameterDataRequest {faceFound: false, mode: "set", parameterValues: [{id: "FaceAngleZ", value: 12, weight: 0.5}]}` | first use: `Runner::run(vts_server.feed {params: [FaceAngleZ], mode: set})` → slot 0 and S7's gate; claim on `FaceAngleZ` (set); value written to slot 0 | `InjectParameterDataResponse` | S7 / `{perform}` |
| 10 | the same request every frame | values only, no op; frame thread: `roll = lerp(roll, 12°, 0.5)` in the tracker layer | `InjectParameterDataResponse` | S7 / `{perform}` |
| 11 | (the tool stops sending) | 1 s after the last value: the frame thread eases `FaceAngleZ` out over 100 ms; the housekeeping tick releases the claim at that deadline and disarms the slot | none | S7 / `{perform}` |
| 12 | (the person revokes BonkTool with `vts_server.revoke`) | `Sessions::end(S7, Revoked { credential: true })`: S7's gate is cleared before it returns; `SessionEnded` → record deleted | next request: `APIError` 50 | ended / none |

## 12. Tests

- **Shim against the `omuzaki-vts` types:** every reply decodes as the crate's response type; a scripted tool client
  in each test file drives a real loopback socket; `FakeSessions` (approve, deny, time out) and `omuzaki-ops-mock`'s
  runner record each op with its actor and grants.
- **Golden request and reply pairs**, written from the README and `ErrorID.cs` (never captured from VTube Studio), in
  `crates/omuzaki-vts/tests/golden/shim/`: one per answered request, one per refusal row, compared with the
  timestamp and `requestID` normalised.
- **Properties:** no output (reply, log, status, event, op record) contains the issued token; arbitrary bytes and
  JSON never panic and always get one of ids 2 to 7 or a close; injection stops within one frame of revoke or pause.
- **Only an owner's real tools can check** (shim note §9): whether Streamer.bot, Mix It Up, Touch Portal, the Stream
  Deck plugin and TITS accept Omuzaki's version string and our refusal ids, the 5 frames versus 5 seconds hotkey
  cooldown, and which requests they send on connect. Recorded as evidence on the conformance task.

## 13. Task cut

Each task: Parent #529, `lane:cloud`, priority P1 unless stated. File when ADR 0066 is accepted; re-cut comments go on
#1086, #1212 and #1213.

- **S0, the `Sessions` contract change (first and alone).** Fence: `crates/omuzaki-ops-api/src/session.rs`, its
  tests and API snapshot; `crates/omuzaki-ops/src/**` (the executor's implementation); `omuzaki-ops-mock`'s
  `FakeSessions`. Contract: `request_pairing`, `gate`, `SessionGate` (§4). Acceptance: `request_pairing` pauses
  `Pairing` and queues exactly one `Need::Pairing`; Pair resumes, Refuse ends `Revoked`; the gate reads false and its epoch has
  increased before `pause`, `stop`, `end`, a `perform`-dropping `set_grants`, `undo_session` and the budget pauses in
  `charge` and `request_budget` return (a test reads it from another thread inside a hook), the flag is true after
  `resume` and the epoch is not lower than before the pause; default bodies return `Unsupported`. Model sonnet. Needs the Director's approval on
  the issue (Platform area contract, shared with #405). Blocked by #1218.
- **S1 (re-cut #1086), server core.** Fence: `crates/omuzaki-vts/src/server/{mod,listener,session,auth,pairing}.rs`,
  `tests/server.rs`. Contract: `ShimConfig`, `ShimLink { runner, sessions, principal, pairings: Arc<dyn PairingStore>,
  events, session_events, inject: InjectWriter }`, `start`, `ShimHandle { local_addr, clients, project_opened, stop }`,
  `ShimError { NotLoopback, PortInUse { port, suggestion }, Io }`, `PairingStore`, `ToolKey`, `PairingRecord` (with
  `principal`). Changes to #1086: default port 8011; client `vts-<name>`; `Host` and `Origin` checks; the 100 ms
  housekeeping tick with the 10 s authentication deadline, the 120 s pairing deadline and the 60 s refusal hold; at
  most 2 unauthenticated connections; the transition table of §4. Acceptance: #1086's list plus a foreign `Origin`
  is 403, a second token request is 51, two idle unauthenticated connections are closed after 10 s while a third, opened
  meanwhile, is refused at the upgrade and a new one connects once they are gone, a token sent after a pairing that
  took longer than 10 s still leaves 10 s to authenticate, an authentication after a Revoked-with-credential end is
  `false` before the `SessionEnded` event is processed, and one racing the revoke either fails or is ended by it, a pairing left unanswered ends `Closed` at 120 s, and an
  authentication opens its session for the stored principal.
  Model sonnet. Blocked by #1085, S0, #726, #316.
- **S2 (re-cut #1212), hotkeys, expressions, current model.** `ShimModel` is dropped: lists come from
  `vts_server.catalog` through the runner. Acceptance as #1212, plus each trigger is exactly one PF-25 op and the
  queue drains one trigger per 5 published frames. Model sonnet. Blocked by #1086, S3, and #478's live-action ops task.
- **S3 (re-cut #1213), ops and settings.** Adds `vts_server.catalog`, `feed`, `param_create` and `param_delete` to
  the table of §10; port default 8011. Model sonnet. Blocked by #1086, #373.
- **S4, parameter injection through the shim.** Fence: `crates/omuzaki-vts/src/server/inject/**`,
  `tests/server_inject.rs`. Contract: `inject_table() -> (InjectWriter, InjectReader)`, `InjectSlot` (values,
  arrival times, mode, `faceFound` time, the `SessionGate`), the published frame counter, `DEFAULT_PARAMS`.
  Acceptance: set, add, weight, 454 on a second `set`, 453, 451, 452; a mode change runs `feed` again; the claim is
  released at 1.0 s ± one tick and a second tool's `set` then succeeds, dropping the first tool's easing contribution
  at that frame (the output is one `set` value, never two); the slot is disarmed so the same tool's next request runs
  `feed` again; a `param_delete` and a `project_opened` disarm too, and a pause followed by a resume before any event
  is delivered leaves the slot dead until `feed` runs again (epoch); `faceFound` stops counting 1 s after the last
  injection; custom parameters only through their ops, 100 then refusal. Model sonnet. Blocked by S0, S1, S3.
- **S5, the stage merges injected slots into the tracker layer.** Fence: `crates/omuzaki-stage/src/perform/bridge.rs`
  (new; one `mod` line in `perform/mod.rs`) and `benches/bridge_merge.rs`. Acceptance: §5.3 table as unit tests;
  the same inputs give bit-identical output for every permutation of slot order and every assignment of values to slots, including slots that share a `ToolKey` (values such as `1e6`, `-1e6` and `0.1`); a cleared gate drops the slot on the
  next frame; under 10 µs p99 at 4 slots × 128 channels; zero allocations (counting allocator). Model sonnet.
  Blocked by S4.
- **S6, items, props and the other refusals.** Fence: `src/server/requests/refused.rs`, `tests/server_refused.rs`.
  Acceptance: every row of the shim note's §5 table and §3's changes, golden replies. Model sonnet. Blocked by #1086.
- **S7, event subscriptions.** Fence: `src/server/events.rs`, `tests/server_events.rs`. Contract: `ShimEvent`.
  Acceptance: the four produced events, "not produced" listing, 950. Model haiku. Blocked by #1086.
- **S8, Settings page for the shim (visual).** Fence: `omuzaki-ui` connectors page module. Acceptance: on and off,
  port, paired tools with revoke, custom-parameter mapping, the non-affiliation line; CI renders before and after.
  Model sonnet. Blocked by S3. Sits beside #1446 (the client's page).
- **S9, the conformance golden set.** Fence: `crates/omuzaki-vts/tests/golden/shim/**`, `tests/shim_golden.rs`.
  Acceptance: one pair per answered request and per refusal, written from the docs with the URL and date in each
  file's header. Model haiku. Blocked by S1.
- **S10, the App adapter: wiring the shim into the desktop App.** Fence: `crates/omuzaki-ui/src/bridges/vts_shim.rs`
  (new; one `mod bridges;` line in `src/lib.rs`, a hub line the Platform lead signs off), `tests/vts_shim_wiring.rs`.
  Builds the `ShimLink`; forwards the shim's sessions' `ApprovalResolved`, `SessionChanged` and `SessionEnded`;
  fills `ShimEvent` from subject changes, tracking status and the D9 `hotkey` kind; calls `project_opened` and `stop`
  (on project change and when `bridges.vts_server.enabled` turns off); hands the `InjectReader` to the stage.
  Acceptance: with a real executor and a scripted tool, a scene subject change gives `ModelLoadedEvent`, a trigger
  pressed in the UI gives `HotkeyTriggeredEvent`, turning the setting off closes the tool's socket after error 1.
  Model sonnet. Blocked by S1, S5, S7, #729 (the `hotkey` part only; the rest can land first).

## 14. Open questions (for the owner, with a recommendation)

1. **Pairing machine-wide or per project?** Recommend machine-wide: a stream tool serves every project, and the
   ceiling is fixed at `perform`, so there are no project grants to scope.
2. **Rate-limit and unsupported-move error id.** No documented id fits; recommend 0 with an explanatory `message`.
3. **Live tracking values to tools?** Recommend never in phase 1; a later person-only opt-in if a real tool needs it.
4. **Events we never produce:** recommend accept and never fire (tools subscribe in bulk on connect) over 950.
5. **Network OSC actions:** recommend the OSC receiver's own session rather than the person (for PF-05's design).
6. **Legal review** of the shim before a public release (shim note condition 2): owner.
