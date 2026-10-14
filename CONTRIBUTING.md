# Contributing to Omuzaki

Thank you for your interest. Omuzaki is in early development, and for now we take **ideas and bug reports only**. We are
not accepting code contributions (pull requests) yet.

## Reporting a bug

Open an [issue](https://github.com/dazsel/omuzaki/issues/new/choose) with the bug report form. A good report has:

- what you did, what you expected and what happened instead;
- the shortest steps or commands that reproduce it;
- the commit (`git rev-parse --short HEAD`) or version, your operating system, GPU and Rust version (`rustc --version`);
- logs, screenshots or renders, as text where you can, with anything private removed.

Search the existing issues first, and add to one if it matches. **Do not report security problems in a public issue**:
see [SECURITY.md](SECURITY.md).

## Suggesting an idea

Start a topic in [Discussions](https://github.com/dazsel/omuzaki/discussions). Describe what you want to do and why it
is hard today, not only the feature you have in mind. Issues stay for bugs.

## Why we are not taking code yet

The project is changing quickly: the contracts between crates are still moving, and a change that is right today may
need to be redone next week. Reviewing outside code well takes time that we would rather spend settling those contracts,
and we do not want to ask you to write something that then has to be thrown away. So pull requests from people outside the
Dazsel organisation are closed automatically, with a note pointing here. It is not a judgement of the change or of you.
We expect to open this up once the contracts settle, and this file will say so when we do.

## Code standards

These apply to every change today and will apply to outside code when we accept it.

1. **Good engineering:** no duplicated logic, clear module boundaries, small focused functions, meaningful names,
   comments that explain why, docs on every public item, tests with every change, no dead code.
2. **Performance:** respect the frame and latency budgets in the spec (section 10). No allocation or locking in
   real-time hot loops. Measure before and after any change on the real-time path, and keep benchmarks for hot code.
3. **Correctness:** correct first. Unit tests, golden-render tests and property tests where they help. Explicit error
   handling: no `unwrap` or `expect` in library code. Deterministic output. `cargo fmt` and `cargo clippy -D warnings`
   clean.

Rules specific to this codebase:

- **Crate layering.** Contracts (`omuzaki-core`, `*-api`) depend only on each other and external crates. Mocks depend on
  contracts. Implementations depend on contracts and lib crates. Apps may depend on anything. `cargo xtask lint-deps`
  enforces this; the table is in `crates/README.md`.
- **API crates change rarely.** A change to an `*-api` crate needs a design discussion first, and a decision record in
  `docs/decisions/` if it breaks compatibility.
- **Self-registration.** Do not add a central list, enum or match that every feature must edit. Register through the
  `register_<kind>!` macros next to what is registered. See "Self-registration and `link()`" in `crates/README.md`.
- **Do not edit generated files** (MCP schemas, CLI help, docs indexes). They are generated.
- **Licences.** Nothing non-commercial, ever: no library, tool, model, dataset or asset with a non-commercial licence.
  Any new dependency, model or asset needs a licence check against `docs/licensing/policy.md`, and a row in
  `docs/licensing/register.md`.
- **No heavy assets or secrets in the repository.** Models, audio, renders and datasets live outside git (see
  `docs/decisions/0003-assets-outside-git.md`).

## Checking a change

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask check --affected     # all of the above on the crates you changed, plus cargo-deny and the layering rules
```

## Continuous integration

GitHub Actions runs on GitHub-hosted runners (Linux and Windows).

| Workflow | When | What |
|---|---|---|
| `ci.yml` | pull requests and the merge queue | One required check, `gate`, that waits for every job. Pull requests: fmt, clippy (`-D warnings`) on the changed crates and their direct dependents, tests of the changed crates, the link gates and the dependency lints. Merge queue: the same on everything affected, plus the Windows cross-build. Scope comes from `cargo xtask check --format json`. |
| `cache-warm.yml` | pushes to `main` | Builds the dependency caches the other workflows restore. The only workflow that saves a cache. |
| `nightly.yml` | daily | `tools/ci/nightly-check.sh`: the whole workspace, every cargo target and doctest, cargo-deny with advisories, the Windows build, the WebAssembly check. |
| `macos-weekly.yml` | weekly | `cargo check --workspace` on macOS. |
| `close-external-prs.yml` | pull requests opened | closes pull requests from outside the organisation with a note pointing here. |

Render tests run on lavapipe (Mesa's software Vulkan driver), which the CI jobs install. To run them locally, install
`mesa-vulkan-drivers` (or your platform's equivalent), or set `WGPU_ADAPTER_NAME`. Golden-image checks need an external
asset store, which hosted runners do not have, so they skip there and say so in the run summary.

**WebAssembly.** Every crate is checked for `wasm32-unknown-unknown` unless it opts out in its own `Cargo.toml`:

```toml
[package.metadata.omuzaki]
wasm = false   # say why: e.g. native audio capture
```

## Code of Conduct

Everyone taking part in issues and discussions follows the [Code of Conduct](CODE_OF_CONDUCT.md).
