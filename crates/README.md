# Rust workspace

The Omuzaki app, as a Cargo workspace (spec v3 §9, §39). Members are globbed (`crates/*`), so adding a crate never edits
a shared file.

## Crate kinds and the dependency rule

| Kind | Names | May depend on |
|---|---|---|
| **API** (contracts) | `omuzaki-core`, `*-api` | `omuzaki-core`, other API crates, external crates |
| **Mock** | `*-mock` | API crates |
| **Lib** (format libraries) | crates with `layer = "lib"` under `[package.metadata.omuzaki]` | API crates and other lib crates (mocks as dev-dependencies) |
| **Implementation** | everything else under `crates/` | API crates and lib crates (mocks as dev-dependencies) |
| **App** | `omuzaki-cli`, `omuzaki-ui`, `omuzaki-test`, `omuzaki-stage`, `xtask`, the area test crates `omuzaki-test-<area>` | anything: they wire implementations together (area test crates: see below) |

`cargo xtask lint-deps` enforces the table (§39 rule 10). API crates change rarely and go through maintainer
review; breaking changes need a decision record (§39.2 item 8).

**Area test crates** (ADR 0054 §6). A test across crates goes in its area's test crate, `omuzaki-test-<area>` (a new
area is a new crate), not in the harness `omuzaki-test`. An area test crate may depend on API crates, mocks, the harness
and its own area's implementation and lib crates: `omuzaki-<area>`, `omuzaki-<area>-*`, plus the few whose names don't
say their area (`omuzaki-rig` and `omuzaki-anim` are `char`, `omuzaki-gltf` is `io`). A change to one area then doesn't
put the other areas' tests in scope. `lint-deps` enforces it.

**Lib crates** (ADR 0011) hold code that several implementations need and that is too large to write twice, and that is
a *format* rather than behaviour: `omuzaki-gltf` is glTF 2.0 for both `omuzaki-io-gltf` and `omuzaki-io-vrm`. A lib crate
declares itself in its manifest, because no name says it:

    [package.metadata.omuzaki]
    layer = "lib"

Implementations, apps and other libs may depend on a lib; a lib may depend on contracts and other libs, never on an
implementation or an app. **Contracts and mocks never depend on a lib, not even as a dev-dependency.** A contract must
not pull real code in, and a mock is the independent oracle the real code is tested against (`omuzaki-io-gltf` is checked
against the mock's own GLB reader and writer); it stops being independent the moment it shares code with what it
checks. A lib crate stays a library: no registration with `inventory`, no global state, nothing that makes it an
implementation of a contract (the format crates that depend on it do that).

## Self-registration and `link()`

Ops, part kinds, templates, formats and the other registries fill themselves: a crate calls `register_<kind>!` next to
what it registers, and no list names it (§39 rule 2). A binary keeps a registration only if it links the crate, and on
wasm only if it calls into the crate's code, so the wiring is explicit (ADR 0015):

- a crate whose library registers anything defines its `link()` with `omuzaki_core::define_link!();` in `src/lib.rs`;
- a crate that depends on one calls `<crate>::link()`: from its own `link()` (`define_link! { omuzaki_char::link(); }`)
  if other crates link it, at startup if it is an App binary;
- each App has a link gate, `tests/registrations.rs`, that compares what the binary registered with what the source
  declares, by name. `cargo xtask registry --app <crate>` prints the expected list.

`cargo xtask lint-links` checks the wiring, and `cargo xtask check` runs it.

## The crates

Each crate describes itself in its own `Cargo.toml`, so adding a crate edits no shared file (ADR 0053):

    [package]
    description = "..."

    [package.metadata.omuzaki]
    kind = "Implementation"     # API, Mock, Lib, Implementation or App, as in the table above
    summary = "..."             # one line: what the crate is

`cargo xtask crate-table` prints the table of every crate from those lines. `cargo xtask lint-crate-table` fails on a crate
without them, on a `kind` that disagrees with `lint-deps`, and on a crate table in this file.

A crate whose folder uses `omuzaki_embed::modules` (ADR 0054) lists that folder in `generated-mods = ["src/ops"]` under the same table (paths relative to the crate; `"src"` means `src/lib.rs`), and `cargo xtask lint-mods` then fails on a hand-written `mod` for a child of the folder or a missing generated `include!`.

**Planned crates** (not in the workspace yet):

- `omuzaki-agent-api` (API): not in the workspace yet: the in-app assistant's panel contract (`Assistant`, `UserMessage`, `ContextChip`, `AssistantEvent`, `HostError`) and `RationaleSource` for "Explain this"; design in `docs/design/agent-runtime.md`, ADR 0040
- `omuzaki-agent-mock` (Mock): not in the workspace yet: `ScriptedAssistant`, which plays an event script on `FakeClock` for panel tests and the gallery
- `omuzaki-agent` (Implementation): not in the workspace yet: `AssistantHost`, the assistant's turn loop over an in-process MCP client and `dyn LlmClient`, the context builder and compaction, the host-local tools and the conversation store; links API crates and rmcp's client only, never `omuzaki-mcp` or `omuzaki-ops`
- `omuzaki-performer-api` (API): not in the workspace yet (designs): the AI performer's contract (`ai-performer.md`, ADR 0031) and its voice and face types: `VoiceAssignment`, `VoiceProvider`, `VoiceCatalog`, `VoiceLead`, `TalkState`, `MoveContext`, `StyleKnobs` and the per-frame `BigMovePlanner` (`ai-performer-voice-face.md`, ADR 0041)

The other API crates (§39.1) arrive in later phases, each with its mock.

**Planned: shared library sources** (ADR 0057, `docs/design/library-sharing.md` §5). `omuzaki-library-source-api` (API: `ItemStore`,
`SignedIndex`, review records, the pure source verifier and the team-licence join), `omuzaki-library-source-mock` (Mock:
`FakeStore` and the conformance suite), `omuzaki-library-source` (Implementation: the folder store, the cache and the
`library.team.*` ops) and `omuzaki-library-source-s3` (Implementation: the bucket store, so the network dependency stays
out of the rest).

**Plugins** (ADR 0058, `docs/design/plugins.md` §1). `omuzaki-plugin-api` (API, built: the WIT package `omuzaki:plugin`,
`Manifest` with its checks and licence classes, the permission summary and update diff, `Approval` and the session
`ceiling`, the sandbox `limits` and `CallKind` budgets, `PluginId`, `PluginInfo`, `PluginState`, `FaultKind`,
`PluginError` and the `Plugins` service; no wasmtime). `omuzaki-plugin-mock` (Mock, built: `MockPlugins`, `MockGuest`,
`MockSessions` and the conformance suite). `omuzaki-plugin` (Implementation, built; native only: the wasmtime 49
component host with fuel and epochs per call kind, the CPU share, the WASI stubs, the worker threads, faults and
restarts, the plugin log, packages, signatures, installs and the compiled-code cache). Still to come in it: the op
bridge, the data folder and `fetch`, settings, reload by watching and the `plugin.*` ops.

**Planned: connectors** (ADR 0030, `docs/design/connectors.md` D1 and D13). `omuzaki-connect-api` (API: manifests,
requests, the `Connectors` service, the tray), `omuzaki-connect-mock` (Mock), `omuzaki-connect` (Implementation: the host
and the `connect.*` ops) and one `omuzaki-connect-<family>` Implementation per adapter family. HTTP has one contract,
`omuzaki-http-api` (API, built), with `FakeHttp` in `omuzaki-http-mock` and the one client in `omuzaki-http`, so
connectors, `net.http` and the LLM providers never link a second HTTP stack. `omuzaki-stage` wires the host into each
App's `Services`.

## Checking your change

    cargo xtask check --affected       # what CI runs, on the crates you touched (the default mode)
    cargo xtask check --affected --list  # just print those crates
    cargo xtask check --all            # the whole workspace
    cargo xtask lint-deps              # only the layering rule above
    cargo xtask lint-links             # only the link() wiring (ADR 0015)
    cargo xtask models                 # only the shipped-model licence gate (ML-01, ADR 0017)
    cargo xtask lint-crate-table       # only the crate metadata (ADR 0053)

`check` maps the files that differ from `--base` (default `origin/main`, via the merge-base; uncommitted and untracked
files count) to crates, adds every crate that depends on them (dev-dependencies too), then runs in order: `fmt --check`,
`clippy -D warnings`, `test`, `cargo deny check licenses bans sources`, `lint-deps`, `lint-links`, `models` (every shipped model has an allowed licence file). A change to `Cargo.toml`,
`Cargo.lock`, `deny.toml`, `clippy.toml`, `rustfmt.toml`, `rust-toolchain.toml` or `.cargo/` checks everything; a change
outside every crate (docs, tools) runs only the `models` gate, since a licence-file change can break it. It always runs every step and ends with a one-screen summary; the exit
code is non-zero if any step failed.

**Link gate only.** An App such as `omuzaki-cli` links every op crate only to carry their registrations (ADR 0015), so a
change to an op crate can break its link but not its behaviour. A crate lists those dependencies in its manifest:

    [package.metadata.omuzaki]
    link_only = ["omuzaki-io-ops", "omuzaki-stage"]

When the crate is in scope only through such dependencies (directly or through crates that depend on them), `check` runs
`fmt` and `clippy` on it and one extra step, `link-gate` (`cargo test -p <crate> --test registrations`), instead of all its
tests; the banner (stderr) prints it as `omuzaki-cli (link gate only)`, while `--list` stdout stays plain package names for scripts. A change to the crate itself, or to any
dependency not listed, or a dev-dependency on a listed one, still runs everything. Only list a dependency whose API the
crate's own source never calls (a bare `use x as _;` or the `define_link!` list does not count).

`lint-deps` classifies crates by name (table above), except that `layer = "lib"` in a manifest makes an implementation-named
crate a lib (an unknown layer, or `lib` on a contract, mock or app, is a violation), and checks normal and build
dependencies against it. Dev-dependencies follow the same table plus one widening: mocks. Output is one line per violation, `crate → dependency: rule`.

## Building

    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo build --target x86_64-pc-windows-gnu      # Windows, cross-compiled from Linux (needs mingw-w64)
    cargo build --target wasm32-unknown-unknown -p <crate>

## Fixtures

Golden files and parity fixtures are checked in. Several were made once by generator scripts from the earlier JavaScript prototype, which is not published; the tests compare against the checked-in files and never run those scripts.
