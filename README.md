# Omuzaki

**A desktop studio to build, perform and stream a VTuber, driven by people or by AI agents through MCP.**

Omuzaki is made by [Dazsel](https://dazsel.com/omuzaki). The code is at <https://github.com/dazsel/omuzaki>.

## Status: early development

Omuzaki is **not released**. There is no installer, no stable API and no promise that any given feature works yet.
The workspace builds and its tests pass, but most of the studio is contracts, mocks and first implementations, not a
finished app. The spec describes where it is going; the crate list in `crates/README.md` shows what exists today, and
marks what does not exist yet.

## What it will do

- **Build** a character: from an idea, a sketch or an existing model to a rigged, expressive character, with a typed
  operation behind every action so each step can be undone.
- **Perform** it live: from a webcam, a phone, motion capture, a voice or an AI performer, into any streaming setup.
- **Stream and reuse** it: live output for streaming software, and exports for games and rendered video.
- **Be driven by agents:** every operation the app can do is exposed through MCP, so Claude Code, Codex or any other MCP
  client can do what a person can, while the person watches and can steer or undo.

The full plan is in `docs/product/02-studio-spec-v3.md`. It is a plan, not a list of working features.

## How it's built

A Rust workspace in `crates/`, built contract-first. Each area has an API crate that holds the types and traits, a mock
crate with a deterministic in-memory version for tests, and implementation crates that do the real work. A dependency
rule, checked by `cargo xtask lint-deps`, keeps implementations depending on contracts and never on each other.
Implementations register themselves (operations, part kinds, formats), so adding one does not edit a shared list.

Start with `crates/README.md` for the layout and the rules, then `docs/README.md` for the documentation map.

## Building from source

You need a recent Rust toolchain through [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version and rustup
installs it on first use.

```sh
cargo build
cargo test
```

Some tests and spikes can use optional heavy assets (models, audio). They are not in the repository. Point
`OMUZAKI_ASSETS` at a folder that holds them; the default is an `assets-local/` folder in the workspace root, which is
git-ignored. Tests that need an asset that is missing skip or are marked as such, so a plain `cargo test` works without
any. See `assets/README.md`.

The full check that CI runs on the crates you changed:

```sh
cargo xtask check --affected
```

Linux builds need the usual system development packages for audio and windowing (ALSA, udev, xkbcommon, X11 or
Wayland).

## Contributing

Ideas and bug reports are welcome: open an issue for a bug, or start a discussion for an idea. **We are not accepting
code contributions (pull requests) yet.** Pull requests from outside the Dazsel organisation are closed automatically.
That may change later. `CONTRIBUTING.md` explains how to report a bug and why, and `SECURITY.md` explains how to report
a vulnerability privately.

AI coding agents working in the repository should read `AGENTS.md`.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT licence ([LICENSE-MIT](LICENSE-MIT))

at your option. Third-party dependencies, models and assets keep their own licences; the policy for them is in
`docs/licensing/policy.md`.

## Trademarks

Omuzaki and Dazsel are trademarks of Dazsel; the code licence does not grant use of the names or logos. See
[TRADEMARKS.md](TRADEMARKS.md).
