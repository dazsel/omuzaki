# 0022: Evals are data-driven tasks graded on the end state, replayed free on every PR and sampled from a pinned model only when the maintainers enable spending (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** QG-03, AG-01, ML-01, ML-09, ML-11, ML-12,
QG-09; spec §11, §12, §15, §16, §34, §39 rule 2, §40; ADR 0009 (determinism), ADR 0016 (user-supplied models), ADR 0017
(ML-01, in flight), ADR 0020 (agent sessions). The design is `docs/design/evals.md`.

## Context

QG-03 asks for agent tasks ("make the face rounder", "create a character from this sheet") run through MCP with a
reference model, each with a checker on the resulting document and renders, nightly, with a pass rate that may not
fall, and with every feature adding a task. ML-01 asks for an eval suite per model, with real and synthetic test sets
kept apart. ML-11 asks for critics that agree with human raters. Today there is one planned scripted eval, no
task format, no place for results, and no rule for spending on a model. LLM tokens cost money; no spend has been
approved. Private test material must never reach a fixture
or a result.

## Decision

1. **One eval system in `omuzaki-test`** for three families: agent tasks (QG-03), model suites (ML-01, run at
   `EvalLevel::Full`), and judge calibration (ML-11 critics, an optional LLM judge). One history format, one gate, one
   section on the progress page.
2. **Tasks are TOML files in the owning crate** (`crates/<crate>/evals/<task>.toml`), found by a glob, never listed
   centrally. A task names a spec feature, a prompt a person would type (the symptom, not the recipe), a fixture, the
   grants, caps, and checks. **Fixtures are op scripts** run through the normal runner on an empty project; binary
   inputs are eval-set items in the assets folder, named by hash.
3. **Checks are data, graded on the end state** by a separate checker runner with `read` and `render` only, on the
   result folder and a pristine fixture copy: document predicates, read-only op calls (`measure.*`, `render.*`, critics)
   with before/after expectations, render diffs inside a region, and transcript facts for safety properties (stayed in
   its brief, never applied a forbidden op). A new measurement is a new `measure.*` op, not a harness change.
4. **Every task proves its checks discriminate:** a solution script that must pass, a plausible wrong script that must
   fail, and an empty script that must fail (unless doing nothing is the right answer).
5. **Two drivers against the same server `omuzaki mcp` builds** (the `omuzaki-cli` runner constructor and
   `omuzaki_mcp::McpServer`, in process; `--binary` over stdio for a nightly smoke):
   - **scripted:** deterministic and free; affected tasks on every PR, all tasks nightly; any deviation fails;
   - **reference model:** our own minimal Messages API loop with a pinned model id, effort and system prompt, N
     attempts per task, served-model asserted, no server-side fallbacks, refusals and infrastructure errors kept out of
     the pass rate. Never a merge gate.
6. **Results:** one JSON line per attempt in `status/eval-history.jsonl` and one per run in `status/eval-runs.jsonl` in
   the assets folder; transcripts, check reports and renders under `evals/runs/` there too, kept 90 days
   unless an open issue names them. Never in git.
7. **The gate.** Scripted: exact. Reference, per (task, version, model, effort) series, provisional until 20 attempts:
   - **per task:** a regression when the last 10 attempts' passes are improbably low under the pooled pass rate of the
     previous 28 days (binomial tail below 0.01);
   - **per suite:** a regression when the night's mean task pass rate falls below the trailing 14-night median minus
     max(5 points, 3 × the median absolute deviation).

   A regression opens or updates an issue with the failing transcripts and a free replay of the last passing ones. A
   release needs no open eval regression.
8. **Spending is off until the maintainers enable it.** The reference driver ships disabled and stays off until they
   decide. When enabled, it reads only
   `OMUZAKI_EVAL_API_KEY`, from a dedicated Console workspace with its own spend limit. Caps per attempt, run, night or
   week, and calendar month are kept in integer micro-dollars from recorded usage and checked before every request.
   The proposal is a cheap model nightly and a stronger one weekly (about $160 a month, hard cap $200); the maintainers
   pick.
9. **Data rules.** Every fixture and set item needs a provenance record with commercial status `yes` from a
   synthetic, own, CC0, public-domain or suitably licensed source. A private list (private test material,
   by path and hash) is refused even with a record. Real sets are test-only and held out:
   - never in a training manifest (a lint);
   - never averaged with synthetic results;
   - thresholds set from dev slices;
   - never shown except as numbers;
   - until the maintainers decide, suites that need a real set are skipped with a visible
     `not run: maintainer decision` line, never shown or counted as passing.
10. **Soft before hard.** Critic, LLM-judge and human scores are recorded but never fail an attempt, until the judge
    agrees with the human-rated review set on at least 90% of its clear-cut items (at least 40) and fails the known
    negatives.
11. **Every feature adds a task** to its acceptance, or carries the `no-eval` label with a reason; the progress page
    and the backlog report list the gaps.

## Consequences

- `omuzaki-test` depends on `omuzaki-cli` (App on App) and on rmcp's client; no new external dependency.
- The existing scripted face eval becomes the first task file (`face.more-oval`) and drops its private test character (private test material can't be eval evidence).
- The render ops should report per-part screen rects in their frame report, so region checks need no special
  op; the likeness measures are what most face tasks check.
- Tool descriptions may not quote task prompts (a lint), so evals measure the tools rather than the quotes.
- 18 tasks are filed under QG-03 (design section 14): the harness pieces, the reference
  driver, the gate and CI, the page, the coverage rule, three starter-task batches for phase 1, eval sets and model
  suites for phase 2, synthetic sheets, the review set and the LLM judge for phase 3, and two maintainer questions (spend; real held-out data).
- Rejected:
  - a central task list (§39 rule 2);
  - checks as per-task Rust code (every task a harness change, fences collide);
  - committed project folders as fixtures (schema drift, size);
  - Claude Code or the Agent SDK as the reference driver (their prompt, tools and model move under us);
  - an LLM judge as the primary grader (cost, bias, non-determinism);
  - temperature 0 as determinism (current models reject non-default sampling and it would not be bit-stable anyway);
  - gating PRs on paid, noisy reference runs;
  - private test material "for realism".
