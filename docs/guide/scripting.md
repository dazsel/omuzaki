---
title: "Scripting guide"
summary: "Write, check and run scripts and macros: the language, what a script may call, grants, limits, errors, dry runs and undo."
area: guide
---

# Scripting guide

A script is a short program that calls Omuzaki's ops in a loop or a condition, so that "set every character's blend to
0.5" is one run instead of twenty clicks or twenty tool calls. It runs as you: everything it changes is one step in your
undo history, and it can do nothing you couldn't do by hand.

Scripts are written in [Rhai](https://rhai.rs), a small language that looks like Rust and JavaScript. The design is in
`docs/design/scripting.md` and the decision behind it in `docs/decisions/0034-scripting.md`.

**The op names in this guide and in `crates/omuzaki-script/examples/` are illustrative.** `omuzaki ops` lists the ops your
build has, with their arguments. Characters in the examples are the synthetic `aki` and `ren`.

## Running a script

**None of these surfaces is wired into an app yet** (driver jobs, which `script.run` needs, come first), so each is
marked *planned*. What exists today is the runtime in `crates/omuzaki-script`, and its ops once an app links it.

- **The Script console** *(planned)* has an editor, an output pane and a Run button (Ctrl+Enter; Ctrl+Shift+Enter is a
  dry run). It checks the script as you type and underlines each problem at its line and column.
- **The command line** *(planned)*: `omuzaki --project <folder> script run <file>` runs a file as you. `--dry-run`
  shows what it would do and changes nothing, and `--params @file.json` gives the script its `params`.
  `omuzaki script check <file>` prints what the script names and what is wrong with it (`--json` for the full report).
  A script error exits with the code of the op error behind it; a script that does not parse or hits a limit exits with
  `invalid` (3).
- **An AI agent** *(planned)* calls the `script_run` tool, which is the `script.run` op: one call for a loop over
  twenty characters, instead of twenty tool calls. The agent needs the `script` scope, which no agent has by default.
  You grant it per session, like `publish`.

`script.check`, `script.run`, `script.save`, `script.delete`, `script.save_mine`, `script.delete_mine` and `script.list` are ops like any other, so everything
that can call an op can call them.

## The language in five minutes

```rhai
// Variables, arrays and maps (a map is written #{ key: value }).
let names = ["aki", "ren"];
let settings = #{ profile: "egg-oval", blend: 0.5 };

// Loops, conditions and string interpolation.
for name in names {
    if name != "ren" {
        print(`working on ${name}`);
    }
}

// Functions and closures.
fn double(x) { x * 2 }
let sizes = [1, 2, 3].map(|x| double(x));

// The last expression is the script's result.
sizes
```

- Numbers are integers (`1`) or floats (`1.5`); they are different types, and `1` stays `1` when it goes into an op.
- A map's keys are kept in sorted order, so a script does the same thing every time it runs.
- A `fn` is defined at the top level of the script. A closure (`|x| ...`) is how you pass a call to `try_op`
  or to an array's `map` and `filter`. A closure can only be passed straight to those, not kept in a variable,
  an array or a map, or returned, and it needs a body: write `|x| ()` rather than `|x| {}` for one that does nothing.
- `print(x)` adds a line to the output. `log(x)` adds a note, which the console shows muted. Both are capped (see the
  limits).
- `params` is the map the caller passed in. Check a key with `"key" in params` before using it.
- **There is no `sleep`, no file or network access, no clock, no randomness and no environment.** A script finishes in
  seconds; for work that waits, use a job or the automation graph.
- **`eval`, `import`, `Fn`, `call` and `curry` do not exist.** They are refused when the script is read, so every op a
  script can call is visible in its text before it runs. That is what makes `script.check` exact.

Examples to start from, each short and runnable, are in `crates/omuzaki-script/examples/`: `hello.rhai`,
`loop_with_try_op.rhai`, `read_with_get.rhai`, `params.rhai`, `ceiling.rhai`, `try_catch.rhai` and `last_value.rhai`.

## What a script may call

Every registered op `area.verb_noun` is `ops::area::verb_noun(#{ ... })` in a script: one map of arguments, checked against
the op's schema by the op itself. A plugin's ops appear as soon as the plugin registers them. An op returns its result
(an edit returns its summary, a query its answer), converted to Rhai values.
A word Rhai reserves cannot be written in a path, so an area called `export` is written `ops::export_::...`.

A script can call:

- **edit ops that undo with their patch**, which is almost every op that changes the project, and
- **query ops** (`measure.*` and the like).

A script cannot call, and gets a refusal with a hint for:

- a **job** (a render or an import), because a plain job cannot join a transaction;
- an op with **effects outside the document** (a file written, a stream started), because the run's rollback cannot
  undo them;
- **`script.run`**: scripts do not nest.

`script.check` reports the same refusals before anything runs.

Two more functions are built in:

- `get("characters/aki/face")` reads the document and returns `()` when nothing is there.
- `try_op(|| ops::...)` runs one call and returns `#{ ok, value, error }`, so a loop can skip what fails.

## Grants and the consent card

A script runs with **your grants, narrowed to what it names**:

> your grants, intersected with the script's declared ceiling, intersected with `read` plus the scopes of the ops it names

So a script that only names `face.*` ops cannot publish, even if you hold `publish`. The ceiling is optional and is
the first line of the script:

```rhai
// omuzaki: grants = ["edit:character"]
```

It can only narrow. A ceiling that names a scope you lack widens nothing, and one that cannot be read is an error
rather than being ignored. Paths typed inside a script add no file roots: a script reaches only the folders you
already allowed.

*(Planned, with the console.)* Before you first run a script that someone else wrote (an agent, an import), or one that names an op with `broadcast`,
`publish`, `spend`, `net`, `fs` or `native`, the console shows a **consent card** listing each op with its summary and
scope. You say Run once per script text.

A script never waits for approval. An op that would ask a person to confirm is refused, with the scope to grant in the
error, because an approval answered later would run one call of a script that has already ended.

## Limits

Every run is held to these limits. A run that breaks one stops at once, is rolled back, and reports the limit, the line
and the column.

| Limit | Console | `script.run` |
|---|---|---|
| Source size (bytes) | 65536 | 65536 |
| Operations | 1000000 | 10000000 |
| Inner op calls | 10000 | 10000 |
| Call depth | 64 | 64 |
| Expression depth | 64 | 64 |
| Expression depth in functions | 32 | 32 |
| String size (bytes) | 1048576 | 1048576 |
| Array length | 100000 | 100000 |
| Map size | 10000 | 10000 |
| Memory (bytes) | 268435456 | 268435456 |
| Output lines | 1000 | 1000 |
| Output bytes | 65536 | 65536 |
| Wall clock (seconds) | 10 | 120 |
| Result size (JSON bytes) | 65536 | 65536 |
| Nesting of values passed in or out | 64 | 64 |

Operations are every step the script takes, including the steps of a function you pass to `map`, `filter` and the other
array helpers. The nesting limit applies to a value that crosses the sandbox: the result, `params`, and the arguments
and answers of ops. Inside a script, how deep arrays and maps nest is bounded by the array and map limits. Memory is the
heap the run holds, counted as it allocates, and it includes what the data limits do not count (a map's keys, for one).
A program that runs scripts must install `omuzaki_script::MemoryBudget` as its global allocator, and a run refuses to
start without it.

`script.run` takes `seconds` (1 to 600, 120 if you leave it out). Output past the cap becomes one "output truncated"
line. Stop in the console, or cancelling the job, ends the run within one operation and rolls it back.

## Errors, and how to read them

A failed op raises an error carrying a map with:

| Key | What it is |
|---|---|
| `code` | `invalid`, `not_found`, `denied`, `conflict`, `unsupported`, `unavailable`, ... |
| `message` | what went wrong, in a sentence |
| `field` | the argument at fault, when there is one |
| `hint` | how to fix it |
| `op` | the op that failed |
| `line`, `column` | where the call is in your script |

`field` and `hint` are always there; they are `()` when the error has none. Catch the map with `try_op(...)` (it is the
`error` key) or with `try { ... } catch (e) { ... }` (it is `e`). **Only a failed op is caught.** A limit, a cancel, a
refusal and a mistake in the script itself still end the run.

An error that ends the run tells you what and where: `Function not found: ops::face::set_profil (map) (line 1,
position 12)` is a typo in an op name, and `script.check` finds it before the run (`unknown_ops`).

`script.check` answers with these keys, and the console underlines them:

| Key | What it lists |
|---|---|
| `ops` | every op the script names, with line and column, including calls inside functions and closures |
| `unknown_ops` | names the registry lacks |
| `field_errors` | unknown and missing fields in argument maps written out as literals |
| `refused` | calls to ops a script may not make |
| `scopes` | the scopes the named ops need |
| `ceiling` | the declared ceiling, if any |
| `parse_errors` | syntax errors, with line and column |

## Dry run

A dry run sends every call as a plan and lists what each would change, without changing anything. **Each call sees the
document as it was before the script started**, so a script whose later calls read what earlier ones wrote may plan
differently from a real run. Use a dry run to see what a script reaches, not to predict every value.

## One run, one undo step

A run is one transaction labelled with the script's name. Ctrl+Z undoes the whole run; History shows it as one step. If
any call fails and the script does not catch it, or a limit is hit, or you stop it, nothing the run did is kept.

## Macros

A macro is the list of op calls you made, recorded as you worked.

1. **Record.** Start recording, work as usual, stop. Only your own edits are kept: queries, other people's edits, an
   agent's work and other scripts' work are not. An edit you undo is not in the macro, and a transaction that was
   rolled back is not either.
2. **A slider drag is one step.** Many calls of an op that sets a value, which differ only in numbers, become one call
   with the last value. Only ops that declare themselves absolute setters merge, so an op that adds a mark or adjusts a
   value by an amount keeps one call per gesture.
3. **Replay.** Playing a macro runs its calls as one transaction named after the macro, so one undo step. The first call
   that fails ends it and undoes the rest, and the error names the step.
4. **Save.** `script.save` keeps a macro (as `steps`) or a script (as `source`) in the project, where it is journalled,
   undoable and visible to agents, and `script.save_mine` keeps it in My scripts, which travel across projects (a script
   can save into the project, not into My scripts). `script.run` with an `id` runs a saved script or
   macro.

A macro file is JSON lines: a header line and one call per line.

```
{"macro":1,"name":"oval for aki"}
{"op":"face.set_profile","args":{"blend":0.5,"character":"aki","profile":"egg-oval"}}
```
