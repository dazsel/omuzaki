# Defining the reference machine for the §10 budgets (2026-10-12)

> Research. It backs the new "Reference machines" block in spec v3 §10 and builds on an earlier measurement, which showed what the development PC can and cannot say about a laptop. **Nothing here was measured on a real laptop.** New evidence is public: the Steam hardware survey, vendor spec sheets, VTuber software requirements and Microsoft API documentation, all read on 2026-10-12 through a fetch tool that summarises pages. Each figure that decides something was read once; re-check it before quoting it outside this repo.

## 1. Decisions

| # | Question | Decision (spec §10) | Source of the decision |
|---|---|---|---|
| 1 | Reference laptop | Tier `R`: Ryzen 7 6800U at 28 W, 16 GB LPDDR5-6400, Radeon 680M, Windows 11 | proposal; specs checked in section 3 |
| 2 | Floor | Tier `F`: Core i7-1165G7 at 28 W, 8 GB, Iris Xe 96 EU. May miss `gpu.*` and `cpu.total` only while the §8 scheduler degrades and says so; new row `floor.dropped_frames` makes that testable | proposal; the CPU and RAM are my choice |
| 3 | Upper case | Tier `U`: a laptop with a GTX 1650 Max-Q; `gpu.*` must pass with no step-down | proposal |
| 4 | CPU stand-in | `Dev` pinned to its E-cores; `R`'s core is 1.43x slower than a P-core, an E-core 1.49 to 1.96x | proposal; ratio checked in section 4 |
| 5 | Desktop and Mac | Tier `D` (RTX 3060 12 GB, 8 cores) and tier `M` (Apple M1). §10 and §34 already say "desktop", and QG-07 names "the reference laptop and desktop", but nothing defined them | added here |
| 6 | Is face paint inside the 1.5 ms? | **No, on top.** A repainting frame may use 2.0 ms (4.5 ms in quality mode) | already decided: render API Q6 and `budget::FACE_PAINT_GPU`; §10 now says so |
| 7 | Is the tracker inside the 3% CPU? | **Yes.** `cpu.total` counts the whole process tree; a CPU tracker at its full 5 ms leaves 3 ms | recommendation; for the maintainers to confirm (section 5) |
| 8 | RAM counter | **Private working set**, on each hardware backend, largest value counts; commit and working set are also recorded | recommendation; for the maintainers to confirm (section 6) |
| 9 | Estimates and calibration | GPU numbers (and every converted number) are "estimate" until a `[[calibration]]` entry exists in `reference.toml` | file, as the brief asked |
| 10 | Percentile | p95 for every per-frame row; p50, p99 and max reported | recommendation; it is the convention already used by QG-02, the UX design system's UI budget and the first spec's laptop profile |
| 11 | Pacing and window | paced at 30 fps, at least 20 s warm-up, at least 3,000 frames | recommendation (section 7) |

Nothing here changes an existing budget value. Section 8 lists the budgets the evidence puts at risk.

## 2. Who the tiers stand for

Steam Hardware & Software Survey, **September 2026** (read 2026-10-12). It is a survey of Steam players, who skew to gamers, which is also the audience that streams a game beside an avatar. It is not a survey of VTubers.

| Fact | Value | Used for |
|---|---|---|
| Windows 11 / Windows 10 / Linux / macOS | 71.34% / 23.61% / 3.05% / 1.92% | tiers run Windows 11; Windows 10 is 23.6% of machines and is supported but not measured |
| System RAM | 16 GB 37.82%; 32 GB 42.22%; 8 GB or less 7.64% | `R` 16 GB (conservative against 32 GB); `F` 8 GB |
| Physical CPU cores | 6 cores 27.23%; 8 cores 30.06%; 4 or fewer 13.33% | the cumulative median falls in the 8-core bin (40.72% below it, 70.78% by the end of it): "a modern 8-core" is the median Steam PC |
| Primary display | 1920x1080 47.91%; 2560x1440 27.04%; 3840x2160 4.79% | 1080p is the largest single bin (these are monitors, not stream outputs) |
| Integrated GPUs in the top 30 | Intel Iris Xe 1.52%; AMD "Radeon Graphics" 1.51% (plus 0.82% and 0.42% for two spellings of the same generic name); Intel UHD 1.06% | the generic AMD name hides the model, so the 680M's own share cannot be read |
| Laptop discrete GPUs in the top 30 | RTX 4060 Laptop 3.50%; RTX 5060 Laptop 2.45%; RTX 3060 Laptop 1.66%; RTX 4050 Laptop 1.59%; RTX 4070 Laptop 0.86% | `U` stands for these, with a GTX 1650 Max-Q as its slowest realistic member |
| Desktop GPUs | RTX 3060 3.66% (rank 5); GTX 1650 2.19% | `D` uses the RTX 3060 |

Honest reading: among Steam players, discrete-GPU laptops (10.06% of machines in the top 30) outnumber integrated-only ones in the top 30 (about 5.3% counting every spelling). A reference at the integrated tier is therefore the harder case, not the typical one. The reasons to keep it are in section 3.

## 3. Why the reference is an integrated GPU

**What the competing tools ask for** (Steam store pages, read 2026-10-12):

| Tool | Stated requirements | Integrated GPU listed? |
|---|---|---|
| VTube Studio (Windows) | min: 1.8 GHz CPU, 4 GB RAM, "AMD/NVIDIA, at least 2GB dedicated VRAM", DirectX 11; recommended: 2.5 GHz, 6 GB, 4 GB dedicated VRAM | no: dedicated VRAM is stated |
| Warudo | min: Windows 10, Core i5-9400F, GeForce GTX 1660; no memory figure | no |
| Animaze | min: Core i5-4570, GTX 760 (OpenGL 4.3); recommended: Core i5-6600, GTX 980; notes that the figures assume desktop hardware, and tells laptop users to plug in and use the best graphics adapter | no |
| VSeeFace | "a DirectX compatible GPU", 64-bit Windows 8 or newer; warns that tracking "can be pretty resource intensive" and that 30 to 60 fps "will double the CPU usage of the tracking process" | no GPU model named |

None of them lists an integrated GPU. A budget that holds on a Radeon 680M and a degrade-gracefully promise on an Iris Xe is what makes "runs beside a game on a mid-range laptop" (§1) and "better than every tool" (§2) true for a laptop that has no discrete GPU. The cost is real: the reference GPU is 1.3x slower than a GTX 1650 Max-Q and 3.6x slower than an RTX 3060 Laptop by 3DMark Time Spy graphics (2,313, 3,016 and 8,222; sources below), so the 1.5 ms has to be met on slower hardware than the one Steam players mostly own. A pass on `R` implies a pass on every discrete laptop GPU in section 2.

**Spec §2 says "RTX 3060 / M2 class".** `R` is stricter than that, so the headline stays true, but the two wordings should not disagree.

**Specs of the named parts**

| Part | Facts | Source |
|---|---|---|
| Ryzen 7 6800U | 8 cores, 16 threads, boost up to 4.7 GHz, 20 MB cache, 15 to 28 W cTDP; Zen 3+; Radeon 680M with 12 graphics cores (768 shaders) at 2,200 MHz; memory up to LPDDR5-6400, 2 channels | AMD press release of 2022-01-04 for cores, clock and TDP; AMD product page for the graphics and memory lines (the page returned HTTP 503 when fetched directly, so those lines come from a search result quoting it) |
| Core i7-1165G7 | 4 cores, 8 threads, 96 EU Iris Xe at up to 1.30 GHz, DDR4-3200 or LPDDR4x-4267, 2 channels, cTDP-up 28 W | Intel ARK |
| GTX 1650 Max-Q | TU117 (Turing), 1,024 shaders, 1,020 to 1,245 MHz, 128-bit, up to 4 GB GDDR5 (a GDDR6 variant exists), 112.1 GB/s, 35 W | Notebookcheck |
| Apple M1 | 8 CPU cores, 8-core GPU; about 1.25x a Radeon 680M on Wild Life Extreme (4,946 against 3,955) | UL and Notebookcheck medians, as in the earlier measurement |

**Power matters.** Notebookcheck's 680M figures come from a unit with a 45 W Ryzen 9 6900HS. A 6800U at 28 W shares its power between CPU and GPU, so it can score lower. That is why §10 pins the tier to 28 W, and one more reason the GPU numbers stay "estimate" until a real 6800U laptop has been run. The same page puts the 680M 40 to 60% ahead of Iris Xe 96 EU, which agrees with the ratio used here (2,313 against 1,558, 1.48x).

## 4. The reference CPU core

PassMark single-thread, read 2026-10-12: Ryzen 7 6800U **3,197** (550 samples), a high-end desktop CPU **4,580** (10 October snapshot). A `Dev` P-core is therefore 1.43x faster than `R`'s core, which matches the "1.4x".

The E-cores measured in that earlier measurement are 1.49x (integer), 1.96x (FMA) and 1.74x (L2 memory) slower than a P-core. Against `R`'s 1.43x that is 1.04x to 1.37x slower than `R`, so a budget that passes on the E-cores passes on `R`. For `F`'s core (PassMark 2,713 for the i7-1165G7, from the earlier note) the integer kernel is not conservative (1.69x slower than a P-core against 1.49x), which is one reason `F` is not CPU-gated on `Dev`.

Limit: PassMark and the three kernels measure different things, and `R`'s caches and memory differ from a desktop part's. The rule is a bound, not a prediction.

## 5. The CPU budget and the tracker

3% of 8 cores is 0.03 x 8 = 0.24 core-seconds per second. At 30 fps that is **8 ms of CPU time per frame**, all threads together.

| Item | CPU per frame | Share of 8 ms |
|---|---|---|
| Face tracker, CPU path, at its full 5 ms (one thread busy for 5 ms) | 5 ms | 62.5% (1.9% of 8 cores) |
| Left for the render thread, physics (0.5 ms), animation, face system, output and the UI | 3 ms | 37.5% |

If the tracker sits outside the 3%, the real ceiling on a webcam path is 1.9% more: about 4.9%, not 3%. The spec's own §2 target says "face-only streaming at 1080p30 <= 3% CPU", and a person reading Task Manager sees the whole process, so the recommendation is inside. Measuring the whole process tree also removes a bookkeeping rule: every thread counts wherever it runs, and a GPU, NPU or phone tracker shows only its host-side cost.

Evidence on whether 5 ms is reachable on the CPU: none found. Searches for a published per-frame latency of Google's Face Landmarker on a laptop or desktop CPU turned up only a phone-class figure from a third-party wrapper (1 to 5 ms on a Dimensity 9400, not comparable) and a paper that reports CPU load for the Holistic pipeline on an i7-12700H (p50 and p95 usage of one core between 39.1% and 81.2%, memory 549 to 569 MB). The first spec (`00-deep-research-report.md`, laptop profile) set tracking inference at p95 <= 25 ms on a laptop and <= 12 ms on a desktop; v3's 5 ms is 5x and 2.4x tighter and has no measurement behind it yet. VSeeFace's note that 60 fps doubles tracking CPU is why the row is defined at a 30 fps camera.

What does the arithmetic say about feasibility? Only that the other systems are cheap: the anim hot path sums to about 5 us per frame (`docs/design/anim-api.md` section 13). The render thread's CPU cost and the tracker are the unmeasured items.

## 6. The RAM counter

Windows offers three numbers for a process. Microsoft documents `PrivateWorkingSetSize` ("the current private working set size"; Windows 10 22H2 with the September 2023 update, and Windows 11 22H2 and later) and `PrivateUsage` (commit: "the total amount of private memory that the memory manager has committed"), next to `WorkingSetSize`, which also counts shared pages such as mapped DLLs. Task Manager's default Memory column is the active private working set (a third-party write-up on the Scorpio Software blog; no Microsoft page defines the columns).

The renderer-only numbers from that earlier measurement (1080p, 300 frames, polled from PowerShell, so these are **working set**, not private working set):

| Backend | Peak working set | Share of 400 MB | Peak private commit | Share of 400 MB |
|---|---|---|---|---|
| 4090, Vulkan | 183 MB | 46% | 372 MB | 93% |
| 4090, DX12 | 279 MB | 70% | 476 MB | **119%** |
| WARP | 376 MB | 94% | 560 MB | 140% |

Choosing commit would put a bare renderer over budget on DX12 before a tracker model, a UI or a character is loaded, so commit cannot be the counter without raising the number. Working set over-counts shared driver pages, so it is stricter than private working set but is not what a person reads. **Private working set** is what Task Manager shows, is documented, and is a bound that a leak shows up in; commit stays in the report and in the soak (QG-01), where growth is what matters.

Three more rules in §10 follow from the table: the budget applies on each hardware backend and the largest counts (the backend changes the number by 96 MB); software adapters are excluded (WARP is 94% of the budget by itself and no one ships on it); and the report includes an empty-device baseline so a driver's share is visible. The first harness run must record private working set, working set and commit side by side, because the earlier numbers are working set.

Memory on the GPU side is not in these counters on an integrated GPU: textures live in system memory that Windows reports as shared GPU memory. §10 reports it as `gpu.mem` and has no budget for it. The source is the `GPU Process Memory` performance counters (Dedicated Usage and Shared Usage, the data behind Task Manager's GPU memory columns), which count committed allocations, not resident ones, and which Microsoft KB 4490156 says can read wrong on some Windows 10 builds.

Other OSes: on macOS the counter is `phys_footprint`, read with `task_info(TASK_VM_INFO)` (WebKit reads its footprint this way; Activity Monitor's Memory column appears to follow it, but Apple does not document that). On Linux it is the proportional set size (PSS) from `smaps_rollup`. Neither has a reference machine yet, so both are reported, not gated.

## 7. Measurement choices

- **Paced, not a tight loop.** The 4090 ran at 2,790 MHz under load and 210 MHz idle (that earlier measurement). A GPU that renders 1.5 ms of work and then waits 32 ms clocks differently from one in a loop, and a laptop GPU does so more. The harness paces at 30 fps because that is how it will run; an unpaced run is a useful best case but not the budget. This is reasoning from the clock data, not a measurement on `R`.
- **Warm-up and window.** A 400-frame run read 8 to 10% above a 60,000-frame run on the 4090. "At least 20 s warm-up, at least 3,000 frames (100 s)" is a floor I chose; the calibration run should keep the clock in the report and may tighten it.
- **Timestamps and CPU time, not stopwatches.** CI shares the development PC (host load reached 55 during the research), so only counters that load cannot move are allowed to gate.
- **p95.** The first spec, QG-02, the UI budget and the 10% regression rule all speak in p95 (and p99 for the UI). On the 4090 the GPU p95 was within 1 to 2% of the mean, so the choice costs nothing there; on a real iGPU it will be larger, which is the point of using it.
- **Startup, cold and warm.** QG-07 asks for both without saying what cold is. Definition chosen: cold is the first launch after a reboot (empty OS file cache, our pipeline cache and the driver shader cache kept); the first launch after an install is reported only, because shader compilation time belongs to the driver. The `Dev` gate of 1.0 s is the provisional value from the earlier note.
- **Units.** Task Manager shows MiB as "MB", so memory budgets use that; the installer uses decimal MB, the stricter reading.

## 8. Are the budgets realistic? (no value changed)

| Budget | Evidence | Reading |
|---|---|---|
| `gpu.frame` 1.5 ms | the current spike frame (full body, toon, shadows, outlines) is 0.097 ms on the 4090 and estimates at 0.86 ms on `R` (0.5 to 1.5), 0.70 ms on `U`, 1.21 ms on `F` (0.74 to 2.28), before face paint, hair programs and post | plausible on `R` and `U`; thin headroom once hair and post exist; `F`'s pessimistic end is over, which the floor rule covers |
| `gpu.face_paint` 0.5 ms | no measurement: the GPU face benchmark is a stub | unassessed |
| `cpu.total` 3% | section 5 | at risk only for a CPU tracker running near 5 ms |
| `track.face` 5 ms | no published laptop-CPU figure found; 5x tighter than the first spec's laptop target | unvalidated; first real number comes with ML-08 and QG-09 |
| `ram.runtime` 400 MB | a bare renderer is 46 to 94% of it by working set | at risk: tracker model, character data and the output path still have to fit in what is left, and the AMD and Intel drivers are unmeasured |
| `startup.live` 2 s | renderer-only process start to PNG took 0.97 to 1.13 s on the 4090 (it includes starting a 24 MB `.exe` from the WSL file share, so it overstates) and 2.8 s on WARP | at risk on a slower driver; unmeasured on `R` |
| `physics.step`, `ram.editor`, `latency.*`, `installer.size` | no data in this note | unassessed |

None of this proves a budget impossible, so none is changed. Each "at risk" line becomes a finding at the first `R` calibration run, with the follow-up (raise the number, or change the design) decided by the maintainers then.

## 9. Gaps found, and where they went

| Gap | Where |
|---|---|
| Code and docs cite budgets as "spec §10" that §10 does not contain: opening a 200-file project in 50 ms (`crates/omuzaki-doc/benches/open.rs`), and the editor's 60 fps, UI frame 2 ms p95 / 4 ms p99 at 1080p and idle CPU under 0.5% of 8 cores (`docs/design/ux/design-system.md` section 9). The budget harness reads §10, so these are invisible to it | open: needs a maintainer decision on adding rows |
| §2 says "RTX 3060 / M2 class" for the mid laptop; the feature-map tooling keeps its own hand-copied budget table headed "Mid laptop" | open: the two wordings should not disagree |
| §1 promises "beside a game"; no row measures contention with a game, and no row budgets GPU memory (`gpu.mem` is reported only) | open |
| The budget scene is described but not built (a licence-clean synthetic character, a recorded tracker replay) | noted for the budget harness |

## 10. Open questions for the maintainers

1. **Is a Radeon 680M at 28 W the right "mid laptop"?** Default: yes (section 3). The alternative, a GTX 1650 Max-Q reference with the iGPU as floor only, is 1.3x easier on the GPU rows and matches what Steam players mostly own, but leaves iGPU laptops without a budget.
2. **Is the tracker inside the 3%?** Default: yes. If not, a webcam path costs up to 4.9% in total and §2's "<= 3%" needs a footnote.
3. **Private working set as the RAM counter?** Default: yes. Commit would need a bigger number.
4. **Which real device calibrates first?** A Ryzen 7 6800U-class laptop is the one that turns every estimate into a measurement. Which devices exist is a question for the maintainers.
5. **p95 for the per-frame rows,** and the cold/warm definition of startup.

## Sources

Read 2026-10-12 unless noted.
- Steam Hardware & Software Survey, September 2026: https://store.steampowered.com/hwsurvey/ and https://store.steampowered.com/hwsurvey/videocard/
- System requirements: [VTube Studio](https://store.steampowered.com/app/1325860/VTube_Studio/), [Warudo](https://store.steampowered.com/app/2079120/Warudo/), [Animaze](https://store.steampowered.com/app/1364390/Animaze_by_FaceRig/), [VSeeFace](https://www.vseeface.icu/)
- [AMD Ryzen mobile launch release, 2022-01-04](https://ir.amd.com/news-events/press-releases/detail/1039/amdunveils-new-ryzen-mobile-processors-uniting-zen-3-core-with-amd-rdna-2-graphics-in-powerhousedesign); [AMD Ryzen 7 6800U product page](https://www.amd.com/en/products/processors/laptop/ryzen/6000-series/amd-ryzen-7-6800u.html)
- Intel ARK: [Core i7-1165G7](https://www.intel.com/content/www/us/en/products/sku/208662/intel-core-i71165g7-processor-12m-cache-up-to-4-70-ghz/specifications.html), [Core i7-1260P](https://www.intel.com/content/www/us/en/products/sku/226254/intel-core-i71260p-processor-18m-cache-up-to-4-70-ghz/specifications.html) (96 EU at 1.40 GHz, for comparison)
- Notebookcheck: [GTX 1650 Max-Q](https://www.notebookcheck.net/NVIDIA-GeForce-GTX-1650-Max-Q-GPU-Benchmarks-and-Specs.418763.0.html), [Radeon 680M](https://www.notebookcheck.net/AMD-Radeon-680M-GPU-Benchmarks-and-Specs.589860.0.html), [integrated graphics showdown](https://www.notebookcheck.net/Integrated-graphics-showdown-AMD-Radeon-680M-makes-Intel-Iris-Xe-look-like-child-s-play.618595.0.html), RTX 3060 Laptop median Time Spy graphics 8,222 (range 6,285 to 9,235) from the [Chinese-language edition of its page](https://www.notebookcheck-cn.com/NVIDIA-GeForce-RTX-3060-Laptop-GPU.555234.0.html)
- PassMark single-thread: [Ryzen 7 6800U](https://www.cpubenchmark.net/cpu.php?cpu=AMD+Ryzen+7+6800U&id=4846), a high-end desktop CPU (same site, 10 October snapshot); the i7-1165G7 figure (2,713) is from that earlier measurement
- Microsoft: [KB 4490156, GPU process memory counters](https://support.microsoft.com/help/4490156), [PROCESS_MEMORY_COUNTERS_EX2](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex2), [PROCESS_MEMORY_COUNTERS_EX](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex); Task Manager's Memory column: [Scorpio Software blog, 2023-04-12](https://scorpiosoftware.net/2023/04/12/memory-information-in-task-manager/)
- Face-tracker CPU load: [arXiv 2601.08234](https://arxiv.org/pdf/2601.08234) (Holistic pipeline on an i7-12700H); no vendor latency figure for Face Landmarker on a laptop or desktop CPU was found
- In this repo: `docs/design/render-api.md` (decision Q6), `crates/omuzaki-render-api/src/quality.rs`, `docs/design/anim-api.md` section 13
