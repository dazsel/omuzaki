# 0047: Platform export is a data file and a registration in the op layer, fitted at export time (2026-10-13)

**Status:** proposed. **Context:** EX-03, EX-04, EX-13
to EX-15, `docs/design/platform-export.md` (the full design), `docs/design/io-api.md` (D3 to D5, D10), ADR 0011
(lib crates), ADR 0015 (link), the existing VRChat rank checker (`vrchat_rank.rs`), the fit tools.

## Context

Five features export a character for a platform with limits: VRChat PC and Quest, Roblox, cluster, Resonite and the
engine packages. The existing rank checker is VRChat-only, and the fit tools (resize, atlas, bones, decimate, cleanup)
were filed without a decision on where limits, fixes and platforms live, or how an export is fitted to a limit. The
spec says EX-14 and EX-15 are "built on the same engine", so the engine's shape decides how small they become.

## Decision

1. **Everything stays in `omuzaki-io-ops`** (limits engine, fit tools, the planner, profiles, the `platform.*` ops). No
   lib crate: nothing outside the op layer needs the engine (the fit runs before `run_export`, so exporters only see a
   fitted scene; `omuzaki-stage` is an app and may depend on `omuzaki-io-ops` for `measure.perf`), the fit tools are
   behaviour, and profiles self-register, which ADR 0011 rules out for a lib. No `omuzaki-io-api` change.
2. **A platform is one data file and one registration.** `data/platforms/<id>.toml` holds the published limits
   (`[limits]`: `source`, `retrieved`, `version` required, tiers, the texture estimate, per-stat rows) and our policy
   (`[profile]`: the exporter id, default tier, what the platform strips, texture and atlas sizes, the decimation
   margin). `register_platform_profile!(T)` with `T: PlatformProfile` adds the id, the data and an optional `prepare`
   hook for platform code (Roblox's rig and attachments). One table per id, so Roblox's three tables are
   `roblox-body`, `roblox-accessory` and `roblox-layered` sharing one `prepare`. Ids: `vrchat-pc`, `vrchat-quest`, the
   three Roblox ids, `cluster`, `resonite`; `unity`, `godot`, `unreal`, `vrchat-unity` are reserved for EX-04.
3. **The rank checker becomes the first two tables.** `AvatarStats` moves to `limits/` and gains the counts other platforms cap;
   texture memory becomes a per-table estimate over pixel counts; `vrchat_rank.rs` is a wrapper with its API and
   tests unchanged.
4. **The fit is a fixed pipeline, re-evaluated after each stage:** strip, drop unused, merge meshes, size textures,
   atlas, shrink textures again only if memory is still over, reduce bones, decimate. A stage runs only when a stat it
   affects is over the target. It never changes the humanoid map, springs, constraints, expressions and their binds,
   node count, `meta` or provenance. Floors no stage can lower are checked before any lossy stage, and an unreachable
   target is `IoError::Rejected` naming each item and its fix. Output is deterministic; a time cap fails the job, it
   never shortens a stage.
5. **The fit is export-time only.** A variant (`variants/<name>.toml`) selects parts and materials and may name its
   profile; the fit reduces what is left and is never written back to the project.
6. **Rights are unchanged.** The fitted scene goes through `run_export`, so the licence gate and preflight apply as
   they are. SE-07's mark (ADR 0042 D9) is applied after `fit_to` and before `run_export`, so the fit never resamples a
   marked texture; a file signature, if any, sits after `run_export` and before any package.
7. **Agents:** `platform.list` and `platform.check` are `read` queries, `platform.fit` is a `read` job (a dry run with
   progress and cancel), and `io.export` with a `profile` stays `publish` + `fs`, so it is `needs_person` for an agent.
   Omuzaki writes files and tells the person how to upload; an upload op waits for the platform upload work.

## Consequences

- Adding cluster or Resonite is a researched data file and a six-line registration. Roblox adds its `prepare` code.
- The fit-tool tasks are re-cut (data path, `fit.meshes`, the second texture pass, floors, time cap); one new tool,
  `fit_meshes`, is filed. The rest of the fit tasks stand.
- Limit tables go stale: a release check flags any table older than 90 days; PR CI never depends on the date.
- If a second implementation crate ever needs `evaluate`, `limits/` moves to a lib unchanged.
- **Rejected:**
  - **A lib crate for limits and fit tools now:** no second consumer, and profiles must register.
  - **`PlatformProfile` in `omuzaki-io-api`:** an API and hub-adjacent change for op-layer policy; the only cross-crate
    extension point, the package wrapper, is EX-04's to design.
  - **Storing fitted variants:** lossy, version-dependent output that goes stale and duplicates assets; it is
    reproducible in seconds.
  - **Profiles as code only:** limits and policy a person could check against a page would hide in Rust literals.
  - **An adaptive planner that loops over stages:** harder to make deterministic and to explain in a report, for no
    gain on the fixtures; one fixed order with one allowed repeat covers every case found.
