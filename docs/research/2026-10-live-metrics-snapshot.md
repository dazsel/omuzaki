# The live-metrics snapshot: fields, owner, publishing

**Status:** research with a short design, for review. **Feeds:** `omuzaki://metrics/live` (`docs/design/mcp-resources.md` §2), the UI health meter (PF-26, the Health row). **Sources read:** files on main when this was written, plus the pending perform-status, latency-stats and recorder work.

## Summary and recommendation

- **What it is:** one small plain-data record, `LiveMetrics`, of how the studio is doing right now: fps, frame time (smoothed and worst in the last second), tracking-to-frame and audio latency, late frames, audio underruns, and budget use per system against spec v3 §10. Numbers and states only; no names, no device ids.
- **Owner:** a new API crate, **`omuzaki-metrics-api`** (deps: `omuzaki-core`, `serde`, `schemars`; no `unsafe`). It does not depend on `omuzaki-render-api` or `omuzaki-audio-api`, so the UI and MCP pull in neither. Each system writes its own *section*; nobody else's type is copied.
- **Publishing:** a **seqlock over `AtomicU64` words** (safe Rust). The writer takes no lock and does not allocate; any thread reads. Estimate: about 25 ns per frame warm, 300 ns worst case cold, against 16.7 ms (0.0018 %). The benchmark is `metrics_publish`.
- **`MetricsSource` does not move.** It stays in `omuzaki-mcp` as the one-method JSON adapter over the typed `MetricsReader`. The UI is a second consumer of the *data*, not of that seam, and it reads the typed reader directly.
- **The App composes it:** the `omuzaki` binary (`omuzaki-cli`) makes one hub, hands each writer to its owner thread, and the reader to MCP and the UI. A headless `omuzaki mcp` serves `{"running": false}`.
- **V0 is untouched.** V0's L2 meter (fps, latency, lost) reads `perform.status` and `Tracker::latency()`. Every task below is post-V0.
- **Two corrections to the original brief:** `TH-10` meters *spend* (time, tokens, connector credits), not frame budgets, so it is **not** a consumer (Q2). And `MetricsSource` is not yet in `omuzaki-mcp` on main (no hit for it); it is the seam the provider task will add.
- **Open questions** (each with a recommendation) are in §3, §4 and §5; the task list is §6 (eleven tasks, three of them on the critical chain).

## 1. What exists

| Source | Where | What it holds | Gap for a snapshot |
|---|---|---|---|
| `FrameReport` | `omuzaki-render-api/src/pass.rs:111` | `frame`, `render_size`, `cpu: PassTimes`, `gpu: Option<GpuTimings>` (late, names an earlier frame), `draw_calls`, `triangles`. `Copy`, no heap. | No fps, no wall-clock interval, no dropped count. `PassTimes::total()` sums the passes that ran. |
| Budgets | `omuzaki-render-api/src/quality.rs:15` (`budget::LIVE_GPU` 1.5 ms, `FACE_PAINT_GPU` 0.5 ms) | Two of the §10 rows as constants. | Other rows (`track.face`, `physics.step`, `voice.fx`, `latency.*`) have no constant in a runtime crate. |
| `Scheduler` | `omuzaki-budget/src/scheduler.rs` | `smoothed() -> Option<Seconds>` (EMA, `EMA_ALPHA` 0.1, GPU time without `PaintTextures`, else CPU), `rungs()`, `quality()`, `face_pressure()`, `observe() -> Option<Change>`. | The stage owns one; the snapshot reads its rung, change count and `face_pressure()`. **Not its `smoothed()` for `gpu_ms`:** that falls back to CPU time whenever `FrameReport.gpu` is `None`, including a pending readback on a GPU-capable backend, so it would label CPU time as GPU. |
| `AudioStatus` | `omuzaki-audio-api/src/report.rs:150` | `bridges: Vec<BridgeStatus>` (`buffering`, `drift_ppm`, `underruns`, `overruns`), `tap_drops: Vec<TapDrops>`, `model_blocks_missed`, `devices`. | Heap `Vec`s with names: fine on a control thread, not for a cell. Needs a condensed POD. |
| `RouteLatency` | `report.rs:37` | `declared_total()` and `measured: Option<Seconds>`; `BusLatency` for the `voice.fx` row. | Needs "the heard route" picked once. |
| Frame loop modules | `omuzaki-stage/src/frame/` (`mod.rs` is the §8 order; no driver yet) | Pose, rig, runtimes, scene, face. | The loop driver is not built yet. It is where fps and intervals get measured. |
| `PerformStatus` | Pending work: `{tracking, calibration, retarget, source_fps}`, published with `try_lock` | The ops answer for `perform.status`. | **Overlap, built on:** the snapshot copies only its POD numbers (`source_fps`, `tracking`, `calibration`). `PerformStatus` and its ops stay as they are. |
| `LatencyStats` | Pending work: `{frames, dropped, p50, p95, max}`, capture to publish on the app clock; the track mailbox is a lock-free SPSC triple buffer; `Tracker::latency()` is a cold control-thread call | The tracker's pipeline delay and drops. | **Overlap, built on:** the tracker section copies `Tracker::latency()`, sampled on the tracker's control thread, into the cell; no twin type. |
| `Recorder` / `span()` | Pending work, `omuzaki-test/src/budget/recorder.rs`; moving it to a runtime crate is separate work | Per-system times by §10 key, with nearest-rank percentiles, for the nightly harness. | Keeps every sample in a `Vec` for percentiles: right for a gate, wrong for a live record. **Built on:** the key names and the table parser. The live record uses an EMA and a per-100 ms maximum instead. |
| `measure.perf` | Pending work | Per-system p50/p95 against the budget table, for the nightly run. | Offline, N frames. Same keys; different job. |
| `Performer` | Pending work, `omuzaki-stage/src/perform/performer.rs`: fixed 60 Hz step, `Tracking`, `skipped_steps()` | A count of steps dropped when the loop fell behind (`MAX_STEPS_PER_ADVANCE`). | **Built on:** `skipped_steps()` is the perform section's dropped-step field. |

Nothing pending adds a status, metrics or health type that this would sit beside; the overlaps are the rows above.

## 2. The snapshot

Four sections (frame, perform, tracker, audio), each with one writer thread, one cell and one `at: Seconds` (app clock, when it was published). The **budget** block is not stored: the reader derives it from the sections and the constants, so it costs the writer nothing.

**Rule for every number:** it is a *live* value (a moving average, the last value, or the worst in a 1 s window), for a person or an agent looking now. It is **not** the §10 gate statistic (p95 over 5 minutes, on a named tier); the gates stay in the harness. Every budget row is printed with the word `live`.

| Field | Unit | Source | Changes | Writer |
|---|---|---|---|---|
| **frame section** | | | | |
| `fps` | frames/s, f32 | count of `render()` returns in the last 1 s (ten 100 ms buckets). New: `FrameReport` has none. | every frame, published 10 Hz | stage frame loop (M2) |
| `frame_ms` (smoothed) | ms, f32 | EMA, α 0.1 (the Scheduler's `EMA_ALPHA`), of the wall interval between frame submits | as above | M2 |
| `frame_ms_worst_1s` | ms, f32 | max interval over the ten buckets | as above | M2 |
| `cpu_ms` | ms, f32 | `FrameReport.cpu.total()`, EMA | as above | M2; exists |
| `gpu_ms` | ms, `Option` | The frame meter's **own GPU-only EMA** (α 0.1) of `GpuTimings.passes.total()` minus `PaintTextures`, updated only from a `FrameReport.gpu` that is `Some` and names a frame newer than the last one used. A frame with `gpu: None` (no backend support, or a readback still pending) changes nothing. `None` until the first valid timing, and always `None` when `Capabilities::gpu_timings` is false. | as above | M2; exists |
| `face_paint_ms` | ms, `Option` | `gpu.passes.get(Pass::PaintTextures)`, repainted frames only | on repaint | M2; exists |
| `draw_calls`, `triangles`, `render_size` | count, count, px | `FrameReport` | every frame | M2; exists |
| `late_frames` | count since start | intervals over 1.5 x the target interval (new rule, one constant) | on a miss | M2 |
| `quality_rung`, `quality_changes`, `face_pressure()` | rung, count, enum | `Scheduler::rungs()`, `Change` count, `face_pressure()` | rare | M2; exists |
| `output_dropped` | count | **Not measurable yet.** Frames the output did not take: output window, virtual camera (BR-02). The row reads `null` until one of them reports. It is the `floor.dropped_frames` row's source (§10). | n/a | later |
| `track_to_submit_ms` | ms | The capture time of the frame in use (`Performer::applied_capture()`, app clock, an accessor M2 adds to `Performer`: the capture time of the newest frame applied in **any** step of the last `advance`, not only the last step, whose `PerformOutput.frame` is `None` when that step received nothing) to the instant the stage **finished submitting** the frame that shows it. Measured after submission, so waiting for the frame slot and rendering are inside it. New. **Camera to photon is not software-measurable**: the display half is the QG-02 hardware loop (V0 exit 2), so the field is named for what it measures. | every frame | M2 |
| **perform section** | | | | |
| `source_fps` | frames/s | `PerformStatus.source_fps` | every advance | perform thread (M3) |
| `tracking`, `calibration` | enums | `PerformStatus` | on change | M3 |
| `skipped_steps()` | count | `Performer::skipped_steps()`: perform steps dropped when the loop fell behind | on a stall | M3 |
| **tracker section** | | | | |
| `track_ms` p50, p95, max | ms | `LatencyStats`: capture to publish. `Tracker::latency()` is a cold control-thread call (no allocation or lock guarantee), so it is sampled **on the control thread that owns the tracker** at 10 Hz and the `Copy` result is copied into the cell; the perform and frame loops never call it. | 10 Hz | tracker's control thread (M11) |
| `tracker_dropped`, `tracker_frames` | counts | `LatencyStats.dropped`, `.frames` | 10 Hz | M11 |
| **audio section** | | | | |
| `heard_latency_ms` (declared) | ms | `RouteLatency::declared_total()` of the heard route | on graph change | audio control thread, 4 Hz (M4); exists |
| `heard_latency_ms` (measured) | ms, `Option` | `RouteLatency.measured` | after a loopback | M4; exists |
| `voice_fx_ms` | ms | `BusLatency` of the `mic` bus plus one block of the **actual** `AudioStatus.block_frames` (the contract allows variable blocks; at 1,024 frames the block is 21.3 ms, not 5.3 ms) | on graph change | M4; exists |
| `underruns`, `overruns` | counts | sum over `BridgeStatus.underruns`, `.overruns` | on an event | M4; exists |
| `tap_drops`, `model_blocks_missed` | counts | sum of `TapDrops.dropped`; `AudioStatus.model_blocks_missed` | on an event | M4; exists |
| **process section (later)** | | | | |
| `cpu_share`, `ram_mb` | %, MB | a runtime process probe. The Windows probes live in `omuzaki-test`, an app crate; moving them is part of M8. | 1 Hz | M8 |
| `system_ms{key}` | ms | per-system spans (`physics.step`, pose, rig, ...) | every frame | M9 |

**Budget block (derived at read time).** Each row is `{value, budget, ratio}` with `ratio = value / budget` and a `live` flag.

| Key (spec §10) | Budget | Value | Status |
|---|---|---|---|
| `gpu.frame` | 1.5 ms | `gpu_ms` | exists (`budget::LIVE_GPU`) |
| `gpu.face_paint` | 0.5 ms | `face_paint_ms` | exists (`budget::FACE_PAINT_GPU`) |
| `track.face` | 5 ms | `track_ms.p95` (a slight over-estimate: it includes capture and convert) | after M11 |
| `voice.fx` | 10 ms | `voice_fx_ms` | exists |
| `latency.laptop` | 80 ms | `track_to_submit_ms` plus the camera's exposure and the display half, which are missing: the row says `partial` | after M2 |
| `physics.step` | 0.5 ms | `system_ms["physics.step"]` | M9 |
| `cpu.total`, `ram.runtime` | 3 %, 400 MB | process section | M8 |

The constants for the rows that have none go in `omuzaki_metrics_api::budget`. A test in `omuzaki-test` (task M10, using the table parser) asserts each equals its spec row, so the numbers are never copied by hand.

**Privacy.** The record holds numbers and enum states only. It leaves out device names, tap consumer names and any path. It is read under the `read()` scope, like every resource.

## 3. Where the type lives

**Recommendation: a new API crate, `omuzaki-metrics-api`.** Alternatives, rejected:
- `omuzaki-core`: it "stays small and changes rarely" (its own docs), and this record will grow a section per subsystem.
- `omuzaki-render-api` or `omuzaki-audio-api`: the UI and MCP would depend on a renderer or an audio contract to read a number.
- `omuzaki-budget`: an implementation crate (it depends on `render-api`); an API consumer may not depend on it.

```rust
// omuzaki-metrics-api/src/lib.rs (signatures only)
/// One consistent reading. A section is None until its writer has published once, or when a read lost every retry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LiveMetrics {
    pub frame: Option<FrameMetrics>,     // each section: plain Copy fields + `at: Seconds`
    pub perform: Option<PerformMetrics>,
    pub tracker: Option<TrackerMetrics>,
    pub audio: Option<AudioMetrics>,
    pub budget: Vec<BudgetUse>,          // derived by the reader; {key, value, budget, ratio, partial}
}
/// Makes the hub: one writer per section (not Clone, so one writer each) and a Clone reader.
pub fn metrics() -> (MetricsWriters, MetricsReader);
pub struct MetricsWriters { pub frame: FrameWriter, pub perform: PerformWriter, pub tracker: TrackerWriter, pub audio: AudioWriter }
impl FrameWriter { pub fn publish(&mut self, m: &FrameMetrics); }  // &mut self: single writer by type; no lock, no alloc
impl MetricsReader { pub fn read(&self) -> LiveMetrics; }          // infallible; allocates (the Vec), never the writer
pub mod budget { pub const TRACK_FACE: Seconds = Seconds(0.005); /* ... one per row without a runtime constant */ }
```

The cell inside (private; see §4) is `SeqCell<const N: usize>`: `store(&self, &[u64; N])` for the one writer, `load(&self) -> Option<[u64; N]>` for readers. Each section type implements `to_words`/`from_words` (`f32` and `f64` through `to_bits`), with a round-trip property test.

**Does `MetricsSource` move? No.** `omuzaki-mcp` will own `trait MetricsSource { fn snapshot(&self) -> Result<Map<String, Value>, OpError>; }` (a project decision: the seam stays in `omuzaki-mcp` until a second consumer needs it). It stays as the adapter over the typed reader:

```rust
struct ReaderSource(MetricsReader);          // in omuzaki-mcp, resources/metrics.rs
impl MetricsSource for ReaderSource { fn snapshot(&self) -> Result<Map<String, Value>, OpError> { /* to_value(self.0.read()) */ } }
```

What the UI changes: it is the second consumer of the **data**, and the data is what moved out of `omuzaki-mcp`: `MetricsReader` and `LiveMetrics` are in the API crate. The *trait* has not got a second consumer: it returns a JSON `Map` and an `OpError`, both MCP concepts, and the meter wants typed fields and no error. So the rule stays as written and is not triggered; it is just that the shared thing is a concrete type and not a trait. Tests that want a fake use `metrics()` and publish into a writer (cheap, no trait).

**Q1 (for the maintainers):** new crate, or a module in `omuzaki-core`? **Recommend the new crate.** One more small `*-api` crate is the cost; it keeps the dependency graph honest.

**Q2 (for the maintainers):** should TH-10 read this? **Recommend no.** Its notes are "time, tokens and connector credits spent, with stop limits"; that is the Build Theatre's *spend* and a different record. If it later wants a latest-value cell, it reuses `SeqCell` (move the cell to its own crate then, not now). The TH-10 design should note this.

## 4. How it is published

**Mechanism: a seqlock built from safe atomics** (Boehm, "Can Seqlocks Get Along with Programming Language Memory Models?", 2012). Per section: `seq: AtomicU64` and `words: [AtomicU64; N]`.
- *Writer* (one thread): `seq.store(s + 1, Relaxed)` (odd = writing); `fence(Release)`; N `words[i].store(w, Relaxed)`; `seq.store(s + 2, Release)`.
- *Reader* (any thread, any number): `s1 = seq.load(Acquire)`; if odd, retry; N `Relaxed` loads; `fence(Acquire)`; `s2 = seq.load(Relaxed)`; if `s1 != s2`, retry. At most 16 tries, then that section reads as `None` (stale). A reader can never block the writer, and the writer never waits.

Why not the alternatives:
- *Mutex or RwLock:* a lock on the real-time path; a reader holding it stalls the frame (spec §10). The perform status's `try_lock` publish is right for its ops answer but would skip the newest value under contention, which a latest-value cell must not.
- *The tracker's mailbox:* a triple buffer with `UnsafeCell`s. SPSC, `&mut` take, and a value is taken once: one reader, no re-reads. The snapshot has several readers (MCP, UI, harness) who each want the current value. It also needs `unsafe` and a loom model.
- *`ArcSwap` or a boxed `AtomicPtr`:* a new dependency, or an allocation per publish.
- *One `AtomicU64` per field:* free, but `fps` and `frame_ms` could come from different frames. The seqlock gives one consistent record for about the same cost.

**Consistency, `unsafe`, and a check.** The workspace denies `unsafe_code`; this needs none. A `loom` model (as the tracker mailbox does) runs one publish against two reads and asserts "never torn, never backwards".

**Cost per frame (estimate; nothing is measured yet, so it is printed as one).** Assume 3 GHz and one store per cycle.
- *Accumulate*, every frame (`FrameMeter::observe(&FrameReport, now)`): `PassTimes::total()` twice (cpu, gpu) is 2 x 9 slots x about 2 cycles = 36 cycles = 12 ns; the EMA, the bucket max and the counters are about 15 cycles = 5 ns. **About 17 ns.**
- *Publish*, when a 100 ms bucket closes (every 6th frame at 60 fps): about 20 words of `to_bits` (20 cycles), 20 relaxed stores (20 cycles), two `seq` stores and a release fence (free on x86; `dmb ishst`, about 10 cycles, on ARM64). About 60 cycles = 20 ns, so **about 3 ns per frame** amortised.
- *Warm total:* 17 + 3 = **about 20 to 25 ns per frame.**
- *Cold worst case:* after about 10 ms of rendering, assume the meter's state (2 lines) and the cell (4 lines) come from DRAM at 80 ns: 2 x 80 ns every frame, plus 4 x 80 / 6 = 53 ns amortised, plus the warm 25 ns = **about 240 ns, call it 300 ns.**
- *Against the budget:* 300 ns / 16.67 ms = 1.8 x 10^-5 = **0.0018 % of a 60 fps frame**, or 18 us/s of the 240 ms/s that `cpu.total` (3 % of 8 cores) allows: 0.0075 % of that line.
- *Readers:* 20 loads plus a check, about 50 ns; the JSON form about 3 us. At most about 20 reads/s in all (MCP plus UI) pull the cell's 4 lines to another core, so the writer's next publish takes 4 x 70 ns = 0.28 us once per read: about 6 us/s. Nothing the frame loop can feel.
- *Memory:* (21 + 8 + 8 + 8) words x 8 B + 4 `seq` words = 392 B for the whole hub.

**Allocation.** The writer path allocates nothing: the cell is built in `metrics()`, `FrameMeter` holds fixed arrays (ten buckets), and publish takes `&FrameMetrics` by reference. A counting-allocator test over 3,600 frames (the pattern of the tracker's `tests/no_alloc.rs`) asserts zero. Only `read()` allocates (the `budget` `Vec`), on the reader's thread.

**The benchmark the tasks add:** `crates/omuzaki-metrics-api/benches/metrics_publish.rs` (criterion, as the tracker's `hot_path.rs`) with `frame_publish` (target: at most 100 ns, 4x the warm estimate), `read_uncontended`, and `read_while_writing` (a writer at 1 kHz, 16 reads in the budget). M2 adds `frame_meter_observe` (target at most 150 ns). Measured numbers replace the estimates above in the implementation, per §10's rule that unmeasured numbers are printed as estimates.

**Q3 (for the maintainers):** publish every frame, or at 10 Hz? **Recommend 10 Hz of app time** (a bucket close). The meter and an agent look a few times a second; per-frame publish would cost about 6x the cache traffic for no reader. The accumulation stays per frame, so the 1 s worst case is exact.

**Q4 (for the maintainers):** history for a sparkline? **Recommend none in the cell.** The UI samples at 10 Hz into its own ring (600 samples = 2.4 KB for a minute); an agent that wants a trend polls, or runs `measure.perf`. This keeps the record at 392 B.

## 5. Who composes it in the App

**The `omuzaki` binary (`omuzaki-cli`, which already serves `omuzaki mcp`).** It is the only place that sees every subsystem, and implementation crates must not depend on one another, so no one of them can compose the others' sections. At startup:
1. `let (w, reader) = metrics();`
2. `w.frame` goes to the stage frame loop (`FrameMeter` in `omuzaki-stage`, owned beside the `Scheduler`); `w.perform` to the perform thread's `PerformControl`; `w.tracker` to the control thread that owns the `Tracker` (a 10 Hz tick that calls `Tracker::latency()`); `w.audio` to the audio control thread's 4 Hz tick (`AudioMeter` in `omuzaki-audio`, which condenses `AudioEngine::status()` and the latency report).
3. `reader.clone()` goes to `McpOptions::with_provider(MetricsProvider::new(ReaderSource(reader)))` and to the UI's health panel.

Each writer is a separate move, so a missing subsystem leaves its section `None`, never wrong. The renderer, the tracker and the audio engine know nothing about metrics; their owners call `publish`.

**Headless `omuzaki mcp`** (no studio, no frame loop). It registers the same provider over a hub nobody writes to, so `omuzaki://metrics/live` answers `{"running": false, "note": "no studio is running; start one to see live numbers"}` with `read()` scope as usual. An agent gets a clear answer; "resource not found" would read as a bug or a typo.

**Q5 (for the maintainers):** serve the `running: false` answer, or leave the resource out of `resources/list` when headless? **Recommend serving it** and listing it. It costs one branch in the provider and spares agents a wrong turn. (The resource's MIME is `application/json`, set by the provider task, as the URI table says.)

## 6. Tasks it unlocks

All are post-V0: none is on V0's critical path, and none edits a file V0's tasks (T4) own, except M3 and M7, which wait for them. All belong to PF-26 unless noted, at priority P1. Fences are exact.

| # | Task | Fence | Contract | Blocked by | Size |
|---|---|---|---|---|---|
| M1 | `omuzaki-metrics-api`: the cell, the sections, `LiveMetrics`, `metrics()` | new `crates/omuzaki-metrics-api/**`; one row in `crates/README.md`; `docs/licensing/register.md` unchanged (no new dependency) | §3 signatures. `SeqCell`; four sections with `to_words`/`from_words`; `budget` constants; the `LiveMetrics` JSON schema. Tests: round trip, bounded retry, `None` before first publish, loom model, 0 allocations over 3,600 publishes. Bench `metrics_publish`. | none | ~500 lines |
| M2 | `FrameMeter` in the stage | `crates/omuzaki-stage/src/frame/meter.rs` (+ one `mod` line), `tests/frame_meter.rs`; one getter in `crates/omuzaki-stage/src/perform/performer.rs` (after the perform work merges) | `observe(&FrameReport, now)` fills fps, EMA, worst-1s, late frames, `Scheduler` fields; `submitted(capture: Seconds, now)` computes `track_to_submit_ms` **after** the stage's submit call; `flush(&mut FrameWriter)` at each 100 ms bucket. `applied_capture()` is a one-line getter on `Performer`'s kept `last_frame`. Tests with `omuzaki-render-mock` `set_pass_cost` and a fake `Clock`: **missing GPU timings** (`gpu: None` for 10 frames, then `Some`) leave `gpu_ms` `None` and then set it from the GPU time only, never CPU; **catch-up:** a two-step `advance` whose first step applies a frame and second receives none still reports that frame's capture time; an injected 40 ms frame shows as `frame_ms_worst_1s` and one `late_frame`; **delayed submission:** a 30 ms delay between `advance` and submit gives `track_to_submit_ms` >= 30. 0 allocations. Bench `frame_meter_observe`. | M1 (`PerformOutput.frame`) | ~330 |
| M3 | Perform section | `crates/omuzaki-stage/src/perform/metrics.rs` (+ one `mod` line), `tests/perform_metrics.rs` | In `PerformControl::advance`, after the status publish: copy the `PerformStatus` numbers and `skipped_steps()` into `PerformWriter`. It calls nothing on the `Tracker` and measures no latency. 0 allocations. | M1, V0's T4 | ~100 |
| M11 | Tracker section | the tracker owner's control-thread tick (`crates/omuzaki-track/src/` or the App's, per T2 layout); `tests/tracker_metrics.rs` | A 10 Hz tick on the control thread that owns the `Tracker`: call `Tracker::latency()`, copy the `Copy` `LatencyStats` into `TrackerWriter` (a POD copy; no handoff to the perform thread). Test with `FakeTracker::scripted`: p95 and drops appear; 0 allocations in the tick after warm-up. | M1 | ~100 |
| M4 | `AudioMeter` | `crates/omuzaki-audio/src/meter.rs` (+ one `mod` line), `tests/meter.rs` | A 4 Hz tick: condense `AudioStatus` and the latency report into `AudioMetrics`; sums, no names. `voice_fx_ms` uses `AudioStatus.block_frames`, not a constant. Tests on the mock engine: injected underruns, and block sizes 128, 256 and 1,024 (`voice_fx_ms` differs by the block's duration). | M1 | ~150 |
| M5 | The MCP provider | `crates/omuzaki-mcp/src/resources/metrics.rs` (+ one `mod` line), `tests/metrics_resource.rs` | `MetricsSource` (one method, owned here), `ReaderSource`, `MetricsProvider` serving `omuzaki://metrics/live` (`serves`, `read()`, `templates`); the `running: false` answer. Under the `read()` scope. | M1, A1 (the trait) | ~200 |
| M6 | Compose in the App | `crates/omuzaki-cli/src/app.rs`, `mcp.rs`; `tests/` | §5 steps 1 to 3; a headless test: `omuzaki mcp` serves the `running: false` body. A running-studio test with a published fake section. | M1, M5 | ~120 |
| M7 | The Health panel reads the reader | `crates/omuzaki-ui/src/` (the Live workspace's Health module only), gallery entries | Replace the minimal meter's data source with `MetricsReader`: fps, frame time, latency, late and dropped frames, audio underruns, budget bars with the `live` label. Gallery at 1366x768 and 1920x1080: idle, over-budget, tracking lost. Keyboard reachable, text through `Msg`. | M1, M2, M3 | ~350 |
| M8 | Process section and runtime probes | new runtime crate for the probes (move from `omuzaki-test`'s pattern); `PerformWriter`-style `ProcessWriter` in `omuzaki-metrics-api` | `cpu_share`, `ram_mb` at 1 Hz; Windows `GetProcessTimes` and `PrivateWorkingSetSize`; Linux fallback. | M1 | ~300 |
| M9 | Per-system spans into the record | `crates/omuzaki-stage/src/frame/` spans; `system_ms` in the frame section | Wrap `physics.step`, pose, rig, skinning in the span guard with a second, live sink (EMA plus bucket max); keys equal the §10 keys. | M1, M2 | ~250 |
| M10 | Keep the constants honest | `crates/omuzaki-test/tests/budget_constants.rs` | For each `omuzaki_metrics_api::budget` constant (and `LIVE_GPU`, `FACE_PAINT_GPU`), assert it equals its spec §10 row via the table parser. | M1 merged | ~60 |

**Critical chain:** M1, then M2, M3, M4 and M11 in parallel, then M5, M6 and M7. M1 is the only task that others wait on for a design; once it merges, all but M7 and M9 are small because the contract is fixed.

**Not done here, by design:** no code, no benchmark, no spec edit. If the maintainers accept Q1 to Q5, the spec's §11 line for `omuzaki://metrics/live` ("fps, latency, budget use") needs no change; the field table above is the detail.
