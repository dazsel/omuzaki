---
title: "Compatibility bridges: the VTube Studio-compatible API shim, and how VMC and OSC fit (PL-05)"
summary: "the VTube Studio-compatible API shim (ADR 0063): where it lives, which requests it answers or refuses and with which error ids, the tool's session, pairing, token and revocation, injected parameters in the tracker layer, safety caps, naming, ops and settings; how VMC (PF-04) and OSC (PF-05) fit; tests and the task cut."
area: design
---

# Compatibility bridges: the VTube Studio-compatible API shim, and how VMC and OSC fit (PL-05)

**Status:** proposed. **Feature:** PL-05 (#529). **Decision record:** [ADR 0063](../decisions/0063-bridges.md).
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
| Who a tool is | One `Transport::Plugin` session per authenticated connection, actor `Actor::Plugin { id: "vts:<name>#<n>" }`, principal the studio's person, ceiling `perform` and nothing else | 4 |
| Pairing | A token request opens the session `Paused { Pairing }` and queues `Need::Pairing`; only a person in Omuzaki's window pairs; the token's keyed hash is kept machine-wide | 4 |
| What reaches Omuzaki | Only ops through the `Runner`, as the tool's session: live actions for hotkeys and expressions, `vts_server.catalog` for lists, `vts_server.feed` to claim parameters | 3, 5 |
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
| first injection of a parameter set | `vts_server.feed` | live action, `perform` (§5.1) |

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

**The session.** One per authenticated connection, opened through `Sessions::open`:

| `SessionSpec` field | Value |
|---|---|
| `transport` | `Transport::Plugin` (no new variant: a tool is outside code acting on a person's behalf, which is what the variant means) |
| `client` | `vts:<pluginName>/<pluginDeveloper>`, sanitised (printable, 3..=32 chars each, as the API allows) |
| actor (built by the executor) | `Actor::Plugin { id: "vts:<pluginName>#<session>" }`; the chip says "<pluginName> (via VTube Studio-compatible API)" |
| `on_behalf_of` | the studio's person (the one who pairs) |
| `grants` | `{perform}`. Never widened: the shim offers no "approve for this session" path for other scopes, and an op needing another scope is refused, not queued |
| `budget`, `limits` | `usd_micros` 0; `Limits::default()` (agent-sessions 8.2) |

Before authentication a connection has **no session**: only `APIStateRequest`, `AuthenticationTokenRequest` and
`AuthenticationRequest` are answered; everything else is 8.

**Pairing (agent-sessions 10.2, 7.1).**
1. `AuthenticationTokenRequest` opens the session `Paused { reason: Pairing }` and queues `Need::Pairing { client }`.
   The card reads: "‹pluginName› by ‹pluginDeveloper› wants to control your avatar through the VTube Studio-compatible
   API. Tools choose their own names." Pair or Refuse; the icon, if sent, is shown only after a size check (≤ 64 KiB
   decoded, PNG 128 × 128, as the API states) and is never stored.
2. **Only a person in Omuzaki's window resolves it** (7.2). Agents cannot, and no op, CLI or MCP command pairs.
3. Pair: the shim draws 32 bytes from the OS CSPRNG, sends them base64url (43 characters, inside the API's 64), stores
   the record, resumes the session. Refuse, or no answer in **120 s**: reply 50, end the session `Revoked`, and answer
   50 at once to the same name for 60 s (no card spam).
4. One card at a time across all clients: a second token request while one is open gets 51.

**The token record.** `PairingRecord { tool: ToolKey { name, developer }, hash: [u8; 32], paired_at, last_used }`,
`hash` = HMAC-SHA256 of the token under a per-machine shim key kept in the OS keychain (CN-05). The records live in
the machine data dir (`bridges/vts-pairings.toml`, owner-only), not in settings (settings are listed to agents) and
not in a project. Pairing is **machine-wide**: a stream tool is not tied to a project (open question 1). The token is
never logged, put in an op record, event, status or error text.

**Authentication.** `AuthenticationRequest` matches name, developer and the keyed hash (constant-time compare); a
match opens an `Active` session with no card; a mismatch is `authenticated: false`, as the README documents.

**Revocation.** `vts_server.revoke { tool }` (person only), or revoking the session in the Agent panel (agent-sessions
§6, credential ticked by default), deletes the record and ends every session of that tool `Revoked { credential: true
}`. The live connection then gets 50 to every request, and its token authenticates `false`. Pausing a session stops
its injection (§5.1) and holds its triggers (`Waiting`). Turning the shim off answers 1 and closes every socket.
Closing the project ends the sessions `ProjectClosed`; the shim reopens a session for each authenticated connection in
the next project without a card.

## 5. Injected parameters

### 5.1 Authority through an op, values through a wait-free table

`InjectParameterDataRequest` is sent every frame. One op per frame per tool would exceed the session's run limit
(3,000 a minute) and put thousands of records into the op log, the reason automation D7 keeps variables out of ops. So
the **authority** is an op and the **values** are data under it:

- The first injection of a parameter set (or a set that grows) runs `vts_server.feed { params, mode }` as the session.
  It checks `perform`, the session's state and its limits, and is logged once. It returns a feed slot (one of
  `MAX_CLIENTS`) and arms the slot's live flag.
- Values go to the slot in an `InjectTable` (a triple buffer per slot, the pattern of `omuzaki-track-api::mailbox`):
  newest wins, nothing queues, both sides wait-free.
- The live flag is one atomic per slot. The shim clears it, **before** replying, when the session pauses, ends or is
  revoked (it learns from `SessionChanged` and `SessionEnded`); the frame thread reads it every frame, so injection
  stops on the next frame.

### 5.2 The parameter map

`DEFAULT_PARAMS` maps the README's default input parameters to Omuzaki: `FaceAngleX/Y/Z` → head yaw, pitch, roll
(degrees); `FacePositionX/Y/Z` → head position; `EyeOpenLeft/Right` → `eyeBlinkLeft/Right` as `1 − v`;
`EyeLeftX/Y`, `EyeRightX/Y` → gaze; `MouthOpen` → `jawOpen`; `MouthSmile` → `mouthSmileLeft` and `Right`;
`BrowLeftY`, `BrowRightY` → `browOuterUpLeft/Right`; the rest of the README's list where a channel exists. A default
parameter with no channel is accepted and ignored (its value is still answered by `ParameterValueRequest`).
Custom parameters (`ParameterCreationRequest`, at most `MAX_PLUGIN_PARAMS` (100) per tool, names `CustomParamName`)
are kept per tool and drive nothing until the person maps one to a channel on the Settings page (task S8). An unknown
name is 453.

**Reading values back.** `InputParameterListRequest` and `ParameterValueRequest` return names, ranges, defaults and
this tool's own injected values, **never live tracking values**: those are derived from the camera, and SE-04 keeps the
camera's output on the device unless a person routes it (open question 3).

### 5.3 Which layer, and the rules honoured

Injected values enter **`Source::Tracker`** (PF-23's order: rest, score, tracker, audio, trigger). They are tracking
input by the API's own definition (they replace the face tracker's parameter values), so they should lose to triggers
and to audio's `Max` mouth exactly as the camera does, and the life layer must treat them as human. A new `Bridge`
source would change `omuzaki-anim-api` (`Source::COUNT`, `ALL`, every `Ownership` array) for no behaviour the tracker
layer lacks. The stage merges each live slot into the tracker's `ChannelBuf` before `LayerStack::resolve`, slots in
connection order:

| Rule (README) | Omuzaki |
|---|---|
| `set` mode, `weight` w (0..=1, default 1) | `tracker[c] = lerp(tracker[c], v, w)`; the channel joins the tracker layer's mask |
| `add` mode (weight ignored) | `tracker[c] += v` |
| one owner per parameter in `set` mode | a second tool's `set` on a claimed parameter is 454; `add` is allowed |
| re-send at least once a second | a channel whose last value is older than `KEEPALIVE` (1 s) leaves the mask, eased out over 100 ms (no pop) |
| `faceFound: true` | the tracker counts as `Tracking` while the slot is live, so the ease-to-rest and life-layer hand-off (PF-23) do not start |
| values within ±1,000,000 | 451 beyond (`ParamValue`); then clamped to `Channel::range()` (or ±90° head, ±1 gaze) |
| weight outside 0..=1 | 452 |

**Budget.** The merge is at most `CHANNEL_CAPACITY` (128) lerps per slot, 4 slots: under **10 µs a frame** at the
99th percentile on the CI runner, no allocation, no lock (a `criterion` bench in task S5).

## 6. Safety and privacy (SE-04)

- **Nothing listens unless a person turned it on.** `bridges.vts_server.enabled` defaults to false; only
  `vts_server.start` (person only) binds, and the app at launch starts the shim only if the person left it on.
- **Loopback only.** `ShimConfig::bind` must be `127.0.0.1` or `::1` (`ShimError::NotLoopback`); there is no LAN
  option. The upgrade is refused (HTTP 403) when `Host` is not `localhost`, `127.0.0.1` or `[::1]` with the bound
  port, or when an `Origin` header is present and not a loopback origin, so a web page or a DNS-rebinding page cannot
  reach it.
- **No discovery.** The shim never opens a UDP socket and never broadcasts; a test asserts it opens one port.
- **Caps.** `max_clients` 4 (a fifth is refused at the upgrade); a message over 64 KiB closes the socket (1009);
  per client, injection 120 messages a second (burst 240; above it values coalesce, newest wins, each still answered)
  and other requests 20 a second (burst 40; above it error 0 "rate limited"); any client past 1,000 messages in one
  second is closed (1008); hotkeys one per target every 5 frames (203) with a queue of `HOTKEY_QUEUE` (32, then 200).
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

The shim's source is a `ShimEvent` channel the App fills from `Runner` events (as the App, which holds `read`) and,
once #729 lands, from the automation envelope's `hotkey` kind (`automation.md` D9, pending). `ShimEvent` is the
shim's own small enum, so envelope changes in #729 touch only the App's adapter. The shim publishes no `LiveEvent` in
phase 1; if it later does, it uses `Source::Plugin { id: "vts:<name>" }` and sets no `acting`, which D9 already allows.

## 9. VMC and OSC: the shared bridge rules, and what PL-05 adds (nothing)

PF-04 (VMC in and out, #468) and PF-05 (OSC, MIDI, gamepad, keyboard, Stream Deck, #469) are P0 features with their
own tasks to come; PL-05 adds **no tasks** for them. What it settles is the rule set every bridge follows:

1. **Off by default, started by a person** (a person-only op; settings `Machine`, `AgentWrite::Never`).
2. **A session per bridge**, `Transport::Plugin`, principal the person who started it, ceiling `perform`; actions run
   as ops through the `Runner` in that session; per-frame data flows under a feed op's live flag (§5.1).
3. **Loopback by default.** VMC is the exception the shim is not: phones and mocap PCs send over the LAN (mocopi,
   XR Animator), so PF-04 may offer a LAN bind behind a person's choice with a sender allowlist.
4. **Caps and clean refusals** as §6; no discovery broadcast unless a person enables it.

**VMC** ([protocol](https://github.com/sh-akira/VirtualMotionCaptureProtocol/blob/master/docs/english.markdown), MIT,
read 2026-10-15): OSC over UDP, Marionette on 39539 and Performer on 39540 by convention. VMC in is a tracker: `/VMC/Ext/Bone/Pos`
gives the tracked `BoneMask` (`input-motion.md` §6.5), `/VMC/Ext/Blend/Val` + `/VMC/Ext/Blend/Apply` drive channels
through the same tracker merge as §5.3, `/VMC/Ext/OK` drives face-found. VMC out sends the solved pose; it is an output
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
| `vts_server.feed` | live action | `perform` | refused unless the actor is a shim session (`Actor::Plugin`, id `vts:…`) | `{params, mode}` → a slot; refused 454 / 453 as §5.3 |
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
| 4 | `AuthenticationTokenRequest {pluginName: "BonkTool", pluginDeveloper: "Example Dev"}` | `Sessions::open` → `S7` `Paused { Pairing }`, actor `vts:BonkTool#7`; queues `Need::Pairing { client: "vts:BonkTool/Example Dev" }`; the card shows | (waits) | S7 paused / `{perform}` |
| 5 | (the person clicks Pair in Omuzaki) | token drawn, keyed hash stored, S7 resumed | `authenticationToken: "<43 chars>"` | S7 active / `{perform}` |
| 6 | `AuthenticationRequest {…, authenticationToken}` | hash matches | `authenticated: true` | S7 / `{perform}` |
| 7 | `HotkeysInCurrentModelRequest` | `Runner::run(vts_server.catalog)` as `vts:BonkTool#7` | `modelID: "9f3a…"`, hotkeys `[{name: "Smug", type: "ToggleExpression", hotkeyID: "c41e…"}, …]` | S7 / `{perform}` |
| 8 | `HotkeyTriggerRequest {hotkeyID: "c41e…"}` | `Runner::run(<PF-25 press>, {trigger: smug})` as S7; journal: live action, actor `vts:BonkTool#7`, `on_behalf_of` the person | `HotkeyTriggerResponse {hotkeyID}`; `HotkeyTriggeredEvent` to subscribers | S7 / `{perform}` |
| 9 | `InjectParameterDataRequest {faceFound: false, mode: "set", parameterValues: [{id: "FaceAngleZ", value: 12, weight: 0.5}]}` | first use: `Runner::run(vts_server.feed {params: [FaceAngleZ], mode: set})` → slot 0, live flag on; value written to slot 0 | `InjectParameterDataResponse` | S7 / `{perform}` |
| 10 | the same request every frame | values only, no op; frame thread: `roll = lerp(roll, 12°, 0.5)` in the tracker layer | `InjectParameterDataResponse` | S7 / `{perform}` |
| 11 | (the tool stops sending) | 1 s after the last value, `FaceAngleZ` leaves the mask over 100 ms | none | S7 / `{perform}` |
| 12 | (the person revokes BonkTool) | record deleted; S7 `Ended { Revoked { credential: true } }`; slot 0 flag cleared | next request: `APIError` 50 | ended / none |

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

Each task: Parent #529, `lane:cloud`, priority P1 unless stated. File when ADR 0063 is accepted; re-cut comments go on
#1086, #1212 and #1213.

- **S1 (re-cut #1086), server core.** Fence: `crates/omuzaki-vts/src/server/{mod,listener,session,auth,pairing}.rs`,
  `tests/server.rs`. Contract: `ShimConfig`, `ShimLink { runner, sessions, principal, pairings: Arc<dyn PairingStore>,
  events, inject: InjectWriter }`, `start`, `ShimHandle`, `ShimError { NotLoopback, PortInUse { port, suggestion },
  Io }`, `PairingStore`, `ToolKey`, `PairingRecord`. Changes to #1086: default port 8011; `Host` and `Origin` checks;
  120 s pairing wait and the 60 s refusal hold; caps of §6. Acceptance: #1086's list plus a foreign `Origin` is 403
  and a second token request is 51. Model sonnet. Blocked by #1085, #726, #316.
- **S2 (re-cut #1212), hotkeys, expressions, current model.** `ShimModel` is dropped: lists come from
  `vts_server.catalog` through the runner. Acceptance as #1212, plus each trigger is exactly one PF-25 op. Model
  sonnet. Blocked by #1086, S3, and #478's live-action ops task.
- **S3 (re-cut #1213), ops and settings.** Adds `vts_server.catalog` and `vts_server.feed` to the table of §10;
  port default 8011. Model sonnet. Blocked by #1086, #373.
- **S4, parameter injection through the shim.** Fence: `crates/omuzaki-vts/src/server/inject/**`,
  `tests/server_inject.rs`. Contract: `inject_table() -> (InjectWriter, InjectReader)`, `InjectSlot`,
  `DEFAULT_PARAMS`. Acceptance: set, add, weight, 454 on a second `set`, 453, 451, 452, keep-alive at 1.0 s ± one
  frame, `faceFound`, 100 custom parameters then refusal; flag cleared before the revoke reply. Model sonnet.
  Blocked by S1, S3.
- **S5, the stage merges injected slots into the tracker layer.** Fence: the stage's tracker-merge module and a bench
  (Runtime lead names the file). Acceptance: §5.3 table as unit tests; under 10 µs p99 at 4 slots × 128 channels;
  zero allocations (counting allocator). Model sonnet. Blocked by S4.
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

## 14. Open questions (for the owner, with a recommendation)

1. **Pairing machine-wide or per project?** Recommend machine-wide: a stream tool serves every project, and the
   ceiling is fixed at `perform`, so there are no project grants to scope.
2. **Rate-limit and unsupported-move error id.** No documented id fits; recommend 0 with an explanatory `message`.
3. **Live tracking values to tools?** Recommend never in phase 1; a later person-only opt-in if a real tool needs it.
4. **Events we never produce:** recommend accept and never fire (tools subscribe in bulk on connect) over 950.
5. **Network OSC actions:** recommend the OSC receiver's own session rather than the person (for PF-05's design).
6. **Legal review** of the shim before a public release (shim note condition 2): owner.
