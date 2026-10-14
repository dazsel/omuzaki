# MCP resources: the provider contract and URI table

**Status:** built for the document (A1 on main); updates (A2) and the per-kind providers are planned. **Decision:** the seam stays in `omuzaki-mcp` until a second consumer exists.
**Code:** `crates/omuzaki-mcp/src/resources/mod.rs`, `resources/doc.rs`, `server.rs` (`McpOptions::with_provider`), `session.rs` (`list_resources`, `read_resource`, `require_read`).

## 1. The provider contract

A resource family is a type that implements `ResourceProvider` (`resources/mod.rs`). The trait has four methods:

| Method | Must guarantee |
|---|---|
| `templates()` | Returns the RFC 6570 templates this family serves, for `resources/templates/list`. Pure: no document access. |
| `list(runner, grants)` | Returns the concrete resources worth listing now. Reads only through the `Runner` it is handed. |
| `serves(uri)` | Says whether `uri` belongs to this family, and nothing else. It is how an unknown URI (no provider serves it) differs from a known URI that names nothing. |
| `read(runner, grants, uri)` | Returns the contents of a URI that `serves` accepted. `NotFound` for a URI of this family that names nothing. |

The server enforces what the provider cannot:

- **Scope.** `read_resource` calls `require_read` before it dispatches, so every `read` is behind the `read` scope. `list_resources` lists providers only when the session holds `read`; without it, only `omuzaki://log` is listed. A provider never sees a session that lacks `read`.
- **Routing.** `Providers::serving` gives the URI to the first provider whose `serves` returns true. The built-in `DocProvider` is first, then host providers in registration order, so a host provider cannot take `omuzaki://project` or `omuzaki://character/…`; the first match wins. No match gives `resource_not_found` and lists the templates.
- **Threads.** `list` and `read` are blocking calls, so they run off the async threads. A provider holds no session state.
- **Errors.** `protocol_error` maps `NotFound` to the protocol's `resource_not_found`, `Denied` to `invalid_request` with `code: denied`, and anything else to an internal error that keeps its code.

**Registration.** A host adds a family with `McpOptions::with_provider(Arc<dyn ResourceProvider>)` (`server.rs`). The provider is then listed, templated and readable with the built-in ones, and no list in `omuzaki-mcp` changes. The trait is re-exported as `omuzaki_mcp::ResourceProvider`; the `resources` module itself is private.

**Why the trait lives in `omuzaki-mcp` for now.** Only the MCP server consumes it. It moves to an API crate when a second consumer needs it: `omuzaki://ui/context` is the first. Until then, moving it would add a crate for one user.

## 2. The URI table

| URI | MIME | Owner | Status |
|---|---|---|---|
| `omuzaki://log` | `application/json` | built in (`events.rs`, `LOG_URI`) | available |
| `omuzaki://project` | `application/json` | built in (`DocProvider`) | available |
| `omuzaki://character/<id>` | `application/json` | built in (`DocProvider`) | available |
| `omuzaki://render/<id>.png` | `image/png` | per-kind provider | planned |
| `omuzaki://measure/<id>` | not yet set | per-kind provider | planned |
| `omuzaki://track/<id>` | not yet set | per-kind provider | planned |
| `omuzaki://metrics/live` | not yet set | per-kind provider | planned |
| `omuzaki://library/<item>` | not yet set | per-kind provider | planned |

Planned rows have no data source on main yet, so each one's MIME type and template are set by the provider that adds it. The per-kind provider issues are filed once the A1 trait is reviewed.

## 3. The `omuzaki://project` summary

`omuzaki://project` summarises the document root without dumping it. For each top-level key:

- an object gives `{"ids": [...]}` (its child keys), so `characters` lists the character ids;
- anything else is passed through as its value.

The response also carries a `note` that points to `omuzaki://character/<id>` and `omuzaki://log`.

It is generic on purpose. A new section of the document appears in the summary without a list to edit, and an agent reads a section's contents through its own URI. Two limits follow from the code. Object children show ids only, not values. Strings and arrays at the top level are passed through whole, so a large array there is large here too; the summary does not cap them.

## 4. Updates

`resources/updated` for provider resources is planned as A2. It will map the `Event::OpApplied` patch paths to the URIs they touch. Today the server pushes `resources/updated` only for `omuzaki://log`, when a client subscribes to it (`session.rs`, the subscribe path). The provider trait has no method for mapping paths to URIs yet, so A2 adds it.
