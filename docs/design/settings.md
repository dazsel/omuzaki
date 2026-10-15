---
title: "Settings: the registry, the store, profiles, session restore and release notes"
summary: "the registry, the store, profiles, session restore and release notes"
area: design
---

# Settings: the registry, the store, profiles, session restore and release notes

**Status:** maintainer answers folded in (§9); awaiting acceptance. **Feature:** SE-10. **Decision record:** `docs/decisions/0026-settings.md`.
**Spec:** v3 §4.1 (UI and agents are peers), §6 (ops, scopes), §7 (project layout), §31 (SE-03, SE-04, SE-08, SE-10), CN-05 (keys in the OS keychain), VC-08 (crash recovery), §39 rule 2 (self-registration).
**Builds on:** `docs/design/ops-api.md` (services, `Undo::Explicit`, `require_person`), `docs/design/l10n-ops.md` (derived message ids), `docs/design/ux/shell.md` §7 and §17 (schema-generated forms; what the shell saves), `docs/design/audio-api.md` §2.2 (the machine profile), `docs/design/journal.md` and ADR 0021 (recovery), `docs/design/scenes.md` (what a scene owns), ADR 0013 (file roots), ADR 0023 (the locale follows the reader).

This page explains the decisions so an implementer or reviewer can disagree with a reason. The types are shown as the contract they will have; their doc comments are the implementer's.

## 1. What this covers

| Asked for | Where |
|---|---|
| What a setting is, against project data and secrets; the registration | D1, D2 |
| Scopes `User` and `Machine`; who may change which key | D3, D4 |
| Storage: TOML, atomic writes, unknown keys kept, per-key migrations | D5, §4 |
| Profiles: overlay, switching, "share" | D7, D8 |
| Session restore; crash recovery; the opt-out | D9 |
| Release notes: format, drafting, languages, shown once, Help | D10 |
| The SE-10 work cut | §6 |

**Not covered, on purpose:** the updater itself (SE-08: delta updates, rollback, the beta channel's plumbing); the keychain wrapper (CN-05); the contents of each phase 2 feature's settings (each registers its own keys); the Settings window's visual design (`shell.md` §7 already fixes the schema-generated form).

## 2. The shape of it

```
 any crate                      omuzaki-settings-api (contract)            omuzaki-settings (implementation)
 register_setting!(UiScale) ──► SettingRegistration (inventory)  ──────►  Store: layers + snapshot + events
                                trait Settings (the service)              settings.toml   (User, config dir)
 ops (settings.*, session.*) ─► ctx.service::<dyn Settings>()  ────────►  profiles/*.toml (User, config dir)
   person: UI, CLI                                                        machine.toml    (Machine, local data dir)
   agent: MCP, assistant                                                  session.toml    (state, local data dir)
 consumers (renderer, audio) ◄─ Snapshot (Arc, no alloc) + SettingsEvent
```

## 3. Decisions

### D1. Four kinds of persisted thing; only the first is a setting

| Kind | Examples | Lives in | Why |
|---|---|---|---|
| **Setting** | UI theme and scale, quality tier, audio device per role, update channel, startup behaviour, tracker preferences | the user's config dir or local data dir (D3) | taste or hardware of a person, not of a project |
| **Project state** | characters, variants, scenes and their lights, shows (outputs, hotkeys, redeems), `default_scene`, the last workspace (`project.toml`, `shell.md` §17) | the project folder | travels with the project, undoable, mergeable, branchable |
| **Secret** | API keys, stream keys, tokens | the OS keychain only (CN-05) | never in a file, log, op record or MCP output |
| **State** | recent projects, window geometry, "last version seen", calibration, model acknowledgements, "my library" | their own stores (`session.toml`, ADR 0017, `library.md`) | written by the app as a side effect, not chosen on a page, so it has no op to "set" it and no default |

The test: *would a second person who opens this project want it?* Yes: project state. *Does it name a key?* Secret. *Did the person choose it on a Settings page or tell an agent to?* Setting. Otherwise state.

- A setting may hold a **reference** to a secret (`SecretRef`, a label such as `connector/openai`), never the secret. `SecretRef` has no accessor for the value, so a setting's value can be logged, put in an event or shown to an agent without a leak.
- A project never holds a setting. Where the spec says `project.toml` has "settings" (§7), that means project state such as the default character. After D11 a project holds no settings at all, not even file roots.

### D2. A setting is a typed key that registers itself

```rust
/// Where a setting's value lives and whom it follows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// The person's taste: follows them to another computer (config dir, which may roam).
    User,
    /// Bound to this computer's hardware or paths: audio devices, GPU choices, file roots, the update channel.
    Machine,
}

/// What an agent may do to this key. Every actor may read every key (scope `read`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentWrite {
    /// Any agent holding `edit:settings` (harmless taste: theme, density).
    Free,
    /// The write is queued for the person (`OpCtx::require_person`): outputs, anything that can start capture or spend.
    Confirm,
    /// Never (`OpError::person_only`): privacy, consent, devices, file roots, update channel.
    Never,
}

/// Whether a profile (D7) may hold this key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InProfile { Member, Excluded }

/// When a change takes effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// Subscribers react to the event now.
    Live,
    /// Saved now; the UI says "applies after restart".
    NextStart,
}

/// Static facts about a setting. Every field is required: there is no default, so the author decides each one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SettingMeta {
    /// `<area>.<name>[.<name>…]`, lowercase snake segments, at least two. No key is a prefix of another.
    pub key: &'static str,
    /// English, one line (message id derived, as l10n-ops D3.1).
    pub summary: &'static str,
    pub scope: SettingScope,
    pub agent: AgentWrite,
    pub profile: InProfile,
    pub apply: Apply,
    /// Bumped when the stored shape changes (D5).
    pub version: u32,
}

/// A typed setting. The marker type is the key: `settings.get::<UiScale>()`.
pub trait Setting: 'static {
    /// The value. Its JSON Schema (with `x-ui.*` annotations, `shell.md` §7) generates the Settings page and the agent's schema.
    type Value: Clone + PartialEq + fmt::Debug + Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static;
    const META: SettingMeta;
    /// The default. Always valid, so a missing or invalid stored value falls back to it.
    fn default() -> Self::Value;
    /// Rules a type cannot say (ranges, cross-field). Runs on every write and on load.
    fn check(_value: &Self::Value) -> Result<(), SettingError> { Ok(()) }
    /// Brings a value stored at `from` (< `META.version`) to the current shape.
    fn migrate(_from: u32, old: Value) -> Result<Value, SettingError> { Ok(old) }
    /// Earlier keys this one replaces (a rename), newest first.
    const RENAMED_FROM: &'static [&'static str] = &[];
}

/// `register_setting!(UiScale);` next to the type. Nothing edits a central list (§39 rule 2).
pub struct SettingRegistration { /* meta, schema(), default_json(), check, migrate, renamed_from */ }
pub fn registrations() -> impl Iterator<Item = &'static SettingRegistration>;
pub fn find(key: &str) -> Option<&'static SettingRegistration>;
```

Example (the first real user replaces `ds::Settings` in `omuzaki-ui`):

```rust
/// UI scale in percent, 75 to 200.
pub struct UiScale;
impl Setting for UiScale {
    type Value = u16;
    const META: SettingMeta = SettingMeta {
        key: "ui.scale", summary: "UI scale in percent",
        scope: SettingScope::User, agent: AgentWrite::Free, profile: InProfile::Member,
        apply: Apply::Live, version: 1,
    };
    fn default() -> u16 { 100 }
    fn check(v: &u16) -> Result<(), SettingError> { SettingError::range("ui.scale", *v, 75, 200) }
}
register_setting!(UiScale);
```

**Why a marker type per key and not a central enum or a string table:** a feature crate owns its keys, a central list would be a merge hotspot with hundreds of contributors (§39 rule 2), and the compiler checks each call site's type. **Why every field is required:** the three safety choices (scope, agent, profile) are exactly the ones a default would get wrong silently. **Why no JSON Schema written by hand:** it is derived (`schemars`, already a workspace dependency) from the value type, as op schemas are, so the Settings page and the agent's `settings.list` cannot drift from the code. A `tests/settings.rs` in each crate registering keys (the `check_crate!` pattern of l10n-ops §10.1) fails a key with a bad name, a prefix clash, a default that fails `check`, a default that does not round-trip through TOML, a missing summary, a `Machine` key marked `Member`, or a schema containing `format: "secret"`.

**Names.** Segments are the area and the setting: `ui.theme`, `quality.tier`, `audio.bindings`, `io.fs_roots`, `update.channel`, `startup.reopen`, `mascot.opacity`. The area is the Settings page section; its title is a message `settings-area-<area>` in the owning UI crate (no registry of areas; a missing title falls back to the capitalised segment and the l10n report lists it).

**Localised text.** The summary, the doc comments on the value type and its enum variants follow l10n-ops §3.1: English in code, derived ids (`setting-ui-scale`, `setting-ui-scale--unit`), `locales/en/<crate>.ftl` generated, the six languages from the translation pipeline. Keys and enum values are never translated (they are stored and sent by agents).

### D3. Two scopes, four files, and where each lives

| File | Holds | Directory | Roams? |
|---|---|---|---|
| `settings.toml` | every `User` key | the user config dir | may (Windows `%APPDATA%`, `~/.config`, `~/Library/Application Support`) |
| `profiles/<id>.toml` | profile overlays (D7), `User` keys only | the user config dir | with `settings.toml` |
| `machine.toml` | every `Machine` key; the active profile id | the local data dir (`%LOCALAPPDATA%`, `~/.local/share`, `~/Library/Application Support`) | never |
| `session.toml` | session state (D9) | the local data dir | never |

**Why two scopes and not one file:** the config dir may sync between computers. Audio device ids, file roots and GPU choices are wrong on the second computer and must not arrive there; theme and scale should. Putting the two in different directories makes the rule physical: a `Machine` value cannot roam because it is not in a roaming folder. **Why not "project" as a third scope:** a project is not a place a person's preferences belong (D1); the one case that looks like it is D11.

**Directories** come from `directories` 6.0 (MIT OR Apache-2.0; pulls `dirs-sys` 0.5, MIT OR Apache-2.0, and `option-ext` 0.2, MPL-2.0, which policy allows unmodified, so the register row notes it). `OMUZAKI_CONFIG_DIR` and `OMUZAKI_DATA_DIR` override them (tests, portable installs). The store takes a `Paths` value; tests and the mock never touch the real directories.

### D4. Settings are read and written through ops; the service holds the state

Ops are pure functions of a *document* and return a `Patch` (`ops-api` lib docs). Settings are not in the document, and must not be: the journal, undo, branches (ADR 0025) and recovery replay would then capture and replay one person's taste inside a project another person opens. The precedent for state outside the document is the audio machine profile and the library (`Undo::Explicit` plus a service). So:

- `omuzaki-settings-api` defines the service trait `Settings` (§5). `omuzaki-settings` implements it; the app wires it with `ServicesBuilder::insert::<dyn Settings>`; ops reach it with `ctx.service::<dyn Settings>()?`. A missing service is `Unavailable` (ops-api rule 4).
- The `settings.*` ops are edits with `Undo::Explicit` and an empty patch. They pass `Mode::DryRun` when `ctx.dry_run` is set and `Mode::Apply` otherwise (ops-api rule 6), and the service does the rest: under `DryRun` it runs the same validation and returns the same `Change`, `Option<Change>`, `ProfileInfo` or report as an apply, but writes nothing, swaps no snapshot, records nothing and emits no event. The ops do not work `before` and `after` out for themselves, because a reset's `after` can be a base value that `resolve` cannot see under a profile override. Every mutating method also takes the `Actor`, so a deleted active profile's side effect is attributed to the person who asked.
- **No history.** A settings change is not an undo step (Ctrl+Z does not flip your theme while you edit a character). Each row has "reset to default"; the service keeps the last 200 changes in memory (`SettingChange { key, before, after, actor, at }`, read by `settings.changes`), which is how the UI shows "changed by an agent 2 min ago" (the violet marker of `shell.md` §7).
- **Who may write what.**
  - A new scope `edit:settings` (`Scope::EditSettings`) is needed by `settings.set`, `settings.reset` and `settings.profile.*`. It joins `Grants::agent_default()` (spec §6: agents get `read`, `edit:*`, `render`), because the key's own `AgentWrite` is the real gate:
    - `Free`: allowed.
    - `Confirm`: `ctx.require_person("changes <key>")`, so the executor queues it for the person (ADR 0020).
    - `Never`: `OpError::person_only(..)`, never queued.
  - A person may write any key. Reading needs only `read`.
  - A profile switch by an agent is `Confirm` as a whole (it may change many keys the agent could not set one by one).
- **Assignments for the keys we already know** (each key's owning area confirms its own):

| Key | Scope | Agent | Profile | Apply |
|---|---|---|---|---|
| `ui.theme`, `ui.density`, `ui.scale`, `ui.reduce_motion` | User | Free | Member | Live |
| `ui.language` (SE-03: switch needs no restart) | User | Confirm | Excluded | Live |
| `quality.tier` (live / high) | User | Free | Member | Live |
| `startup.reopen` (D9) | User | Confirm | Excluded | NextStart |
| `privacy.crash_reports` (SE-04: `Off`, `SaveOnly` by default, `AskToSend`) | User | Never | Excluded | Live |
| `audio.bindings` (role to device, `audio-api.md` §2.2) | Machine | Never | n/a | Live |
| `io.fs_roots` (D11) | Machine | Never | n/a | Live |
| `update.channel` (SE-08) | Machine | Never | n/a | NextStart |
| `mascot.*` | User | Free (opacity, size), Confirm (click-through, on top) | Member | Live |

`privacy.crash_reports` has three values: `Off` captures and writes nothing; `SaveOnly` (the default) writes a sanitised report to a local folder and uploads nothing; `AskToSend` adds a `Send` after a preview of the exact bytes, hidden until the owner's decisions (`privacy.md` §9) land.

**The rule for choosing `AgentWrite`:** any key that touches privacy, network, file roots, credentials, updates or capture is `Confirm` or `Never`, never `Free`. `Free` is for taste only (appearance, quality, density). A `tests/settings.rs` author states the reason in the key's doc comment, and reviewers check it against this rule.

**Alternatives rejected.**
- **A second document mounted into the executor** (`app/…` paths): its patches would enter the project journal and branches, and undo would reach taste.
- **Settings as a plain Rust struct each crate saves itself** (what `omuzaki-ui::ds::Settings` and the device profile do today): no agent peer, no profiles, no migration, one file format per feature.
- **A central `Settings` struct with a field per feature:** every feature edits one file; the registry is the answer to §39 rule 2.

### D5. Storage: TOML, atomic, merge-on-write, unknown keys kept

- **Format.** TOML, nested by key segment (`ui.scale` is `scale` under `[ui]`), edited with `toml_edit` 0.25 (already a dependency, MIT OR Apache-2.0) so comments, order and **unknown keys survive**: a key no build registered (a newer build wrote it, a removed feature) is kept verbatim, never deleted, never shown on the page, listed by `settings.list { unknown: true }` with a "remove" action. A hand-edited comment stays.
- **Reads.** Loaded once at start. For each registered key present: deserialise into `Value`, run `check`. A value that fails is **not** deleted: the file keeps it, the key reads as its default, and a `SettingFinding` ("`ui.scale = 900` is out of range 75 to 200; using 100") goes to the log and the Settings page. A file that does not parse as TOML is renamed `settings.toml.broken-<utc>`, the defaults are used, and the person is told once. Nothing blocks start.
- **Writes.** On a change the store marks the key dirty and a writer thread, 250 ms later (at most every 2 s under a burst, like the journal keeper), takes an **OS file lock** (advisory exclusive lock on a sibling `settings.toml.lock`, `flock`/`LockFileEx`, released on process death), and holds it across **re-read, merge of this process's dirty keys, and atomic replace**, so two processes can never both merge from the same old text and lose one update. The lock file is never the data file and is never deleted. The writer owns the temp file: a hidden `.settings.toml.<pid>.tmp` created under the lock with `create_new`, `sync_all`, `rename` over the target (the pattern of `omuzaki_doc::disk::write_atomic`, which moves into `omuzaki-core`; it gains a variant that takes the lock). A stale temp of a dead process is removed by the next lock holder. Lock wait is bounded (2 s, then keep the keys dirty and report); a lock that cannot be created (read-only dir) is reported, not ignored. Last writer wins per key, not per file. A clean exit flushes. Windows: a rename over a file another process holds open retries three times over 100 ms, then keeps the key dirty and reports.
- **Per-key migrations.** `SettingMeta::version` is the key's current shape. The file has a `[versions]` table that lists only keys whose stored version is above 1 (absent means 1). On load, a key stored at an older version goes through `Setting::migrate(from, old)` once, in memory; the first write after a migration first copies the file to `settings.toml.bak-<app version>` (once per app version). A key stored at a **newer** version than the build knows is kept verbatim and reads as the default (never migrated down, never overwritten by a default). A renamed key lists `RENAMED_FROM`; on load the old key's value is moved to the new key unless the new key exists. Why per key and not one file version like `omuzaki-doc`: features ship and change independently, and a migration for `audio.bindings` must not need to know `ui.scale`.
- **Why no file watcher in v1:** the app is the writer; hand edits are read at the next start. `settings.reload` (person) calls `Settings::reload`, which re-reads both files and applies the difference as one snapshot swap and one `Reloaded` event (none when nothing differs); a file that cannot be read changes nothing.

### D6. Reading is cheap, and consumers react to events

- `Settings::snapshot() -> Arc<Snapshot>`: the effective values (default, base, profile resolved) as an immutable map, swapped as a whole on every change. A read is an `Arc` clone and a lookup, with no allocation and no lock beyond the pointer swap. `settings.get::<K>()` is built on it.
- **Real-time threads never read settings per frame.** A consumer subscribes to `SettingsEvent`, copies the value it needs into its own state when its key changes (the renderer's quality tier, the audio engine's bindings), and uses that copy on the frame path.
- One `SettingsEvent` per change set (a `settings.set` is one; a profile switch is one carrying every key that changed). Each `Change` carries `before`, `after` and the key's `Apply`, so the UI can say which changes wait for a restart.

### D7. Profiles: a named overlay of `User` keys, applied atomically

**What a profile is:** a sparse map of `User` keys whose `InProfile` is `Member`, stored in `profiles/<id>.toml`. The effective value of a key is, in order: the active profile's value, the base value in `settings.toml`, the default. `Machine` keys cannot be in a profile (D3: the profile files roam, devices and paths do not), and `Excluded` keys cannot (privacy, consent, language, startup).

**Where the line with project data is.** A profile never holds anything a project owns, so it can never duplicate a scene or a variant:

| Belongs to | Examples | Not in a profile because |
|---|---|---|
| a **scene** (ADR 0024) | lights, cameras, backdrop, cast | it is document state; "stream" and "record" look the same on the same scene |
| a **variant** (CB-13) | game, close-up outfit and rig overrides | it is a character use, chosen per use |
| a **show** (`shows/<id>.toml`) | outputs, hotkeys, overlays, redeems | it is broadcast setup and travels with the project |
| a **profile** | quality tier, UI density and scale, reduce motion, mascot behaviour, tracker preferences, notification toggles | taste about *how the app behaves while I do this*, the same across projects |

The rule an author applies when choosing `InProfile`: *would I want this to change when I go from streaming to recording, on every project?* If it depends on the project, it is project state; if it depends on the computer, it is `Machine`.

**Writing while a profile is active.** `settings.set` on a `Member` key writes into the **active profile**; with no profile active it writes the base. (`layer: "base"` writes the base explicitly.) Otherwise a tweak made while "Stream" is active would silently vanish at the next switch. The Settings page shows a banner "Saving to profile: Stream". `settings.reset` removes the key from the active profile (it falls back to the base value).

**Switching.** `settings.profile.switch { profile: id | null }` computes the effective values under the new profile, compares them with the current snapshot, swaps the snapshot **once**, and emits **one** `SettingsEvent { cause: ProfileSwitched { from, to }, changes }` listing only the keys whose value changed. Keys with `Apply::NextStart` that changed are returned in the op's output as `pending_restart`. The active profile id is stored in `machine.toml` (profiles roam; which one this computer is in does not). Deleting the active profile switches to none first. A profile file that cannot be read is skipped with a finding; the base is always intact.

**Creating.** `settings.profile.save { name, from?: id | "current" }`: `"current"` (default) captures every `Member` key whose **effective value differs from what the base would give** (base value, else default), so the profile reproduces today's setup exactly. An override equal to the default is kept when the base differs: default 100, base 125, active override 100 saves `scale = 100`, never dropping it. Keys equal to the base are omitted. `from: id` duplicates. Saving over an existing name replaces it only with `overwrite: true`. A profile has a stable `ProfileId` (an ASCII slug plus four random characters, `stream-k3f9`) used for its file name, and a display `name` (any text, 1 to 48 characters, NFC-normalised; Japanese names are normal here), so renames never touch the file name and no name can be a path.

```rust
pub struct ProfileId(String);                       // file name stem; opaque
pub struct ProfileInfo { pub id: ProfileId, pub name: String, pub keys: u32, pub active: bool }
```

```toml
# profiles/stream-k3f9.toml
name = "Stream"
[values.quality]
tier = "high"
[values.ui]
reduce_motion = true
```

### D8. "Share" is cut

The maintainers asked for it to be cut unless a concrete need appears. None did:
- A profile cannot hold `Machine` values (D3), so the part that made sharing hard (devices mapped by name, with a report) does not exist.
- What remains is a few `User` values (quality tier, density, reduce motion). People trade presets for scenes, characters and shows (the library), not for these, and the right quality tier depends on the GPU.
- An import path would need its own validation, a report and an agent rule, for almost no use.

Left free by design: a profile is one small TOML file. Copying it into `profiles/` works, and the loader validates every key the same way it validates `settings.toml` (a non-`Member` or unknown key in a copied file is ignored and reported, never applied). This costs nothing and needs no feature. **Reopen when:** a creator asks to share a profile and it is more than values the library cannot carry. The profile work drops export and import.

### D9. Session restore: remember what was open, never what was running

Session state is **state** (D1), not a setting: the app writes it. It lives in `session.toml` in the local data dir because paths and monitor layouts are machine-bound.

```rust
pub struct SessionState {
    pub last_project: Option<PathBuf>,
    pub recent: Vec<RecentProject>,          // newest first, at most 20
    pub windows: BTreeMap<String, WindowGeometry>,
    pub last_seen_version: Option<String>,   // D10
    pub opening: Option<PathBuf>,            // set before a project opens, cleared after: the crash-loop guard
}
pub struct RecentProject { pub path: PathBuf, pub name: String, pub opened: Timestamp }
pub struct WindowGeometry { pub x: i32, pub y: i32, pub w: u32, pub h: u32, pub maximised: bool, pub monitor: Option<String> }
```

- **Remembered:** the last project; a recent list (File > Open Recent); window position and size per window id (main, mascot, each pop-out) with the monitor's name; the last version seen. The **workspace** stays in `project.toml` (`shell.md` §17: "last workspace"), so it reopens with the project on any computer; panel layouts per workspace stay in the config dir. Nothing is duplicated.
- **Never restored:** anything that starts capture or output. Opening a project does not start the camera, the microphone, a stream, a recording, the virtual camera, an agent session or a paid job (SE-04: no capture the person did not start). The Welcome and Perform screens show the usual "Start" button.
- **Windows.** A saved geometry is used only if it intersects a connected monitor by at least 120 by 80 points; otherwise the window opens centred on the primary monitor at its default size. Size is clamped to the work area.
- **The `opening` marker is durable before the load starts.** `begin_open` writes `session.toml` synchronously (`write_atomic` with fsync, under the same lock as D5), returns the error if it fails, and the caller does not start loading when it fails (it opens the Welcome screen with the reason). Only the other session writes (recent, geometry) are debounced; `opened` clears the marker the same way and returns its error too (the project stays open; the caller reports it, and a stale marker is the worst case). A failed debounced write is logged and retried, and `flush` at exit reports it. A crash between `begin_open` and the debounce interval therefore still leaves the marker.
- **Opt-out.** `startup.reopen` is a User setting, `last` (default) / `ask` (the Welcome screen offers "Reopen <name>") / `welcome` (nothing reopened). `session.recent` and `session.forget { path }` clear entries; "Clear recent" empties the list.
- **What wins when both exist (VC-08).** Recovery always wins over the opt-out, because losing work is worse than an extra click. Order at start, as a pure function `startup_plan(session, settings, recovery_pending: &[PathBuf]) -> StartupPlan`:
  1. A project in `recent` (or `last_project`) whose folder has an unclean-shutdown marker (ADR 0021: a journal with no `close` record) goes to **Recover**: the recovery offer (restore to the last transaction, or open the files as they are) is shown first. With `ask` or `welcome`, the same offer appears on the Welcome screen as a card. Several crashed projects are listed, newest first. After the choice, that project opens and becomes `last_project`.
  2. Else `startup.reopen = last` and `last_project` set: **Open**. Before opening, `opening` is written; it is cleared when the project is open and shown. If `opening` is still set at the next start (the app died while opening it), the plan is **Welcome** with the notice "Last time opening `<name>` stopped unexpectedly. Reopen it, or open its recovery copy." No crash loop: opening is attempted at most once per start.
  3. Else **Welcome**.
- **A project that cannot be reopened** (folder gone, drive not mounted, unreadable, schema newer than this build) never blocks start or disappears silently: the Welcome screen shows a card naming it and the reason ("The folder `D:\shows\spring.omuzaki` is missing. Is the drive connected?") with **Locate...** and **Remove from recent**. The entry stays in `recent` until the person removes it (a network or external drive comes back).
- **Headless.** The CLI and MCP never restore a session: a project is always named. `session.*` ops work everywhere.

### D10. Release notes: reviewed user text, shipped in the binary, shown once

**Two audiences, two files.** `.changes/<branch>.md` fragments are developer notes (one or two lines, written by the PR author, kept in the repo). Release notes are user-facing text, written and reviewed once per release.

**Format** (a folder per version in `crates/omuzaki-release-notes/notes/`, embedded at compile time with `include_dir` 0.7 (MIT; verified on crates.io, last release 2024-06) so it works on native and wasm and needs no network):

```toml
# notes/0.9.0/notes.toml          English is the source; ids are stable and never reused
version = "0.9.0"
date = "2026-11-02"
headline = "A wider cute face, and settings profiles"       # optional
[[item]]
id = "profiles"
kind = "new"                       # new | improved | fixed | changed
area = "settings"                  # the Settings page area / app area; used to group
status = "reviewed"                # draft | reviewed | skip
text = "Save your stream and record setups as profiles and switch between them in one click."
source = ".changes/1283-settings-profile-ops.md"      # dev-only, never shown
```
```toml
# notes/0.9.0/ja.toml             one file per locale: id to text; missing ids fall back to English
[text]
profiles = "..."
```

**Drafting.** `cargo xtask release-notes draft <version>` lists the fragments added since the previous release tag (from git, so fragments need not be deleted) and writes `notes.toml` with one `status = "draft"` item per fragment: the fragment text as a starting point, `kind` guessed from the PR title's prefix, `source` set. A person or an agent rewrites each item for a user ("what can I do now", not "what changed in the crate"), sets `reviewed`, or sets `skip` for internal changes. The release PR is reviewed like any docs PR; a maintainer owns the English. `cargo xtask release-notes check` fails the release if any item is `draft`, if `en` text is empty, or if an id is duplicated, and reports per-locale coverage. It is a release gate, not a PR gate.

**Languages.** English is required and reviewed before release. The other five (ja, ko, zh-Hans, es-419, pt-BR) go through the SE-03 translation pipeline (with its glossary). A translation that is not ready **does not block the release**: that item shows in English, with "Not yet translated" next to it, so a fix for a crash is never held for translators. The report lists missing items per locale, like the l10n report (l10n-ops §9.3). zh-Hant joins with SE-09.

**Shown once after an update.** The running version is compared with `last_seen_version` in `session.toml` by SemVer (`semver` 1.0, MIT OR Apache-2.0):
- no value (first run): record the version, show nothing (the first-run wizard, SE-01, is the welcome);
- running version greater: show a "What's new" view with the notes of every version in `(last_seen, running]`, newest first, at most three, with "Older versions" below; `last_seen_version` is updated when the view is **dismissed**, not when it opens, so a crash while it is up shows it again;
- running version smaller (SE-08 rollback): show nothing and keep the higher value, so the roll-forward does not repeat it;
- it never opens over a live session or a first-run wizard: it waits for the Welcome screen or the next project open, and it is a panel the person can close with Esc, not a blocking dialog.
Beta builds have their own notes (`0.9.0-beta.2`); the stable notes are written as the change since the previous stable release.

**Always reachable.** Help > Release notes opens the same view with a version picker (all shipped versions). Ops: `release_notes.list` and `release_notes.get { version, locale? }` (queries, scope `read`); the UI uses the same ops. MCP and `--json` answer in English; the UI and the CLI's human output follow the reader's locale (ADR 0023). It is independent of how the update arrived (installer, package manager, SE-08 updater).

### D11. `io.fs_roots` is a Machine setting, not saved "with the project" (amends ADR 0013 §3)

ADR 0013 says saved roots "persist with the project". That is wrong on two counts: the paths are machine-bound (`D:\assets` means nothing on the next computer), and a project received from someone else could then carry grants. The registered key `io.fs_roots` is `Machine` scope, `AgentWrite::Never`; the Settings > Files page edits it through `settings.set`. A per-project association ("this project uses these roots") is not needed: roots are the person's, and the executor still gives a session only the roots the person delegates (ADR 0013 §4). ADR 0026 supersedes that sentence; the rest of 0013 stands.

### D12. Backends and the web player

The store sits on a small private trait (`Backend { read(file), replace(file, text) }`) with a file implementation. The web player is a viewer with no project editing and keeps its few choices (language, reduce motion) in browser storage through a second backend, in a later task; the contract does not change. `omuzaki-settings-mock` is an in-memory `Settings` (no files, same layering, same events) for tests of other crates.

## 4. Worked example on disk

A streamer on Windows: scale 125, a "Stream" profile, a beta channel, one asset root, and an unknown key from a newer build.

```toml
# %APPDATA%\Omuzaki\config\settings.toml
format = 1
[ui]
theme = "dark"
scale = 125                    # a base value: used when no profile overrides it
[quality]
tier = "live"
[versions]
"ui.scale" = 2                 # only keys migrated beyond version 1 are listed
[future_feature]               # no build here registers it: kept verbatim
level = 3
```
```toml
# %APPDATA%\Omuzaki\config\profiles\stream-k3f9.toml
name = "Stream"
[values.quality]
tier = "high"
[values.ui]
reduce_motion = true
```
```toml
# %LOCALAPPDATA%\Omuzaki\data\machine.toml
format = 1
active_profile = "stream-k3f9"
[update]
channel = "beta"
[io]
fs_roots = ["D:\\assets"]
[audio.bindings.voice]
fixed = { host = "wasapi", id = "{0.0.1.00000000}.{8b1c...}", label = "Rode NT-USB", channels = "mono" }
```
```toml
# %LOCALAPPDATA%\Omuzaki\data\session.toml
format = 1
last_project = "D:\\shows\\spring.omuzaki"
last_seen_version = "0.8.2"
[[recent]]
path = "D:\\shows\\spring.omuzaki"
name = "Spring show"
opened = 2026-11-02T09:41:00Z
[windows.main]
x = 80
y = 60
w = 1760
h = 960
maximised = false
monitor = "DELL U2720Q"
```

What the effective values are: `quality.tier = "high"` (profile), `ui.reduce_motion = true` (profile), `ui.scale = 125` (base), `ui.theme = "dark"` (base). `settings.profile.switch { profile: null }` returns one event with two changes (`quality.tier` high to live, `ui.reduce_motion` true to false). `settings.set { key: "ui.scale", value: 100 }` while "Stream" is active, `scale` being a `Member`, writes `[values.ui] scale = 100` into the profile and leaves the base 125. On the next start the app is 0.9.0: the plan is **Open** `spring.omuzaki` (no unclean marker), and after it is shown, "What's new" lists 0.9.0 (and 0.8.3 if it was skipped).

## 5. Crates and contract changes

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-settings-api` (new) | API | `SettingScope`, `AgentWrite`, `InProfile`, `Apply`, `SettingMeta`, `Setting`, `SettingRegistration`, `register_setting!`, `SettingError`, `SecretRef`, `ProfileId`, `ProfileInfo`, `Snapshot`, `Resolved`, `Source`, `Change`, `SettingsEvent`, `ChangeCause`, `SettingFinding`, the `Settings` service trait, `SettingsExt` (typed `get` and `set`), `SessionState` and its parts. Depends on `omuzaki-core`, `omuzaki-ops-api` (`Actor`), `omuzaki-doc-api` (`Value`) |
| `omuzaki-settings-mock` (new) | Mock | `MemorySettings`: the service in memory, with the layering and events, no files |
| `omuzaki-settings` (new) | Implementation | `Paths`, the store (layers, snapshot, events, writer thread, migrations), profiles, the session store, `startup_plan`, the `settings.*` and `session.*` ops |
| `omuzaki-release-notes` (new) | Implementation | the embedded notes, the loader and fallbacks, `whats_new(last_seen, running)`, the `release_notes.*` ops |
| `omuzaki-ops-api` | API | `Scope::EditSettings` (`edit:settings`) added to `Scope`, `ALL` and `Grants::agent_default()` (ADR 0026) |
| `omuzaki-core` | API | `fsx::write_atomic` moved from `omuzaki-doc` |
| `xtask` | App | `release-notes draft` and `release-notes check` |

```rust
/// The service. Raw (string key, JSON value) so ops and the UI's generated page need no type per key;
/// `SettingsExt` adds the typed `get::<K>()` and `set::<K>()`.
pub trait Settings: Send + Sync {
    fn snapshot(&self) -> Arc<Snapshot>;
    fn get(&self, key: &str) -> Result<Resolved, SettingError>;
    fn list(&self, filter: &ListFilter) -> Vec<Resolved>;
    /// Every method that can change values, profiles or the log takes the ops layer's `Mode` and the `Actor`.
    /// `Mode::DryRun` runs the same validation and returns the same value as `Apply`, and writes, swaps and emits nothing.
    /// Validates (schema, `check`, `Apply`), then updates the active profile or the base. One event.
    fn set_raw(&self, key: &str, value: Value, layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError>;
    fn reset(&self, key: &str, layer: Layer, mode: Mode, by: &Actor) -> Result<Option<Change>, SettingError>;
    fn profiles(&self) -> Vec<ProfileInfo>;
    fn save_profile(&self, name: &str, from: ProfileSource, overwrite: bool, mode: Mode, by: &Actor) -> Result<ProfileInfo, SettingError>;
    fn rename_profile(&self, id: &ProfileId, name: &str, mode: Mode, by: &Actor) -> Result<ProfileInfo, SettingError>;
    /// Deleting the active profile switches to none in the same step; the report says what that changed.
    fn delete_profile(&self, id: &ProfileId, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;
    /// Atomic: one snapshot swap, one event.
    fn switch_profile(&self, to: Option<&ProfileId>, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;
    /// Re-reads the files; atomic like a switch, with `ChangeCause::Reloaded`.
    fn reload(&self, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;
    fn changes(&self, limit: usize) -> Vec<SettingChange>;
    fn subscribe(&self) -> Subscription<SettingsEvent>;
    fn session(&self) -> &dyn SessionStore;
}

pub struct Resolved { pub key: String, pub value: Value, pub default: Value, pub source: Source, pub meta: SettingMeta }
pub enum Source { Default, Base, Profile(ProfileId) }
pub enum Layer { Auto, Base }
pub struct Change { pub key: String, pub before: Value, pub after: Value, pub apply: Apply }
pub struct SettingsEvent { pub cause: ChangeCause, pub changes: Vec<Change> }
pub enum ChangeCause { Set, Reset, ProfileSwitched { from: Option<ProfileId>, to: Option<ProfileId> }, Reloaded, Migrated }
pub struct SwitchReport { pub changed: u32, pub pending_restart: Vec<String> } // also returned by delete_profile and reload

pub trait SessionStore: Send + Sync {
    fn state(&self) -> SessionState;
    fn begin_open(&self, project: &Path) -> Result<(), SettingError>; // writes and fsyncs `opening` NOW, never debounced
    fn opened(&self, project: &Path, name: &str) -> Result<(), SettingError>; // clears `opening` durably NOW, updates last_project and recent
    // Debounced: cannot fail the caller; a failed write is logged, retried, and reported by `flush`.
    fn forget(&self, project: &Path);
    fn set_window(&self, id: &str, geometry: WindowGeometry);
    fn mark_version_seen(&self, version: &str);
    fn flush(&self) -> Result<(), SettingError>; // writes anything debounced now (at exit)
}
```

**Ops** (all `omuzaki-settings` unless stated; "agent" is the rule of D4):

| Op | Kind | Scope | Agent | Undo |
|---|---|---|---|---|
| `settings.get {key}`, `settings.list {area?, changed?, unknown?}`, `settings.changes {limit?}` | query | read | yes | none |
| `settings.set {key, value, layer?}`, `settings.reset {key, layer?}` | edit | edit:settings | per key (D4) | explicit |
| `settings.reload` | edit | edit:settings | person only | explicit |
| `settings.profile.list` | query | read | yes | none |
| `settings.profile.save {name, from?, overwrite?}`, `.rename {profile, name}`, `.delete {profile}` | edit | edit:settings | confirm | explicit |
| `settings.profile.switch {profile \| null}` | edit | edit:settings | confirm | explicit |
| `session.recent`, `session.get` | query | read | yes | none |
| `session.forget {path}` | edit | edit:settings | person only | explicit |
| `release_notes.list`, `release_notes.get {version, locale?}` (`omuzaki-release-notes`) | query | read | yes | none |
| `release_notes.seen {version}` | edit | edit:settings | person only | explicit |

## 6. The work cut

SE-10 work. This design merges nothing away but **splits two pieces that were too big**, **drops export and import** (D8) and adds the prerequisites the design found. Every item needs this design first.

| Work | Does | Needs |
|---|---|---|
| settings-api and mock | `omuzaki-settings-api` and `omuzaki-settings-mock`: the types and traits of §5, `register_setting!`, the registry, `MemorySettings`, the crate-check test helper | the `Scope::EditSettings` change |
| ops-api scope | `omuzaki-ops-api`: `Scope::EditSettings` (`edit:settings`) in `Scope`, `ALL`, `name`, `Grants::agent_default()`, the spec §6 row | none |
| write_atomic move | move `write_atomic` from `omuzaki-doc` into `omuzaki-core::fsx`, `omuzaki-doc` uses it; no behaviour change | none |
| store | `omuzaki-settings`: `Paths`, the TOML store (`toml_edit`), layers `User` and `Machine`, merge-on-write with the writer thread, unknown keys kept, per-key migrations and renames, findings, snapshot and events, `directories` | the settings API crate, the `write_atomic` move |
| settings ops | `settings.get`, `.list`, `.set`, `.reset`, `.changes`, `.reload` ops with the `AgentWrite` rules and l10n text | the store, the `Scope::EditSettings` change |
| profiles in the store | profiles in the store: files, overlay resolution, write-to-active, atomic switch with one event, `ProfileId`, hand-copied files with findings | the store |
| profile ops | `settings.profile.*` ops: list, save, switch, rename, delete (no export or import) | the profiles in the store, the settings ops |
| session store | the session store (`session.toml`, atomic), `session.recent`, `.get`, `.forget`, the `opening` guard, `last_seen_version` | the store |
| startup plan | `startup_plan` (pure, table-tested: opt-outs, recovery first, missing project, `opening` guard, monitors) and the Welcome-screen cards and restore wiring in `omuzaki-ui` | the session store, the settings ops, document recovery detection and the relaunch prompt, the UI shell |
| release-notes crate | `omuzaki-release-notes`: the format, `include_dir` embedding, loader with fallback, `whats_new`, `release_notes.*` ops, the 0.1.0 seed notes in six languages (placeholders marked) | none |
| release-notes xtask | `cargo xtask release-notes draft` and `check` (D10) | the release-notes crate |
| What's new panel | "What's new" panel after an update, Help > Release notes, version picker | the release-notes crate, the session store, the UI shell |
| Settings window | `omuzaki-ui`: the Settings window generated from the registry (sections by area, search, modified dot, reset, "applies after restart", agent-changed marker, "Saving to profile") | the settings ops, the UI shell, the remaining design-system components |
| first real keys | the first real keys: register `ui.theme`, `ui.density`, `ui.scale`, `ui.reduce_motion`; `omuzaki-ui` reads and writes through the service and saves them (nothing persists today) | the settings ops |
| profile switcher | profile switcher (top bar) and Settings > Profiles page: save, switch, rename, delete; no export or import | the profile ops, the Settings window |

Other work touched: the Settings > Files page (edits `io.fs_roots`, Machine scope, D11), the mascot keys (use the registry), the other settings pages (use the registry), the Storage page (retention keys register), and audio-api's `ProfileStore` stand-in (replaced by `audio.bindings` in the `Settings` service when the audio engine lands).

## 7. Performance and correctness

- No allocation or lock on any real-time path: a read is an `Arc` clone off the frame path (D6); a change costs one snapshot rebuild (hundreds of keys, microseconds) off the frame path.
- Disk: at most one atomic replace per file per 250 ms; a write is a few KB. No settings I/O on the executor's lock except the in-memory update.
- Determinism: the store takes a `Clock` and `Paths`; merge-on-write, migrations and profile switching are pure functions of file text and are property-tested (a switch followed by the reverse switch restores the exact snapshot; any dirty-key write set applied to any file text leaves unrelated keys byte-identical).
- Errors are values everywhere; a corrupt file never blocks start (D5).

## 8. Alternatives rejected (beyond D4 and D8)

- **One registry file for all four kinds of persisted thing:** different lifetimes, owners and roaming; D1's table.
- **`confy`, `config` or `figment`-style layered config crates:** they merge files and environment into a struct, but not our overlay-with-write-to-active, per-key migration, unknown-key preservation, or agent rules; `toml_edit` plus 300 lines of store is smaller than adapting one.
- **Storing profiles inside `settings.toml`:** one corrupt profile would threaten the base, and copying a profile (D8) would need editing a big file.
- **Release notes fetched from the network:** needs a server, fails offline, and SE-04 says no telemetry by default.
- **Release notes in the Fluent catalogs:** the catalogs are for UI strings that exist once; notes are per-release content that arrives after the strings freeze and has its own review.

## 9. Maintainer decisions (2026-10-12)

1. `edit:settings` is in the default agent grants; each key's agent rule is the real gate (the rule in D4).
2. D11 confirmed: `io.fs_roots` is `Machine`, `AgentWrite::Never`; ADR 0026 supersedes that sentence of ADR 0013 §3; a project holds no settings.
3. Release-note languages do not block a release; English is the fallback and the l10n report lists what is missing.
4. Profiles hold `User` keys only; machine keys (devices, paths, roots) stay with the machine.
