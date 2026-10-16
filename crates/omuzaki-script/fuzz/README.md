# Fuzzing omuzaki-script

Two libFuzzer targets, outside the workspace and outside the CI gate (they need a nightly toolchain and `cargo-fuzz`):

- `check` feeds any bytes to `ScriptHost::check`: it must not panic or hang, and every problem it reports has a line and a
  column.
- `run` feeds any bytes to `ScriptHost::run` against the fake runner of `../tests/common/mod.rs` (one fake, not two): no
  panic, no hang past the limits, every error positioned, and a failed run leaves nothing committed.

The seed corpus is the sandbox-escape corpus (`../tests/escapes`) and the example scripts (`../examples`), read from
where they live so a new escape or example is a seed without copying it.

## Running

Install once: `cargo install cargo-fuzz --locked` and `rustup toolchain install nightly`. Then, from `crates/omuzaki-script`:

```sh
mkdir -p fuzz/corpus/check fuzz/corpus/run
cargo +nightly fuzz run check fuzz/corpus/check tests/escapes examples -- -max_total_time=600
cargo +nightly fuzz run run fuzz/corpus/run tests/escapes examples -- -max_total_time=600
```

The first folder after the target is where libFuzzer keeps what it finds (it is not committed, see `.gitignore`); the others
are read-only seeds. A crash is written to `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`.

Run both for ten minutes before a change to the engine, the bindings or the check lands, and put the numbers libFuzzer
prints at the end (executions, corpus size, peak memory) in the pull request.
