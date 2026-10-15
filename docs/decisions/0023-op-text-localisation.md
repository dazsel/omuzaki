---
title: "0023: Op text keeps its English in the code, gets derived message ids and typed arguments, and is rendered for the reader: agents read English, people read their language (2026-10-12)"
summary: "Op text keeps its English in the code, gets derived message ids and typed arguments, and is rendered for the reader: agents read English, people read their language"
area: decisions
---

# 0023: Op text keeps its English in the code, gets derived message ids and typed arguments, and is rendered for the reader: agents read English, people read their language (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** SE-03, SE-09; spec v3 §6, §11, §31,
§39 rule 2; ADR 0015 (registration linking), ADR 0020 (agent sessions, `ErrorDetail`, approvals); the Fluent-backed
`Strings`. Design, with the types: `docs/design/l10n-ops.md`.

## Context

SE-03 ships six languages at v1.0. These are its languages: English, Japanese, Korean, Simplified Chinese (zh-Hans),
Latin American Spanish (es-419) and Brazilian Portuguese (pt-BR). Every user-visible string must be externalised, and op ids and MCP tool names
must never be translated.

The design system's own words are already messages (`ds::Msg`). The op layer's are not. These are English literals in
implementation crates that people see in the palette, the inspector, toasts, History, approval cards and `omuzaki help`:

- `OpMeta.summary`;
- schema titles and descriptions (from doc comments, through schemars);
- about 220 `OpError` messages and hints;
- about 150 io `Finding`s;
- the licence gate's reasons;
- part-kind summaries.

Agents read the same text as their prompt, branch on codes, and copy ids into the next call. They need it stable. People
need it in their language.

## Decision

1. **Ids and data are never translated, anywhere:**
   - op ids, tool names, scope ids;
   - error codes, finding rule ids, field paths, document paths;
   - enum values and units in data;
   - file contents.

   Prose is rendered from a message id and typed arguments.
2. **Registered text keeps its English in the code, and its ids are derived.** That covers op summaries, schema
   titles and descriptions, and part-kind text.
   - Ops: `op-<name with . → ->>` for the summary (`.description` for the long text), and `--<key>` per schema level
     for argument titles and descriptions.
   - Part kinds: `kind-<id>`.
   - Later registries use their own prefix with the same walk.

   One function each, in a new API crate **`omuzaki-l10n-api`**, computes ids, walks schemas and derives titles for
   extraction, the UI, the CLI and the checks.
3. **Sentences are declared, not assembled.** Errors, findings and reasons are declared in a per-crate `messages!` table:
   - id `<crate>-<name>`;
   - English in Fluent syntax;
   - a required translator note;
   - typed arguments: `Id`, `Int`, `Num`, `Qty`, `Text`, `List`, `Verbatim`.

   The macro generates a typed function that returns a `Text`. Code never joins translated pieces or picks words for
   agreement: plurals, gender and lists are Fluent's job.
4. **One Fluent file per crate per language.** English is generated from the registries and the tables, checked like a
   golden and blessed with `OMUZAKI_BLESS=1`. Translations are files with the same ids. Each file is embedded and
   self-registered with `register_locales!()` (§39 rule 2, recorded for the ADR 0015 link gate). The shipped list is
   defined once.
5. **Errors grow; they don't change shape.**
   - `OpError` and `Finding` keep `message` and `hint` as the **English rendering**, for MCP, logs, `Display` and old
     records, and gain `text` and `hint_text` (`Option<Text>`) for people.
   - Constructors take a `Text` or, until a crate is migrated, raw English. When every crate is strict, raw English is
     removed. Internal errors and decode errors keep their untranslatable English detail as a `Verbatim` argument
     inside a localised sentence.
   - `GateError` gains `text()`.
   - ADR 0020's person-facing reasons (`Need::Person`, `NeedsPerson`, `PersonOnly`, `require_person`) become `Text`.
6. **The locale follows the reader.**
   - The UI uses the Settings language (first run from the OS) and switches without a restart.
   - CLI human output uses `--lang`, then `OMUZAKI_LANG`, then the OS locale, then English.
   - CLI `--json` is the locale-neutral serde form.
   - **MCP is always English, with no localised copy and no option.** The agent is the reader and translates for the
     person itself. The tool list must be identical on every machine (caching, the QG-03 evals, reproducible reports).
     MCP carries no client locale.
   - Elicitation, which a person reads, uses the studio's language.
   - The in-app assistant uses the English tool surface, and is told the person's language for its replies.
   - Records store ids, arguments and English, so History re-renders in the current language.
7. **Schemas are localised by lookup, never by copy.** The registered schema stays English; it is the MCP
   `inputSchema`. The inspector, palette forms and `omuzaki help` walk it for structure and ask the catalog for each
   title and description by derived id, falling back to the schema's English.
8. **"Did you mean" compares ids only.** The palette also matches localised summaries by substring, which works for
   CJK, and always shows the op id verbatim.
9. **Fallback** goes: the translation (even a stale one), then English, then the English stored with a record, then
   the id with its arguments. It is never blank. Every fallback is recorded once per locale and id. Missing English or
   a formatting error fails the crate check. Missing or stale translations are reported during development, and fail
   only the release check (`xtask l10n report --strict`).
10. **Checks:**
    - a `check_crate!` test in every registering crate: extractable text for every op and kind; English current;
      notes, prefixes and argument sets; translations parse with no orphans, no unknown placeables, and code spans
      kept; a pseudo-locale render of every message;
    - `xtask lint-l10n`: every registering crate is wired, and strict crates pass no raw English to error
      constructors;
    - cross-locale identity tests for the palette, MCP and CLI `--json`;
    - global id uniqueness in `omuzaki-cli`.
11. **Translators get context in the generated file:** where the text is shown and how much room it has, the type,
    range and default, and each argument's kind with an example. They also get the glossary. A model drafts each
    language with the glossary; native reviewers review it, and the review is recorded per language.
12. **Answers to the open questions (2026-10-12):**
    - **Traditional-Chinese systems start in English.** The first-run picker offers 简体中文 in one click, and Settings
      says Traditional Chinese is coming (SE-09). Nobody is switched to Simplified Chinese silently.
    - **CLI human output follows `--lang`, then `OMUZAKI_LANG`, then the OS locale.** Agents use `--json`
      (locale-invariant) or MCP (always English). `omuzaki --help` and the agent guide (the MCP server instructions) say
      so.
    - **Spanish is one neutral Latin-American file, `es-419`.** A sparse `es-ES` override is allowed later, and the
      chain supports `es-ES` → `es-419` → `en`.
    - **Bidi isolation marks are off** for the six left-to-right languages.

## Consequences

- **A new API crate, `omuzaki-l10n-api`.** Its dependencies are `fluent-bundle`, `fluent-syntax`, `fluent-langneg`,
  `unic-langid` and `sys-locale` (Apache-2.0 or MIT; register rows in its task), plus `blake3`. It also needs a row in
  `crates/README.md`. `ds::strings::pseudo` moves into it.
- **API changes, each reviewed as a contract change:**
  - `omuzaki-ops-api`: `OpError.text`, `hint_text`, `OpRegistration.package` and a `TextSource`. This lands after the `omuzaki-ops-api` contract change in ADR 0020,
    whose `String` reasons it retypes.
  - `omuzaki-io-api`: `Finding.text` and `hint_text`.
  - `omuzaki-store-api`: `GateError::text()`.
  - `omuzaki-char-api`: `PartRegistration.package` and a `TextSource`.
- **Every crate that registers ops, kinds or messages gains `locales/`, `register_locales!()` and `tests/l10n.rs`.**
  Op authors keep writing English summaries and doc comments. A doc comment on every argument becomes required, since
  the inspector and the agents both need it.
- **`Strings` is built over the shared catalog,** so there is one Fluent stack and one language switch.
  The existing formatters move to `omuzaki-l10n-api`. The MCP and CLI half of the old text task is new work, and the CI report moves to the xtask
  task.
- **`omuzaki-mcp`'s protocol prose stays English by design** and is exempt from the source lint.
- **Spec edit, made in this PR:** SE-03 names Spanish as es-419 and states that the locale follows the reader. SE-09
  states that Traditional-Chinese systems start in English until it ships.
- **Tasks:** the 23 issues in design section 14, as sub-issues of SE-03.

## Rejected

- **Hand-written `.ftl` as the source:** two places to edit per field, and the MCP schema drifts from the ftl.
- **The English text (or its hash) as the id:** a typo fix orphans five translations, and one English string can't
  have two context-dependent translations.
- **Ids per Rust type:** schemas are inlined, names collide and move, and context is lost.
- **MCP in the client's or the OS locale, or with a localised copy:** machine-dependent tool lists, doubled tokens,
  mixed-language quotes, and no protocol field to read.
- **Localised op names or aliases:** SE-03 forbids them; search over localised summaries covers the need.
- **`OpError.message` as a `Text`:** breaks every crate in one PR, and loses the English agents and old records need.
- **Loading `.ftl` from disk, or one central `.ftl`:** packaging, wasm and test determinism; §39 rule 2.
- **gettext:** Fluent is chosen, and keeps grammar in the translation, not in the code.
