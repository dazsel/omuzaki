# MCP resources: the provider contract and URI table

Prompts (recipes) are a separate seam: `PromptDef` and `McpOptions::with_prompt` (`prompts.rs`); the recipes land with the features whose ops they need.

**Status:** built for the document (A1) and for provider updates (A2), both on main. The per-kind providers are planned. **Decision:** the seam stays in `omuzaki-mcp` until a second consumer exists.
**Code:** `crates/omuzaki-mcp/src/resources/mod.rs`, `resources/doc.rs`, `events.rs` (`Updates`), `server.rs` (`McpOptions::with_provider`), `session.rs` (`list_resources`, `read_resource`, `require_read`).

## 1. The provider contract

A resource family is a type that implements `ResourceProvider` (`resources/mod.rs`). The trait has five methods:

| Method | Must guarantee |
|---|---|
| `templates()` | Returns the RFC 6570 templates this family serves, for `resources/templates/list`. Pure: no document access. |
| `list(runner, grants)` | Returns the concrete resources worth listing now. Reads only through the `Runner` it is handed. |
| `serves(uri)` | Says whether `uri` belongs to this family, and nothing else. It is how an unknown URI (no provider serves it) differs from a known URI that names nothing. |
| `read(runner, grants, uri)` | Returns the contents of a URI that `serves` accepted. `NotFound` for a URI of this family that names nothing. |
| `touched(paths)` | Returns the URIs of this family's resources that a change at `paths` affects. Called on the async threads for every applied op, so it answers from the paths alone: no document read, no blocking. A URI ending in `/` stands for every resource beneath it (`omuzaki://character/` is every character). The default returns no URI, so a provider whose resources do not come from the document needs no code. |

The trait requires `Debug + Send + Sync`, so one provider is shared across threads behind an `Arc`.

The server enforces what the provider cannot:

- **Scope.** `read_resource` calls `require_read` before it dispatches, so every `read` is behind the `read` scope. `list_resources` lists providers only when the session holds `read`; without it, only `omuzaki://log` is listed. A provider never sees a session that lacks `read`.
- **Routing.** `Providers::serving` gives the URI to the first provider whose `serves` returns true. The built-in `DocProvider` is first, then host providers in registration order, so a host provider cannot take `omuzaki://project` or `omuzaki://character/…`; the first match wins. No match gives `resource_not_found` and lists the templates.
- **Threads.** `list` and `read` are blocking calls, so they run off the async threads. A provider holds no session state.
- **Errors.** `protocol_error` maps `NotFound` to the protocol's `resource_not_found`, `Denied` to `invalid_request` with `code: denied`, and anything else to an internal error that keeps its code.

**Registration.** A host adds a family with `McpOptions::with_provider(Arc<dyn ResourceProvider>)` (`server.rs`). The provider is then listed, templated and readable with the built-in ones, and no list in `omuzaki-mcp` changes. The trait is re-exported as `omuzaki_mcp::ResourceProvider`; the `resources` module itself is private.

**Providers with their own source.** `list` and `read` receive the `Runner` for document data. A provider over another source (the asset store, the library) holds that source as an `Arc<dyn..>` given to its constructor; the App supplies that source and registers the provider; that is the pattern. A provider may also compute a value by running a query op through `Runner::run`. The `Request` carries the `actor` and the `grants`: the provider runs as `Actor::System` in `Mode::Apply` with the caller's grants, and the executor checks the op's scope against the grants in the request (`crates/omuzaki-ops/src/executor.rs`, `require`). A query is read-only by kind (`OpKind::Query`, `crates/omuzaki-ops-api/src/lib.rs`), so it never changes the document; a provider must still never write the store or the document itself.

**Why the trait lives in `omuzaki-mcp` for now.** Only the MCP server consumes it. It moves to an API crate when a second consumer needs it: `omuzaki://ui/context` is the first. Until then, moving it would add a crate for one user.

## 2. The URI table

| URI | MIME | Owner | Status |
|---|---|---|---|
| `omuzaki://log` | `application/json` | built in (`events.rs`, `LOG_URI`) | available |
| `omuzaki://project` | `application/json` | built in (`DocProvider`) | available |
| `omuzaki://character/<id>` | `application/json` | built in (`DocProvider`) | available |
| `omuzaki://render/<id>.png` | `image/png` (a blob) | `RenderProvider` (per-kind) | planned |
| `omuzaki://measure/liveliness/..` | `application/json` | `MeasureProvider` (per-kind) | planned; computed when read: it runs `measure.liveliness` through `Runner::run`, and nothing is stored |
| `omuzaki://track/<id>` | `application/json` (a summary) | `TrackProvider` (per-kind) | planned |
| `omuzaki://metrics/live` | `application/json` | `MetricsProvider` (per-kind) | planned (its source is designed elsewhere) |
| `omuzaki://library/<item>` | `application/json` | `LibraryProvider` (per-kind) | planned |
| `omuzaki://comments` | `application/json` | per-kind provider | planned |
| `omuzaki://ui/context` | not yet set | per-kind provider | planned |

Planned rows have no data source on main yet, so each one's MIME type and template are set by the provider that adds it. The per-kind provider issues are filed once the A1 trait is reviewed.

## 3. The `omuzaki://project` summary

`omuzaki://project` summarises the document root without dumping it. For each top-level key:

- an object gives `{"ids": [...]}` (its child keys), so `characters` lists the character ids;
- anything else is passed through as its value.

The response also carries a `note` that points to `omuzaki://character/<id>` and `omuzaki://log`.

It is generic on purpose. A new section of the document appears in the summary without a list to edit, and an agent reads a section's contents through its own URI. Two limits follow from the code. Object children show ids only, not values. Strings and arrays at the top level are passed through whole, so a large array there is large here too; the summary does not cap them.

## 4. Updates

A client subscribed to a URI is sent `notifications/resources/updated` when an op changes it. This covers `omuzaki://log` and every provider resource (A2).

- **Who may subscribe.** `resources/subscribe` (protocol 2025-11-25 and earlier) and `subscriptions/listen` (2026-07-28) accept `omuzaki://log` and any URI some provider `serves` (`session.rs`, `subscribable`). Both need `read`: without it `subscribe` and `unsubscribe` are refused, the listen filter accepts nothing, and the event log stays empty, because `EventHub::start` calls `Runner::subscribe` with the server's grants and the runner refuses that without `read` (`events.rs`).
- **The mapping** (`Updates::changed`, `events.rs`). An `OpApplied` event's patch change paths go to `Providers::touched`. A subscribed URI is notified if a touched URI equals it, or ends in `/` and is a prefix of it. `TxRolledBack`, `Undone` and `Redone` carry no paths, so they notify every subscribed URI. So does a subscriber whose position fell out of the event window (`LOG_CAPACITY` = 256 events). `omuzaki://log` is notified on every wake that carries an event, and is sent last.
- **Coalescing.** There is no timer. The feed task wakes when the newest event number changes, takes every event after its own position, and sends each affected URI once however many events there were: a burst is one notification per URI, then the client reads. Each subscriber (a legacy session's feed task, a `subscriptions/listen` stream) has its own position.
- **The built-in provider.** `DocProvider::touched` (`resources/doc.rs`): a change at `characters/<id>/..` touches `omuzaki://character/<id>`; any change at all touches `omuzaki://project`; a change at the root or at `characters` touches `omuzaki://character/` (every character).
- **Limits.** The provider set is fixed when `McpServer::new` runs (`server.rs`), and the server does not advertise `resources.listChanged`, so a client's cached `resources/list` is not refreshed when a character is added. Renders are planned as content-addressed, so the same hash always returns the same bytes and they never change (`crates/omuzaki-char-api/src/services.rs`). Library items have no op event that maps to them. A measure URI has no path mapping (`touched` keeps its default), so an applied op never notifies it. A rollback, undo, redo or missed event window still notifies every subscribed URI, measure included (`events.rs`, `Updates::changed`); a client re-reads it to re-measure.
- **Evidence.** In `crates/omuzaki-mcp/tests/mcp_resources.rs`: `an_op_on_a_subscribed_character_notifies_it_and_the_project`, `an_edit_to_another_character_sends_nothing_for_this_one`, `undo_and_redo_notify_what_is_subscribed`, `nothing_arrives_after_unsubscribe`, `a_provider_resource_can_be_subscribed_and_unknown_uris_cannot`, `subscribing_and_unsubscribing_need_the_read_scope`, `protocol_2026_listen_covers_provider_resources`. In `crates/omuzaki-mcp/src/events.rs`: `updates_name_what_the_paths_touch_with_the_log_last`, `replacing_or_removing_all_the_characters_reaches_each_subscribed_one`, `a_rollback_or_a_missed_window_touches_everything_wanted`. In `crates/omuzaki-mcp/src/resources/doc.rs`: `a_change_inside_a_character_touches_it_and_the_project`, `a_change_above_the_characters_touches_every_character`.
