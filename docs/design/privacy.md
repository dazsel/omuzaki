# Crash reports: what is collected, scrubbed and consented to (SE-04)

**Status:** proposed, awaiting maintainer decisions (§9). **Spec:** v3 §31 (SE-04: "no telemetry by default; opt-in crash reports (Sentry) with no images"), §40; SE-01 ("Telemetry is off").
**Builds on:** `docs/design/settings.md` and ADR 0026 (`privacy.crash_reports`: scope `User`, agent `Never`, profile `Excluded`, default off), `docs/design/journal.md` §8.4 (the relaunch prompt), ADR 0018 (audio never leaves the device), the licence register row for `sentry` and `sentry-minidump`.
**Evidence:** `tools/crash-canary/` (never shipped, sends nothing). Sources are marked **verified** (read first-hand, on the date shown), **secondary** or **unverified**. This is not legal advice (§8).

## 1. Decisions in one screen

1. **A report is a Rust panic report only: message, location, frames (function and module), app, OS.** Nothing else (§3).
2. **No minidump is ever sent.** The measured result (§4): the SDK's dump carries the crashing thread's whole stack, so a camera-frame-sized buffer on the stack reaches the dump 32,867 times; the SDK offers no dump-type or memory option to prevent that. A design that cannot show the canary absent sends no minidump, so native crashes (access violations, aborts) produce no upload. The recovery prompt still appears (the journal's unclean-shutdown mark, ADR 0021 and journal §8.4); under `SaveOnly` and `AskToSend` the crash also leaves a small local marker (§4).
3. **Nothing leaves the computer by default.** Three values (§2): `Off` (nothing captured or written), `SaveOnly` (the default: the sanitised report is saved on this computer and offered for preview and save on relaunch; nothing is uploaded), `AskToSend` (as `SaveOnly`, plus a `Send` after a preview of exactly the bytes; hidden until the maintainers settle §9). Asked in Settings → Privacy and on the relaunch after a Rust panic, never at first run (§5). **This changes the earlier contract sketch** (`Off, Ask, Always`, default `Off`): see §2.
4. **The no-network path comes first:** "Save crash report" writes the same scrubbed report to a file the person can attach to an issue.
5. **Pipeline:** four stages kept apart (capture, local persistence, retention, upload consent; §2), one frozen payload that is previewed and sent, scrubber (PV3), 64 KiB cap, at most one report a minute and five a day, no offline queue, one Sentry project per channel (§6).
6. **Hosting, DPA, region and who holds the DSN are maintainer decisions** (§9, questions 1 to 5).

## 2. What a person is promised

The Welcome card says "Your camera never leaves this computer. Telemetry is off." (shell §2). Every rule below serves that sentence: with the default settings the app opens no connection to Sentry, links no uploader into the real-time path, and **writes no report at all**. Four stages are kept apart, each gated by `privacy.crash_reports`:

| Stage | `Off` | `SaveOnly` (default) | `AskToSend` |
|---|---|---|---|
| 1. Capture: the panic hook builds the sanitised `Report` | no | yes | yes |
| 2. Local persistence: the report is a file in the app's own crash folder, written before the process ends, so it survives to the relaunch | no | yes | yes |
| 3. Retention: bounded and deleted by the rules of §6 | nothing to keep | yes | yes |
| 4. Upload consent | never | never (no `Send` exists) | a preview, then the person's yes, per report; the button stays hidden until the maintainers decide §9 |

Under `Off` the relaunch says only "Omuzaki closed unexpectedly" with the log folder (§5), and the person can switch to `SaveOnly` for the next crash. A stored report holds only the fields of §3 and sits on the person's own disk like the journal; the default writes it with no prompt because it is local, sanitised and never leaves without a yes that does not yet exist.

**Contract change (resolved in review).** The earlier sketch was `enum CrashReports { Off, Ask, Always }`, default `Off`. It is now `enum CrashReports { Off, SaveOnly, AskToSend }`, default `SaveOnly`: `Always` is dropped (no automatic sending), `Ask` becomes `AskToSend`, and the default writes a local file but sends nothing. `docs/design/settings.md` §6 already carries the new row; ADR 0026 carries a dated amendment note. The promise "Telemetry is off" (SE-01) still holds: nothing leaves the device.

## 3. The report: every field, and what never leaves

`CrashReports` and the allowed shape are fixed by §2 and implemented by `omuzaki-crash` (§7):

```rust
pub enum CrashReports { Off, SaveOnly, AskToSend }   // setting privacy.crash_reports, default SaveOnly (changed from the sketch; §2)
// allowed: message, location, frames (function, module), app { version, channel }, os { name, version, arch }
```

### Fields that may leave

| Field | Why | How it is scrubbed |
|---|---|---|
| Panic message | The one-line reason is usually enough to find the bug | **Only registered templates are kept; every other payload is redacted, never truncated and kept.** A message is trusted only when it is a *registered template*: `omuzaki_crash::panic_template!("id", "fixed text", ArgKind, ...)` (self-registering, like ops) records the id, the fixed text and the argument *types*; the report carries the id and the typed arguments that pass the allow-list (counts, enum names), never free values. A bare `&'static str` is **not** trusted: a lifetime proves nothing about origin (a dependency's literal, or runtime text leaked into a static). The standard library's own panics are recognised by exact value-free prefix (`called \`Result::unwrap()\` on an \`Err\` value`, `index out of bounds`, `explicit panic`); anything after the prefix is dropped. Any other payload (formatted `String`, dependency message, unregistered literal) becomes `<redacted>` plus the panic location and frames. The path scrub still runs on the kept parts, as a second line of defence only. So "failed to load Zyx-Sentinel-7" or a pasted key can never reach a report |
| Location (`file:line:col` of the `panic!`) | Pins the site | Only the path inside the workspace (`crates/omuzaki-x/src/y.rs`) is kept; an absolute path (a dependency in the cargo registry, a build machine path) is cut to the crate name and version plus the relative path |
| Frames: function and module | The stack is what makes a report useful | Names only. Dropped per frame: absolute file path, local variable values, instruction address, register values, source context lines. At most 64 frames, innermost first |
| App version and channel | Tells a fixed bug from a live one | From the build (`CARGO_PKG_VERSION`, the update channel); never from the machine |
| OS name, version, architecture | A Windows-only bug looks different from a Linux one | Family and major version only (`Windows 11`, `x86_64`); no build string, no locale, no host name |
| GPU adapter name | **Not collected.** The adapter name is a fingerprint and a panic in the render crate carries its own module name | Re-open only with a justified bug class (a driver-specific panic) and a maintainer decision; then name and driver version, no device id |

### Fields that never leave (the scrubber deletes them even if the SDK adds them)

| Field | Why it is excluded |
|---|---|
| File paths with a user name or home folder, project folder, library roots | Identify the person and their disk layout |
| User name, host name, machine name, IP address | Personal data; the SDK's `send_default_pii` stays `false` and `server_name` is set empty |
| Project, character, scene, library item and model names | Can be a real or fan-art character (some may be private test characters); the journal and op log hold these |
| The op log, journal records, undo stack, document values | Creative work and private text |
| Breadcrumbs with arguments (the SDK's logging and HTTP breadcrumbs) | Arguments carry names and values. Breadcrumbs are turned off: `max_breadcrumbs = 0` |
| Environment variables, command line, current directory | Secrets and paths; the SDK's `contexts` integration is configured to OS only (§6) |
| Images, video frames, thumbnails, textures | Camera frames never leave the device (SE-04) |
| Audio, transcripts, voice samples | Audio never leaves unless a person routes it (ADR 0018) |
| API keys, tokens, URLs with credentials | Connectors keep keys in the OS keychain (CN-05); a URL in a message is replaced by its scheme and host |
| Free-text fields not listed above | The allow-list is closed: a field not in the first table is dropped |

The scrubber is an **allow-list**, not a deny-list: it builds a new report from the allowed fields instead of removing bad ones from the SDK's event. A new SDK field therefore leaves nothing by default. A dependency panic never contributes its text, so there is nothing to truncate: the report keeps its location and frames only. PV3 proves it with sentinel tests (below): a static literal, a dependency-origin message and a formatted runtime string, each carrying a synthetic name and a secret-shaped value.

## 4. Native crashes: the canary measurement

**Question.** A minidump holds memory. In this app the memory can hold a camera frame or microphone samples. Can the SDK write a minidump that provably keeps them out?

**What the SDK is.** `sentry-minidump` 0.49.3 starts a second process (`minidumper-child` 0.5.0 → `minidumper` 0.10.0 → `minidump-writer` 0.12.0, **verified** by reading the crate sources, 2026-10-13). The crashing process hands its crash context to that server, which writes the file. The integration then attaches the file to a Sentry event. The only hook that sees the dump is `before_capture(scope, path)`; the crate's public API (`crashes_dir`, `before_capture`, `on_process`, timeouts) has **no option for the dump type, the memory regions or the stack size**. On Windows `minidumper` calls `dump_crash_context(&ctx, None, ..)`, and `None` means `MinidumpType::Normal` (**verified** from the source). On Linux it uses `MinidumpWriterConfig` with no memory options set. So the options the SDK offers are: send the dump as written, or do not attach it.

**Canary.** `tools/crash-canary` (108 lines) fills 1 MiB (32,768 copies of the 32-byte mark `OMUZAKI-CANARY-NOT-A-CAMERA-FRAME!`) into a buffer standing for a camera frame, aborts the process (SIGABRT, a native crash), lets the real crash reporter write the dump, and counts whole marks in everything the SDK would have sent. A file-writing transport replaces the network: the DSN host (`127.0.0.1:9`) is never contacted.

Run on Linux x86_64, `sentry` 0.49.3, Rust 1.99.0, 2026-10-13 (`cargo run -- <empty dir>` in `tools/crash-canary`):

| Case | Canary placed in | Envelope bytes | Marks found | Minidump attached |
|---|---|---|---|---|
| `heap-keep` | `Vec<u8>` of 1 MiB, alive at the crash | 1,107,392 | **0** | yes |
| `stack-keep` | `[u8; 1 MiB]` on the crashing thread's stack | 1,107,297 | **32,867** | yes |
| `none-keep` | nothing (baseline) | 1,107,295 | 0 | yes |
| `stack-drop` | stack, `before_capture` calls `scope.clear_attachments()` | 1,922 | **0** | **no** |

**Reading.**
- The default dump is about 1.1 MB, almost all of it thread stacks. A frame buffer on the stack of **any** thread in the dump reaches it whole. Heap memory did not (0 marks); the heap result held in this one run and layout, and is not a guarantee: an `Arc<[u8]>` that was recently copied through a stack temporary can appear in the stack part.
- No SDK option removes stack memory. The only way to a dump with no canary is to not attach it (`stack-drop`: 0 marks, 1.9 KB, the envelope holds only the event).
- Rewriting the dump (zeroing the memory-list stream) after `minidumper` writes it is possible in principle, but it means parsing and re-writing a binary format in the crash path, with no SDK help, and a miss leaks a frame. We do not take that on for the value it adds.

**Decision.** No minidump is sent or stored for upload. `MinidumpIntegration` is not used; the canary links it only to measure. A native crash is handled by a tiny local handler (`crash-handler`, MIT or Apache-2.0, cleared with the others in the audit). The recovery prompt does not need it (the journal's unclean-shutdown mark, ADR 0021, already triggers journal §8.4). Under `SaveOnly` and `AskToSend` only, the handler also writes a **marker file** (version, channel, time, the thread's last panic location if any) and exits; at the next start the person may save or, with consent, send "what we know": the marker's fields only, a subset of §3. Under `Off` it writes nothing. A crash with no panic therefore reports version and time, not a stack. That loses the native stack of GPU-driver and FFI crashes; the cost is real and accepted, and the alternative (a stack-only dump with a verified scrub) is revisited in question 6 if the owner wants native stacks.

**Checks that need Windows (for a maintainer with a Windows machine).** (a) Repeat the canary on Windows 11 with `MiniDumpNormal`: stack content in the dump is expected but **unverified**; (b) confirm that `crash-handler` writes the marker from an access violation and a stack overflow; (c) confirm no dump file is left in `crashes_dir` when `MinidumpIntegration` is not used. None can run on the build box (Linux).

## 5. Consent

**Where it is asked.** Not at first run (SE-01). In two places only:
1. **Settings → Privacy → Crash reports**, a three-way choice with the current value shown (`SaveOnly` is selected on a fresh install; `AskToSend` is shown disabled until the maintainer decisions of §9 land). The setting is `privacy.crash_reports`; a person changes it, never an agent (agent rule `Never`, profile `Excluded`: ADR 0026).
2. **The relaunch after a Rust panic** (the dialog / journal §8.4, extended by a section that appears only when a report exists). It is shown once per crash and only after the recovery choice is made, so a person who just lost work is not asked for a favour first.

**The relaunch after a Rust panic.** Shown once per crash, after the recovery choice, so a person who just lost work is not asked for a favour first.

*Value `Off`:* nothing was captured, so the recovery prompt of §8.4 is unchanged and is followed by one line.

> **Omuzaki closed unexpectedly** … *(the recovery choices of §8.4)*
> Crash reports are off, so nothing was recorded. [Open the log folder] · [Turn on saving for next time]

*Value `SaveOnly` (the default) or `AskToSend`, a report exists:*

> **Omuzaki closed unexpectedly** … *(the recovery choices of §8.4)*
> **A crash report is saved on this computer.** It has no images, audio, project names or file paths. Nothing has been sent.
> [Save crash report…] · [Preview] · *(`AskToSend` only: [Send])* · [Delete it]

- **Save crash report…** is first: it copies the stored report to a file the person picks (`omuzaki-crash-<time>.json`), needing no network and no account. They can attach it to an issue.
- **Preview** shows the exact bytes that would be sent: the frozen payload of §6 (the final envelope, as UTF-8 JSON lines, with a byte count), in a scrollable box. Under `AskToSend`, `Send` posts that same buffer; nothing is rebuilt, re-serialised or added between preview and send.
- **Delete it** removes the stored report and hides the section. **Turn off** (in Settings) sets `Off` and deletes every stored report.

**Rules.**
- Default `SaveOnly`: reports are captured and kept on this computer; nothing is sent, and there is no `Send` until the maintainers decide §9. An upgrade and a profile switch cannot move the value to `AskToSend`; only the person can, in Settings.
- The preview is the send: one frozen buffer; the transport receives only that buffer.
- A person on `Off` is shown the relaunch line at most once per version.
- Headless runs (CLI, MCP) never send: they print the path of the stored report and exit (agents cannot consent: ADR 0026).

## 6. Pipeline

| Part | Decision |
|---|---|
| Scrub | PV3's `scrub` builds a new `Report` from the allow-list (§3) at capture time (stage 1). If scrubbing fails, the report is dropped: not stored, not sent. The SDK's `before_send` is not used for scrubbing and is not needed, because the SDK client never sends (next row) |
| Frozen payload | At `prepare` the final envelope is built once from the stored `Report` (the SDK's `Event` and `Envelope` types serve only as the serializer), the size cap is applied to it, and its bytes are frozen as `Prepared { bytes }` and shown in the preview. Upload is our `Transport` posting exactly `bytes` to the channel's envelope endpoint; the SDK client (`sentry::init`) is **never started**, so there is no client-report, session, breadcrumb, integration or `before_send` path that could append anything after consent. If a later decision uses the SDK client instead, the options that must be off are `send_client_reports = false`, `auto_session_tracking = false`, `default_integrations = false`, `max_breadcrumbs = 0`, `send_default_pii = false`, `attach_stacktrace = false`, and a transport factory that wraps the bytes; this design does not rely on that |
| SDK use | Only the data types (`Event`, `Envelope`, `Dsn`) of `sentry-types` and `sentry-core`, to serialise the one envelope in the shape Sentry accepts. Features: `default-features = false`; no `reqwest`, no `debug-images`, no `contexts`, no `panic` integration (our own hook captures) |
| Size cap | 64 KiB for the whole envelope (the PV3 default); a report over the cap drops frames from the outermost end until it fits |
| Rate limit | At most 1 report per minute and 5 per day per install, counted in `settings`-adjacent local state (a small file, not the project). Over the limit the report is dropped with a reason and still saved locally |
| Offline queue | **None.** A report that cannot be sent now is not retried and not kept for upload; the saved file remains for the person. A queue is a second copy of personal-adjacent data and a way to send later than the person agreed |
| Retention | **Local (stage 3, only under `SaveOnly` or `AskToSend`):** stored reports are kept at most 30 days and at most 5 files; a report is deleted when it was sent, when the person chooses "Delete it", and by "Delete stored reports" in Settings → Privacy; switching to `Off` deletes them all. A file the person saved elsewhere is theirs. **Sentry:** project retention set to the shortest the plan allows (question 3) |
| Projects | One Sentry project per channel (`stable`, `beta`, `nightly`), each with its own DSN; a release build embeds only its own channel's DSN |
| Transport | Behind our own trait (`omuzaki-crash::Transport`) so every test sends nothing; the real transport is an HTTPS POST of the frozen bytes (the SDK's rustls stack; no OpenSSL) |
| Release tooling | **`sentry-cli` is out** (2026-10-13): it is FSL-1.1-MIT, a restricted source-available licence of the BUSL family that the policy bans in the pipeline as well as the product. We upload no debug files or symbols (they would reveal paths and build ids); if symbol upload is ever wanted it goes through Sentry's HTTP API, and gets its own design |

Licences relevant here (**verified**, read on GitHub 2026-10-13): `sentry-rust` is MIT; the Sentry server (`getsentry/sentry`) is FSL-1.1-Apache-2.0; `sentry-cli` is FSL-1.1-MIT (banned, above). The licence audit (B4) agrees on the SDK.

## 7. PV3 re-cut

PV3 depends on the settings work (`docs/design/settings.md`) and on this design. Changes:

- **Crate and fence:** `omuzaki-crash` as written (`Cargo.toml`, `src/lib.rs`, `scrub.rs`, `gate.rs`, `setting.rs`, `tests/scrub.rs`, `tests/gate.rs`, one new `src/marker.rs` for the native-crash marker), plus the same root `Cargo.toml`, register row and `crates/README.md` row. The manifest declares `net = "crash reports, opt-in"`.
- **Report type:** `pub struct Report { message: String, location: Location, frames: Vec<Frame>, app: AppInfo, os: OsInfo }` with `Location { file: String, line: u32 }`, `Frame { function: String, module: String }`, `AppInfo { version: String, channel: Channel }`, `OsInfo { name: String, version: String, arch: String }`: **our own type, not the SDK's event.** `scrub(raw: RawReport, home: &HomeDirs) -> Report` takes what the panic hook captured and returns the allow-listed copy; an SDK event and envelope are built from `Report` only inside `Reporter::prepare`, once, and frozen as bytes.
- **Drop from PV3:** attachment handling (there are none); the "drop an attachment whose bytes start with image magic" clause is replaced by "every payload that is not a registered template is redacted" (§3); "cut free text to 512 characters" applies only to the typed arguments of a registered template.
- **Add to PV3:** `Reporter::save(&self, report: &Report, path: &Path) -> Result<(), SaveError>` (the no-network path); `Reporter::store` (a no-op under `Off`) and the crash folder with the retention of §6 (30 days, 5 files, delete on send, on "Delete it" and on switching to `Off`); `Reporter::prepare(&Report) -> Prepared { bytes }` that applies the 64 KiB cap to the final envelope, and `send(&Prepared)` posting exactly those bytes; `panic_template!` (self-registering; the registry of trusted templates) and the redaction of every other payload (§3); the rate limit as a pure `RateLimiter` over a `Clock` (`FakeClock` in tests) with the 1 per minute and 5 per day numbers; a marker writer and reader (`NativeCrashMarker { version, channel, time, last_panic: Option<Location> }`).
- **Acceptance additions:** the proptest sentinels; a test that `AskToSend` over the rate limit drops the send with `Reason::RateLimited` and still keeps the stored report; a test that `SaveOnly` has no code path to the transport (the fake transport is never called); a test that the bytes the fake transport receives equal the previewed bytes, byte for byte, and that no later item (client report, session) can be added; sentinel tests with synthetic values only (`Zyx-Sentinel-7` as a name, `sk-test-0000` as a secret-shaped value), each placed in (i) an unregistered `&'static str` panic, (ii) a message from a dependency crate, (iii) a formatted runtime `String`, and (iv) a registered template's argument: none may appear in the stored, previewed or sent bytes, and (iv) is rejected at the allow-list; a test that `Off` writes no file and sends nothing, and that switching `SaveOnly` → `Off` deletes the stored reports; a test that no `sentry` event field outside §3 is ever set (assert on the built event); `cargo deny` with `default-features = false` and only the features of §6.
- **Not in PV3:** the relaunch section and the Settings page (UI work for the relaunch dialog and the settings page); the Windows checks of §4.

## 8. Not legal advice

This design states what the licence texts say; it does not interpret them. The licence and data-protection points (Sentry SaaS against the policy's ban on restricted source-available licences, self-hosting, the DPA, retention) are decisions for legal review and are listed in §9.

## 9. Open decisions (numbered; this design does not answer them)

1. **Hosting:** Sentry SaaS, self-hosted Sentry (FSL server run as a service, which the policy as amended in this PR bans unless the maintainers rule otherwise), or no hosted collector (save-to-file only, people attach reports to issues). The design works for all three; with the last, the pipeline of §6 is only the saved file.
2. **DPA:** is a signed DPA with Sentry required before any `Send` ships, and who signs? Until it is signed, the `Send` buttons stay hidden and only "Save crash report" ships. (**Unverified**: what Sentry's standard DPA covers; needs a maintainer or legal review to read it.)
3. **Data region and retention:** EU, US or other; the retention period (default 30 days is **unverified**; we ask for the shortest).
4. **Who holds the DSN and the Sentry account:** a maintainer personally, a company account, or a service account; where the per-channel DSNs live in CI secrets (a DSN is not a secret key but is abusable for spam).
5. **Rate-limit numbers:** 1 per minute and 5 per day are the PV3 defaults; confirm or change.
6. **Native stacks:** accept losing the stack of native crashes (this design), or fund a follow-up that proves a stack-only dump scrub with the canary on Windows and Linux before any dump is sent.
7. **GPU adapter name:** stay out (this design), or allow it for render-crate panics, with a justification.
8. **The default `SaveOnly`:** it writes a sanitised local file on every Rust panic without a prompt (nothing is sent). Confirm it, or choose `Off` as the default, in which case the first crash after install leaves nothing to attach. `SaveOnly` is the working default until then; ADR 0026 and `docs/design/settings.md` §6 already carry it (§2).

## 10. Verification status

| Claim | Status |
|---|---|
| `sentry-minidump` 0.49.3 has no dump-type, memory or stack option; only `before_capture` touches the dump | **verified**: crate source, read 2026-10-13 |
| Linux dump holds the crashing thread's stack, not the 1 MiB heap buffer | **verified** by the canary (one run, Linux x86_64) |
| Windows default is `MinidumpType::Normal`; stack content there | default **verified** in `minidump-writer` 0.12.0; stack content **unverified** (Windows) |
| `sentry-cli` FSL-1.1-MIT; `sentry` server FSL-1.1-Apache-2.0; `sentry-rust` MIT | **verified**: LICENSE files on GitHub master, 2026-10-13 |
| Sentry DPA terms, regions, retention, quota and rate behaviour | **unverified** (needs an account or legal review; needs a maintainer decision) |
