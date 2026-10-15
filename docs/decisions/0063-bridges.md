---
title: "0063: Compatibility bridges drive Omuzaki only through ops, in a paired session that holds `perform` (2026-10-15)"
summary: "Compatibility bridges drive Omuzaki only through ops, in a paired session that holds perform"
area: decisions
---

# 0063: Compatibility bridges drive Omuzaki only through ops, in a paired session that holds `perform` (2026-10-15)

**Status:** proposed (#1085); accepted by the cloud lead on the pull request. **Context:** PL-05 (#529), CN-13 (#717),
PF-04 (#468), PF-05 (#469), PF-23 (#476); `docs/design/bridges.md`; `docs/research/2026-10-vts-shim-terms.md`.

## Context

PL-05 asks for a VTube Studio-style API shim so stream tools written for that API can drive Omuzaki, and for VMC and
OSC. A shim is a network listener that outside programs control; done carelessly it is a second door around the op
layer, the session rules of ADR 0020 and the privacy rules of SE-04, and a way to look like another company's product.
Its parameter injection arrives every frame, faster than the executor's per-session run limit.

## Decision

1. **The shim is a module of `omuzaki-vts`** (`src/server/**`), sharing CN-13's message types and the one WebSocket
   crate #726 picks (also BR-19's). No new crate and no contract change.
2. **Every tool is a session.** One `Transport::Plugin` session per authenticated connection, actor
   `Actor::Plugin { id: "vts:<name>#<n>" }`, principal the studio's person, ceiling `perform` and never more. Every
   change it makes is an op through the `Runner` as that session: PF-25's live actions for hotkeys and expressions,
   `vts_server.catalog` for what it may list, `vts_server.feed` to claim parameters.
3. **Pairing is a person's click.** A token request opens the session `Paused { Pairing }` with `Need::Pairing`;
   only a person in Omuzaki's window pairs. The token is 32 random bytes; only its keyed hash is kept, machine-wide,
   outside settings and projects. Revoking deletes it and ends the tool's sessions.
4. **Per-frame data flows under an op, not as ops.** `vts_server.feed` authorises a parameter set once; values then
   pass through a wait-free table to the frame thread, gated by a live flag the shim clears before replying to a pause
   or revoke. Injected values enter the existing `Source::Tracker` layer (PF-23), honouring set, add, weight,
   `faceFound` and the 1 s keep-alive, in under 10 µs a frame with no allocation.
5. **Safe by default.** Off until a person turns it on; person-only start, stop and revoke; loopback only with `Host`
   and `Origin` checks; no discovery broadcast; message, client and rate caps; a tool never reads the camera, files,
   the project or other sessions, and never sees live tracking values.
6. **A compatible API, not the product.** Omuzaki's name in every product field, the non-affiliation line, no logos,
   default port 8011; clean-room from the public docs only.
7. **VMC and OSC** are PF-04's and PF-05's; they follow the same bridge rules (a session per bridge, ops for actions, a
   feed for per-frame data, off by default), with a LAN option for VMC only. PL-05 files no tasks for them.

## Consequences

- No new `Transport`, `Source` or scope: the contracts stay as they are, at the cost of `Transport::Plugin` covering
  two kinds of outside code (WASM plugins and paired tools), told apart by the `vts:` client prefix.
- The tracker layer can hold several producers; the stage owns the merge and its budget.
- PF-25's live-action ops become a dependency of the shim's hotkeys; the shim's own tasks (#1086, #1212, #1213) are
  re-cut to this design.
- Real tools (Streamer.bot and others) are checked only by owner runs; legal review before public release remains.
