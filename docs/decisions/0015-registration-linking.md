# 0015: Self-registrations reach every binary through `link()`, and a gate proves it (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** registries on wasm (where the problem was first reported), and the earlier choice of self-registration instead of central lists.

## Context

Crates register ops, part kinds, templates, grammar rules, face bakers, importers, exporters, migrations, gallery stories and contrast pairs with `register_<kind>!` macros over `inventory`. Each registration is a `#[used]` static and a constructor in the object file of the module that calls the macro. Nothing in the program refers to it, so two things can drop it from a binary without any error:

1. **The crate is never linked.** rustc links a dependency only if the code names it. An App that only calls ops by name never names `omuzaki-char`, so none of its ops would be there. Until now `use omuzaki_x as _;` has been the workaround.
2. **The object file is dropped.** A linker takes an object out of a library archive only to resolve a reference. For native targets rustc adds a synthetic object that references every `#[used]` static of every linked crate (rust-lang/rust#95604, the fix), so this cannot happen there. For wasm32 rustc adds no such object.

Measured on rustc 1.99 and inventory 0.3.25. The test case is a crate whose two modules each register one item, linked by an app that names the crate but references neither module:

| Target and linker | dev | release (thin LTO) |
|---|---|---|
| x86_64 Linux, mold and GNU ld | both kept | both kept |
| x86_64-pc-windows-gnu, mingw ld (run on Windows through WSL interop) | both kept | both kept |
| wasm32-unknown-unknown, wasm-ld (run in node) | **both dropped** | **both dropped** |
| wasm32, `-C codegen-units=1`, app calls the crate's `link()` | both kept | both kept |

More measurements:
- An app that never names the crate links none of its registrations (Linux, dev). This is how rustc loads crates, so it is the same on every target.
- On wasm with one codegen unit, `use x as _;` alone still kept nothing: the app has to call into the crate.
- MSVC `link.exe` and macOS `ld64` get the same synthetic object as the other native targets (COFF and Mach-O). They were not run here.
- The symptom did not reproduce: its test passes without the `#[used]` workaround.
- The gate below, run on main before this change, found nothing dropped in dev or release, on Linux or on Windows. `omuzaki-stage` had all 29 registrations of the crates it links. `omuzaki` had 0 because it links no crate that registers anything, which is a wiring gap and not a linker problem.

## Decision

1. **`link()` wiring.**
   - Every crate whose library registers anything defines `pub fn link()` with `omuzaki_core::define_link!` in `src/lib.rs`.
   - The function is `#[inline(never)]` and ends in `core::hint::black_box(())`. An empty function would let link-time optimisation see that the call does nothing and delete it, and with the call goes the reference that keeps the crate's object file on wasm. `black_box` is opaque to the optimiser, so the call and the reference stay.
   - A crate that depends on such a crate calls `<crate>::link()`. A library that others link does this from its own `link()`, so the chain reaches every App.
   - Apps call their dependencies' `link()` once at startup. This replaces `use omuzaki_x as _;`.
   - A crate may define `link()` before it registers anything, so the crates that link it are wired ahead of time.
2. **One codegen unit per crate on wasm.** `.cargo/config.toml` sets `-C codegen-units=1` for `wasm32-unknown-unknown`. Each crate is then one object file, and the reference that `link()` makes keeps all of that crate's registrations. Native builds keep their default codegen units: there, naming the crate already keeps every registration.
3. **The gate.**
   - Every `register_*!` macro also records a `omuzaki_core::link::Registered` (package, kind, name, place) from the same expansion, so the record is in a binary exactly when the registration is.
   - `xtask::registry` reads the expected registrations from the source: item-level calls of the workspace's `register_*` macros, in the libraries of an App and of every crate it depends on.
   - It evaluates the `cfg`s on a call, on its inline modules and on the `mod` declarations leading to its file. `#[cfg(test)]` code is left out. A `cfg` it can't decide, such as a cargo feature, is an error naming the file and line, never a guess.
   - Each App has `tests/registrations.rs`, which compares the two sets by name. Today that is `omuzaki` (the CLI and the MCP server; the test runs the real binary) and `omuzaki-stage`. The UI app and the web player add theirs.
   - `cargo xtask lint-links` checks the wiring in rule 1 without building anything.
   - CI runs the dev gate and `lint-links` on every PR (`cargo xtask check`) and the release gate nightly.
4. **Still no central list.** The registrations, the expected set and the `link()` chain all come from each crate's own source.
   - A new registration in a crate that already has `link()` changes nothing else.
   - A crate's first registration adds its `link()` and the calls in the crates that link it. `lint-links` names each one.

## Consequences

- Registering crates gain a one-line `omuzaki_core::define_link!();`, and `omuzaki-stage` forwards to the ones it links.
- Every crate in a wasm build compiles as one codegen unit. Wasm builds get slower; the code gets smaller and faster, which is the usual wasm release setting anyway. `RUSTFLAGS` replaces target rustflags, so a wasm build that sets it must keep `-C codegen-units=1`. The web player's gate catches a build that doesn't.
- The gate also proves the scan is complete. A registration the scan cannot see (nested in a block, or made by another macro) is in the binary but not in the expected set, and fails as "unexpected".
- Registrations in `tests/`, `benches/` and `examples/` are outside the gate: they never ship.
- Rejected:
  - **A generated `link()` that references every registering module** (a build script lists them).
    - A trait-impl anchor doesn't work. rustc places an `impl` method in the codegen unit of its `Self` type, not of the module that writes the `impl`, so the reference pulls the wrong object (measured on wasm).
    - A free function or static as the anchor needs a unique name per call. That takes a proc macro, or `unsafe` `export_name`.
    - Private modules can't be named from the crate root.
    - One codegen unit on wasm gives the same guarantee with none of this.
  - **`codegen-units = 1` for every crate on every target.**
    - Native builds don't need it.
    - It is costly in release: `omuzaki-char` took 14–20 s against 2–3 s with 16 units, on the shared build machine under load. In dev the difference was within noise.
    - A per-crate profile list would also be a central list.
  - **`#[used]` references in tests** (the workaround). Natively they are not needed, and they do nothing for the binaries that ship.
