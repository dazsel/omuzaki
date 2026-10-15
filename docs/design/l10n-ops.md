---
title: "Localising op-layer text (SE-03)"
summary: "Localising op-layer text (SE-03)"
area: design
---

# Localising op-layer text (SE-03)

**Status:** accepted (2026-10-12; ADR 0023). The answers to the open questions are in section 16. **Feature:** SE-03; SE-09 adds zh-Hant later.
**Spec:** v3 SE-03 (§31), §6 (ops, `OpMeta`, schemas from schemars), §11 (MCP), §39 rule 2 (self-registration).
**Builds on:** `docs/design/ux/design-system.md` section 12 (the `ds::Msg` table, `Strings`, the pseudo-locale), the
Fluent-backed `Strings`, ADR 0015 (registration linking), ADR 0020 (sessions, approvals, `ErrorDetail`).
**Code today:** `crates/omuzaki-ops-api/src/{lib.rs,error.rs,registry.rs}`, `crates/omuzaki-io-api/src/report.rs`,
`crates/omuzaki-store-api/src/lib.rs` (`licence_gate`), `crates/omuzaki-char-api/src/kind.rs` (`PartMeta`),
`crates/omuzaki-mcp/src/{describe.rs,render.rs,suggest.rs}`, `crates/omuzaki-cli/src/{render.rs,schema_doc.rs}`,
`crates/omuzaki-ui/src/ds/strings.rs`.

## 1. What this settles

All six languages ship at v1.0: English, Japanese, Korean, Simplified Chinese (zh-Hans), Latin American Spanish
(es-419) and Brazilian Portuguese (pt-BR). The UI's own words are already messages (`ds::Msg`). The op layer's words are not yet. Today these
are English literals in implementation crates, and people see them:

- `OpMeta.summary` (the palette, History, approval cards, `omuzaki help`);
- the schema titles and descriptions schemars takes from doc comments (the inspector, palette forms, `omuzaki help`);
- `OpError` messages and hints (about 220 call sites);
- io `Finding` messages and hints (about 150);
- the licence gate's reasons;
- part-kind summaries.

The tension: **agents need stable text and ids, and people need their language.** An agent reads tool schemas and
errors as its prompt, branches on codes, and copies ids into its next call. A person reads the same facts in the
inspector, an error toast or an approval card. The answer has three parts:

- **Identifiers and data are never translated:** op ids, MCP tool names, scope ids, error codes, finding rule ids,
  field paths, document paths, enum values in JSON, units in data and file contents.
- **Prose has a message id.** Its English source stays in the Rust code, where it is written today. It is rendered
  per locale at the surface, from an id and typed arguments, never by joining strings.
- **The locale follows the reader.** Agents read English. People read the language they chose.

## 2. Kinds of text

| Text | English source (unchanged) | Message id | Rendered by |
|---|---|---|---|
| Op summary | `OpMeta.summary` | `op-<name>` (derived) | catalog lookup by op name |
| Op long description | the argument struct's doc comment, when it says more than the summary | `op-<name>`, attribute `.description` | same |
| Argument title | the schema `title`, else derived from the key | `op-<name>--<key>[--<key>…]` (derived) | same, via the shared schema walk (section 7) |
| Argument description | the field's or variant's doc comment | the same id, attribute `.description` | same |
| Part-kind name and summary | the kind id; `PartMeta.summary`; its params schema | `kind-<id>`, `.summary`, `kind-<id>--<key>…` | same |
| Error, finding, gate reason, "needs a person" reason | a declared message (section 3.2) | `<crate>-<name>` (written) | `Catalog::render(&Text)` |
| UI component words | `ds::Msg` | `ds-…` (exists) | `Strings` over the catalog |
| Agent-facing protocol prose: MCP tool-description scaffolding, next steps, server instructions, `project.describe` | `omuzaki-mcp`, describe ops | none: English by design | not localised (section 6) |
| Transaction labels, part names, comments, anything a person or agent typed | content | none | shown as written |

The registries keep their English strings. Nothing in an op changes except where English was assembled from pieces.

## 3. Where op text lives

### 3.1 Registered text: English in the code, ids derived

An op author writes what they write today:

- the summary in `OpMeta`;
- doc comments on the argument struct, its fields and its enum variants.

The ids are **derived, not written**, so thousands of fields need no hand-made ids and an id cannot drift from its
text:

```
op-face-set_profile = Set the head's face-shape profile (outline, chin, feature layout).
    .description = Moves the outline, chin and feature layout toward a stored profile; the rest of the face keeps its values.
op-face-set_profile--blend = Blend
    .description = How far to move toward the profile: 0 keeps the current face, 1 is the full profile.
op-anim-ease--mode--Fast = Fast
    .description = Reaches the target in a third of the time.
```

Rules (Fluent identifiers are `[a-zA-Z][a-zA-Z0-9_-]*`, so dots and slashes can't appear):

- **Op:** `op-` plus the op name with `.` replaced by `-`. Op names must match `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`,
  with no `-`. The crate check (10.1) enforces it, so the mapping is one-to-one.
- **Argument path:** the op id, then `--` and each key: property names as written in JSON, the JSON value of an enum
  variant (the tag value for an internally tagged enum), recursively. Array items, map values and `Option` add no
  key. A key must be letters, digits, `_` or single `-`. Two slots that map to one id fail the check; rename one.
- **Title:** the schema's `title` if it has one. That comes from `#[schemars(title = "…")]`, or from a doc comment that
  starts with a `# Heading` line, which is schemars 1.x's rule. Otherwise the title is derived from the key:
  `max_size` becomes "Max size". Translators translate the derived English like any other title.
- **Part kinds:** `kind-<id>`. Its value is the title, derived from the kind id (`twin_tails` becomes "Twin tails").
  `.summary` is `PartMeta.summary`. The params schema gives `kind-<id>--<key>…` as above.
- **Later registries use the same walk with their own prefix**, when they are built: library items (`lib-`),
  templates (`tpl-`), formats (`fmt-`), automation nodes (`node-`), connectors (`conn-`). Plugins (§30) ship their own
  resources with their package. The `Catalog` accepts resources added at run time for them; that is the PL tasks'
  business.

The derived ids, the schema walk and the title rule are **one function each, in `omuzaki-l10n-api`**. Extraction, the
UI, the CLI and the checks all call them, so an id is computed the same way everywhere.

### 3.2 Messages: declared next to the code that says them

Errors, findings and reasons are sentences with arguments. Each crate declares them in a `messages!` table, where the
English is written once with its translator note and typed arguments:

```rust
omuzaki_l10n_api::messages! {
    /// An op named a part the character doesn't have.
    pub part_not_found = "char-part-not-found" (part: Id, character: Id)
        => "{ $character } has no part { $part }.";
    /// The hint under `char-part-not-found`.
    pub part_not_found_hint = "char-part-not-found-hint" (character: Id)
        => "List the parts of { $character } with `character.describe`.";
    /// After `part.remove` with several ids.
    pub parts_removed = "char-parts-removed" (count: Int)
        => "{ $count -> [one] Removed one part. *[other] Removed { $count } parts. }";
}

return Err(OpError::not_found(msg::part_not_found(&id, &ch)).hint(msg::part_not_found_hint(&ch)));
```

- **The macro generates one typed function per message,** returning a `Text`. A misspelt or missing argument is a
  compile error, not a `{$part}` on screen.
- **The English is Fluent syntax,** so English plurals are selectors too, and a translation can add the cases its
  language needs (Japanese has none; Korean counters, Spanish and Portuguese genders).
- **The `///` line is the translator note,** and the check requires it.
- **Message ids are `<crate>-<name>`** in kebab case. `<crate>` is the package name without `omuzaki-` (`char`,
  `io-ops`, `ops-api`). `omuzaki-ui` keeps `ds-` for components and uses `ui-` for the rest. `op-` and `kind-` are
  reserved for derived ids. The prefix makes ids unique across crates by construction. The check (10.1) verifies the
  prefix, and a test in `omuzaki-cli`, which links every crate, verifies global uniqueness.
- **A message is a whole sentence or a whole label.** Code never joins translated pieces ("Removed" + "3 parts"), never
  picks between English words to agree with a number or a gender, and never builds a list by joining with ", ".
  Plurals, gender and other agreement are Fluent selectors on an argument. A list is an `Arg::List`.
- **Rewording keeps the id; a change of meaning gets a new id.** Rewording marks the five translations stale (9.3). A
  new id makes them missing, so nobody ships a translation of a sentence that no longer exists.

### 3.3 Fluent resources: one file per crate per language, generated English, self-registered

```
crates/omuzaki-char/locales/en/omuzaki-char.ftl       generated: ops, part kinds, messages of this crate
crates/omuzaki-char/locales/ja/omuzaki-char.ftl       translations, one per shipped language
crates/omuzaki-char/locales/zh-Hans/omuzaki-char.ftl
...
```

- **English is generated.** The crate's l10n test (10.1) writes `locales/en/<crate>.ftl` from the crate's registered
  ops, kinds and message tables, and fails when the committed file differs. `OMUZAKI_BLESS=1 cargo test -p <crate>
  --test l10n` rewrites it, like goldens. Nobody edits it by hand. A rebase conflict in it is resolved by blessing
  again.
- **Translations are hand-maintained files** with the same ids. The drafting tool keeps the English order. Each entry
  starts with `# en: <hash>`, the first 8 hex digits of the BLAKE3 hash of the English entry it translates (value and
  attributes). That is how staleness is found (9.3).
- **Every file is embedded and self-registered.** `omuzaki_l10n_api::register_locales!()` goes once in the crate's
  `lib.rs`. It `include_str!`s the English file and one file per shipped language, and submits them to `inventory`
  (§39 rule 2, no central file). It is recorded for the link gate like any registration (ADR 0015), so a binary that
  forgot a crate's text fails its `registrations.rs` test. A shipped language without its file is a compile error;
  blessing creates empty stubs for new languages.
- **The shipped list is defined once,** in `omuzaki-l10n-api` (`SHIPPED`: `en`, `ja`, `ko`, `zh-Hans`, `es-419`,
  `pt-BR`). The macro's list is checked against it. SE-09 adds `zh-Hant` in that one place, and blessing adds the
  stubs.
- **Regional overrides are allowed later,** in the same place (`OVERRIDES`, empty at v1.0): a locale with a parent in
  `SHIPPED` whose files hold only the entries that differ. For example, `es-ES` over `es-419`, if real users ask.
  Override files are sparse, so the report never counts their absent entries as missing. The chain is in 9.1.
- **Embedding, not loading from disk:** there is no install layout to get wrong, it works on wasm, and tests are
  deterministic. All six languages of op text are a few hundred KB of source. Only English and the current locale are
  parsed.

## 4. `Text`: an id and typed arguments

```rust
pub struct Text { pub id: Cow<'static, str>, pub args: Vec<(Cow<'static, str>, Arg)> }

pub enum Arg {
    Id(String),                          // op id, part id, scope, path, file name, hex colour: shown as is
    Int(i64),                            // counts; drives plural selectors; locale digits and grouping
    Num(f64),                            // measured values; locale decimal separator
    Qty { value: f64, unit: Unit }, // 12.5 cm, 30°, 40 %: locale formatting; the unit stays an id in data
    Text(Box<Text>),                     // a nested localisable phrase: a part-kind name, a scope's display name
    List(Vec<Arg>),                      // the locale's list pattern ("a, b and c"), never a joined string
    Verbatim(String),                    // English we can't translate: an OS error, serde's reason, a parser message
}
```

- **JSON:** `{"id": "char-part-not-found", "args": {"part": {"id": "hair-b"}, "character": {"id": "mira"}}}`. The
  enum is externally tagged, in snake case, with a JsonSchema. It is plain data, so records store it and replays,
  History and the Build Theatre render it again in whatever language is current.
- **`Id` and `Verbatim` are never touched,** by translators or by the pseudo-locale. The difference matters to the UI:
  verbatim English goes in the error's Details disclosure, an id is shown as an id. To the tests, both are allowed to
  stay ASCII.
- **Gender and agreement** select on an `Id` argument, such as a pronoun set the person chose or a kind id, inside the
  Fluent pattern.

## 5. Errors, findings and reasons

**`OpError` grows; it doesn't change shape.** Changing `message: String` into a `Text` would force every crate to
migrate inside one contract PR. Instead, two fields are added and the existing ones keep their meaning:

| Field | Meaning |
|---|---|
| `code`, `field`, `detail` | unchanged: what agents and scripts branch on (`detail` is ADR 0020's) |
| `message`, `hint` | **the English rendering**, always present: what MCP, logs and `Display` show, and the fallback for a record whose id a later build no longer knows |
| `text`, `hint_text` | **new**, `Option<Text>`, serde default and skipped when `None`: what a person's surface renders. `None` only for errors not yet migrated |

- **Constructors take `impl Into<Message>`.** `Message` is `English(String)` or `Text(Text)`, so
  `OpError::not_found("…")` still compiles while a crate migrates. Given a `Text`, the constructor fills `text` and
  renders `message` with `Catalog::english()`, one Fluent format per error, which is rare and off the real-time path.
  `.hint(..)` works the same way.
- **Once every crate is strict (10.3), the English-string conversions are removed.** Raw English can't come back then.
  - Two cases keep a string, wrapped as a `Verbatim` argument of a fixed message: `OpError::internal(detail)` (a bug;
    the person reads "Something went wrong inside Omuzaki" in their language, and the English detail goes to the bug
    report) and `decode_failed` (serde's reason).
- **`Finding`** (io reports) gets the same `text` and `hint_text` beside `message` and `hint`. Its `rule` stays the
  stable id agents branch on. A rule may have several messages.
- **`GateError`** (the licence gate) gets `fn text(&self) -> Text`, one declared message per variant. The rights check
  and the export ops put that text in their findings and errors.
- **ADR 0020's person-facing reasons** are `Text`, not `String`: `Need::Person { reason }`,
  `ErrorDetail::NeedsPerson { reason }`, `ErrorDetail::PersonOnly { reason }`, and `ctx.require_person(reason)`. The
  approval card shows the reason to a person; the agent's MCP error carries its English rendering. The agent sessions contract lands those
  types with `String`; the ops-api task here retypes them.
- **Op outputs are data.** When a query's or an edit's output needs a sentence for people, the output carries a
  `Text` value (the JSON above), never pre-rendered prose. Outputs written for agents (`project.describe`) stay English
  prose plus structure. The UI renders their structure, never their prose.

## 6. How each surface picks a locale

| Surface | Reader | Locale |
|---|---|---|
| UI (editor, gallery, overlays) | person | The language in Settings. On first run it comes from the OS preference list through `negotiate()`. Switching applies on the next frame, without a restart, and re-renders History and the Theatre from their stored ids |
| CLI human output (`omuzaki help`, tables, error lines) | person | `--lang <tag>`, else `OMUZAKI_LANG`, else the OS locale (`LC_ALL`, `LC_MESSAGES`, `LANG` on Unix; the user's UI language on Windows; via `sys-locale`), else `en`. `C` and `POSIX` mean `en`. **Agents use `--json` or MCP, never the human output.** `omuzaki --help` and the agent guide, the MCP server instructions, say so |
| CLI `--json` | program | none: the serde form of the contract types (English `message` plus `text` ids and arguments), byte-identical in every locale |
| MCP over stdio and HTTP: `tools/list`, descriptions, input schemas, instructions, results, errors, `omuzaki://` resources | agent | **English, always, with no option.** Structured results keep `code`, `rule`, `field` and `signature`. No message ids, no localised copy |
| MCP elicitation, when a tool asks the person something through the client | person (in the client's UI) | the studio's UI language when bridged, else the CLI rule |
| In-app assistant (AG-01) | agent (LLM) | the same English tool surface as MCP (ADR 0020 section 10). The Agent panel renders its steps for the person from the records. The assistant's prompt names the person's UI language, so it answers in that language |
| Op log, History, Build Theatre | person, rendered again | stored as ids plus arguments (plus the English); rendered in the current UI language |
| `tracing` logs, panics, bug reports | developer | English |

**Why MCP is always English, with no localised copy alongside:**

- The agent is the reader. It already speaks to the person in the person's language, and turns a result into that
  language better than a second copy would.
- A localised copy doubles the tokens of every error, and invites the agent to quote half-translated text back.
- The tool list must be byte-identical on every machine: for prompt caching, the agent eval suite (QG-03), scripts and
  reproducible bug reports. A cross-locale test checks it (section 10).
- MCP has no client locale to honour. rmcp 3.5 carries none in `initialize`. If a later protocol revision adds one,
  this record is revisited by a new ADR.

**Negotiation** (`negotiate(&[preferences]) -> Locale`, on `fluent-langneg` with our own rules on top):

- `en-*` gives `en`, `ja-*` gives `ja`, `ko-*` gives `ko`, and `pt-*` gives `pt-BR`.
- `es-*` gives `es-419`, the Spanish of v1.0 (neutral Latin American). Once an override such as `es-ES` ships, `es-ES`
  gives itself, and its chain does the rest.
- `zh-Hans`, `zh-CN` and `zh-SG` give `zh-Hans`.
- **`zh-Hant`, `zh-TW`, `zh-HK` and `zh-MO` give `en`, never `zh-Hans`.** Nobody is switched to Simplified Chinese
  silently. The first-run language picker offers 简体中文 in one click, and the Settings language list carries the
  SE-09 note that Traditional Chinese is coming. This holds until SE-09 ships `zh-Hant`.
- Anything else gives `en`.

## 7. Schemas at the surface: the agent's schema never changes

- **The registered schema stays the English schema**, generated by schemars from the doc comments. It is the MCP
  `inputSchema` and `omuzaki help --json`.
- **A person's surface keeps the schema's structure and swaps only its words.** The inspector, the command palette form
  and `omuzaki help` walk the English schema for types, ranges, defaults and enum values. For every title and
  description they ask the catalog, by the derived id, using the same walk as extraction:
  `catalog.arg_title(owner, &path)` and `catalog.arg_description(owner, &path)`. If the catalog has nothing, they fall
  back to the schema's English.
- **No localised copy of a schema exists anywhere.** Nothing can hand a translated schema to an agent by mistake.
- **Enum values in the UI** show the variant's localised title. The JSON value sent is unchanged. Ranges and defaults
  are formatted for the locale. The value an agent sends is always the JSON number.

## 8. "Did you mean" stays on ids

- Suggestions compare what was typed with **ids only**: tool names, op names, argument keys, kind ids and enum values.
  That is `omuzaki-mcp`'s `suggest`, the shared edit-distance helper, and the CLI's unknown-op path. They never
  compare with localised titles.
- The suggested ids are `Arg::Id` arguments. The sentence around them is a message: localised in the CLI and UI,
  English over MCP.
- **The palette** matches a query against the op id (fuzzy, edit distance) **and** against the localised summary
  (substring, which also works for CJK, where edit distance means nothing). A row shows the localised summary with the
  op id beside it, verbatim. Searching for an English word in a Japanese UI still finds the op by its id.

## 9. Fallbacks and missing text

### 9.1 The chain, per message (and per nested `Text`)

1. **The locale's translation,** even a stale one (9.3). For a regional override, first its own sparse file, then its
   parent's: `es-ES` → `es-419` → `en`.
2. **English,** from the embedded English resources. It is always complete for current ids, because the check
   requires it.
3. **The English stored with the record** (`OpError.message`, `Finding.message`, passed to `render_stored`), for an
   id this build does not know: an old log entry, or a record from a newer build.
4. **Last resort:** the id and its arguments, `⟦char-part-not-found: part=hair-b, character=mira⟧`. Never blank,
   never a panic.

A Fluent formatting error (an unknown argument, a selector with no default) keeps the output Fluent produced. Fluent
leaves `{$name}` visible, like `ds::fill` does.

### 9.2 Reporting

- Every fallback from step 1, and every formatting error, is recorded once per (locale, id) in `Catalog::missing()`.
- Debug builds log each one once; the UI's diagnostics panel shows the count.
- **The crate check fails on any missing English or any formatting error.** Missing translations don't fail CI during
  development, so a new op doesn't wait for five languages. They are reported (9.3), and the release check requires
  zero.

### 9.3 Missing and stale translations

`cargo xtask l10n report` lists, per crate and per language:

- **missing:** an English id with no translation;
- **stale:** the `# en:` hash differs from the current English entry;
- **orphans:** a translated id English no longer has;
- **untranslated text:** raw English still in `OpError`/`Finding` constructors (10.3).

`--strict`, run by the release workflow and nightly, fails on any missing or stale entry in a shipped language. That is
the gate for "all six languages at v1.0". At run time a stale translation is still used, because rewording is the
common case, while a change of meaning gets a new id (3.2).

## 10. Tests and lints

### 10.1 The crate check (every crate that registers text)

`omuzaki_l10n_api::check_crate!(omuzaki_char);` in `crates/<crate>/tests/l10n.rs` expands to tests that call the crate's
`link()` and then:

- **Extractable text for every op and kind of the package:**
  - a non-empty, one-line summary;
  - a description on every argument node and every enum variant;
  - an op name that fits the grammar;
  - ids that collide nowhere.
- **The English file is current** (bless with `OMUZAKI_BLESS=1`).
- **Declared messages:**
  - each has a translator note and the crate's prefix;
  - its English parses;
  - its placeables are exactly its declared arguments.
- **Every translation file:**
  - parses;
  - has no orphan ids and no placeable the English lacks;
  - keeps every backticked code span of the English verbatim (op ids inside summaries, for example).
- **Pseudo-locale run of every message of the crate:** each is rendered in `en-XA`, with sample arguments for each
  declared kind (`Int` as 1, 2 and 5 to reach the plural branches; a sample id; a known nested `Text`).
  - It fails on a formatting error.
  - It fails on a plain ASCII letter outside `Id` and `Verbatim` values: that is text that skipped the catalog.
  - It fails when a message doesn't grow, which would mean the pseudo transform never reached it.

### 10.2 Whole-surface checks

- **The UI** runs every panel story in the pseudo-locale and fails on plain English. That covers op text in the
  palette, the inspector, toasts and History.
- **Ids never change across locales:**
  - palette rows, "Copy as MCP call" and scope chips under `en-XA` and one real locale;
  - `tools/list`, an MCP error result and CLI `--json` output, byte-identical whatever the process locale or the UI
    language.
- **Global uniqueness of all message ids,** in `omuzaki-cli`, which links every crate.

### 10.3 Source lint and strictness

`cargo xtask lint-l10n`, part of `xtask check`:

- **Every registering crate has its text wired:** a crate that calls `register_op!`, `register_part_kind!` or
  `messages!` also has `register_locales!()` in `lib.rs` and `tests/l10n.rs` with `check_crate!`. This is the "every
  op has extractable text" gate end to end, and it can't be forgotten by a new crate.
- **No raw English in a strict crate:** in a crate marked `l10n = "strict"` under `[package.metadata.omuzaki]`, the
  non-test sources must not pass a string literal or a `format!` to an `OpError` or `Finding` constructor, `.hint(..)`
  or `require_person(..)`. Each crate's migration task sets the flag. When all are strict, the flag goes and the
  conversions go with it.
- **Exempt:**
  - `omuzaki-mcp` (agent-facing English by design, section 6);
  - mocks and `omuzaki-test` (never shipped);
  - `xtask`;
  - `tests/`, `benches/` and `examples/`.

## 11. Context for translators

- **Generated comments on every English entry,** written by the extraction:
  - for ops: the kind, scope and op name, and where the text is shown with its room ("palette row and History step,
    one line"; "inspector label, about 24 characters"; "tooltip");
  - for arguments: the type, range and default;
  - for messages: the note and each argument's kind with an example (`$part (id, never translated): "hair-b"`).
- **The glossary** `docs/l10n/glossary.md` fixes the product terms per language: part, variant, colourway,
  transaction, grant, scope, take. Drafts must use it.
- **Ids are visible as ids:** placeables can't be translated, and the check keeps backticked code spans verbatim.
- **Drafting and review:** a model drafts each language, using the English file, the comments and the glossary, and
  stamps `# en:` hashes. Native reviewers arranged by the maintainers review it, and the review is recorded per language in the
  glossary file. The drafts are the op-text translation tasks of section 14.
- **In-place context:** `gallery --pseudo --screenshots` and the panel stories show every surface. A reviewer who
  doubts a string checks it in the running UI in their language.

## 12. Performance

- **One catalog per locale,** built when the locale is chosen: it parses the English and the chosen language's
  resources from every registered crate. The budget is 20 ms for the whole product's text, measured by a bench in
  the `Catalog` task. Other languages are parsed only when someone switches to them. The window's 1 s interactive budget (§10)
  keeps its margin.
- **Static text** (summaries, titles, descriptions: no arguments) is formatted once per locale and cached as `Arc<str>`,
  so drawing a palette row allocates nothing (design-system section 9 rule 4).
- **Text with arguments** is rendered once into the panel's view model, when the value arrives or the locale changes
  (rule 5), never per frame.
- **Nothing on a real-time path** (performer loop, audio, tracking) formats text. Live HUD words are UI messages,
  rendered when they change.
- **Unicode bidi isolation marks** (Fluent's default FSI/PDI around placeables) are off. All six languages are
  left-to-right, and agents and tests must see clean text. The UI turns isolation on for right-to-left locales if one
  is ever added.

## 13. Contract changes (binding names; doc comments are the implementer's)

**New API crate `omuzaki-l10n-api`.** It depends on `omuzaki-core`, `fluent-bundle`, `fluent-syntax`, `fluent-langneg`,
`unic-langid`, `sys-locale` and `blake3`; licence rows go in the register.

- **Types:** `Text`, `Arg`, `Unit` (the set formats), `Message { English(String), Text(Text) }` with `From<&str>`,
  `From<String>` and `From<Text>`, and `Locale` (a validated BCP 47 tag).
- **Locales:** `SHIPPED: &[Locale]` (with `Locale::ES_419`), `OVERRIDES: &[(Locale, Locale)]` (override and parent;
  empty at v1.0), `Locale::parent()`, and `Locale::PSEUDO` (`en-XA`).
- **Ids:** `ids::op(name)`, `ids::kind(id)`, `ids::arg(owner_id, &[key])`, `ids::check_op_name(name)`.
- **`schema_text::walk(&Schema, |path, node|)`** and `schema_text::title(node, key)`: the one schema walk and title rule.
- **Macros:** `messages!` (typed functions, `MessageTable` registration, verbatim marker) and `register_locales!()`
  (`Resource { package, locale, source }` registration).
- **`TextSource`:** registries contribute their text through it (`fn(package) -> Vec<RegisteredText>`).
- **`pseudo()` and `accent()`** move here from `ds::strings`, which re-exports them.
- **`negotiate(&[&str]) -> Locale`** and **`os_locales() -> Vec<String>`**.
- **`Catalog`:**
  - `Catalog::new(Locale)`, `Catalog::english() -> &'static Catalog`, `locale()`;
  - `render(&Text) -> String` and `render_stored(&Text, english)` (step 3 of the chain in 9.1);
  - `op_summary(name)`, `op_description(name)`, `kind_title(id)`, `kind_summary(id)`, `arg_title(owner, &path)` and
    `arg_description(owner, &path)`, each returning a cached `Arc<str>` or `None`;
  - `missing() -> Vec<Missing>`.
- **The check:** `check_crate!` and its support module (feature `check`, used only from tests).

**`omuzaki-ops-api`:**

- `OpError.text` and `OpError.hint_text`, `Option<Text>`;
- constructors and `.hint` taking `impl Into<Message>`;
- `OpRegistration.package`, filled by `register_op!` from `CARGO_PKG_NAME`;
- a `TextSource` for ops;
- the crate's own messages (`decode_failed`, `unavailable`, `internal`, the runner's refusals);
- ADR 0020's reasons as `Text`.

**`omuzaki-io-api`:** `Finding.text` and `Finding.hint_text`; constructors and `.hint` taking `impl Into<Message>`.

**`omuzaki-store-api`:** `GateError::text()`.

**`omuzaki-char-api`:** `PartRegistration.package` and a `TextSource` for part kinds.

## 14. Tasks

All are tasks of SE-03, in Phase 1. Each carries its fence, contract and acceptance checks.

| Task | Pri |
|---|---|
| `omuzaki-l10n-api`: `Text`, `Arg`, `Message`, `Locale`, ids, `messages!`, `register_locales!`, `pseudo` (new API crate) | P0 |
| `omuzaki-l10n-api`: `Catalog` (bundles, English, fallback chain, missing report, pseudo transform, negotiation, caches, bench) | P0 |
| `omuzaki-l10n-api`: the schema walk, extraction and `check_crate!` | P0 |
| `omuzaki-ops-api`: op text and localisable errors | P0 |
| `omuzaki-char-api`: part-kind text | P0 |
| `omuzaki-io-api` and `omuzaki-store-api`: localisable findings and gate reasons | P0 |
| migrate `omuzaki-io-api`'s own findings | P0 |
| migrate `omuzaki-ops` and `omuzaki-anim` | P0 |
| migrate `omuzaki-char` ops: part, layout, character (wires the crate) | P0 |
| migrate `omuzaki-char` ops: variant, weights, palette | P0 |
| migrate `omuzaki-face` | P0 |
| migrate `omuzaki-io-ops` (ops, errors, findings, licence wording) | P0 |
| migrate `omuzaki-io-vrm` and `omuzaki-gltf` findings | P0 |
| `omuzaki-cli`: locale choice and localised human output | P0 |
| `xtask lint-l10n` and `xtask l10n report` | P0 |
| MCP and CLI `--json` are the same in every locale; message ids unique across the product (tests) | P0 |
| remove the raw-English conversions once every crate is strict | P1 |
| `omuzaki-mcp`: the agent guide (server instructions) states the locale rules | P0 |
| op-text translation drafts: ja, ko, zh-Hans, es-419, pt-BR | P0 |

Re-cut existing tasks:

- **The UI `Strings` table** is built over the shared `Catalog` (no second Fluent stack in `omuzaki-ui`); it waits for the schema walk.
- **The formatters** (numbers, units, dates, relative ages) live in `omuzaki-l10n-api`, so the CLI and `Arg::Qty`
  use them too; the decimal-comma parser stays in `ds/expr.rs`.
- **The id-stability test** keeps the UI checks; the MCP and CLI half is the "same in every locale" task above.
- **The glossary task** keeps the glossary, the ds translations and the review process; the CI report moved to the `xtask lint-l10n` task.
- **The pseudo-locale UI test task** is confirmed as it was.

## 15. Alternatives rejected

- **Hand-written `.ftl` as the source, with ops naming ids.** Every op field would be edited in two places, and the
  doc comments, which are the MCP schema, would drift from the ftl.
- **The English text (or its hash) as the id, gettext style.** A typo fix would orphan five translations, and the same
  English in two places would get one translation where context needs two.
- **Ids keyed by Rust type, so a shared type is translated once.** Schemas are inlined, type names collide across
  crates and move with refactors, and per-op ids give translators the context. Translation memory absorbs the repeats.
- **MCP in the client's or the OS locale, or English plus a localised copy.** Section 6.
- **Localised op names or aliases.** SE-03 forbids translating ids. Search over localised summaries gives people the
  same reach.
- **Changing `OpError.message` into a `Text`.** It would break every crate in one PR. The additive fields let crates
  migrate one at a time, and keep the English that agents and old records need.
- **Loading `.ftl` files from disk at run time:** install paths, wasm and test determinism, for a few hundred KB.
- **One central `.ftl`:** §39 rule 2.
- **gettext / `.po`.** Fluent is already chosen. It puts plurals, gender and per-language cases in the
  translation, not in the code.
- **Translating a stale string back to English at run time.** Most English edits are rewording. Meaning changes get a
  new id, and the release check catches the rest.

## 16. Decisions on the open questions (2026-10-12)

1. **Traditional-Chinese systems start in English.**
   - The first-run language picker offers 简体中文 in one click.
   - The Settings language list says Traditional Chinese is coming (SE-09).
   - Nobody is ever switched to Simplified Chinese silently.

   The UI side is the `Strings` table in `omuzaki-ui`.
2. **CLI human output follows `--lang`, then `OMUZAKI_LANG`, then the OS locale.**
   - Agents use `--json` (locale-invariant) or MCP (always English).
   - `omuzaki --help` says so, and so does the agent guide, the MCP server instructions.
3. **Spanish is one neutral Latin-American file, `es-419`,** the Spanish of v1.0. A small `es-ES` override file is
   allowed later if real users ask. The catalog's chain supports `es-ES` → `es-419` → `en` from the start.
