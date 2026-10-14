# Speech host CPU share while transcribing

Question: the speech host used 10 to 22 % of 8 cores while transcribing continuously, against `cpu.total` ≤ 3 % (spec v3 §10). What does it take on the CPU path, and what is the baseline?

## What changed in `omuzaki-audio`
- **ASR is demand-driven.** `SpeechHost::request_transcripts()` returns a clonable `TranscriptDemand`; Whisper runs only while one is alive. With none, the VAD still emits `SpeechStarted` / `SpeechEnded` (barge-in, speaking indicator) but nothing is copied or queued (`HostStats::utterances_skipped` counts it). A job still queued when the last claim goes is skipped.
- **The hangover is a parameter** (`HostConfig::segmenter.min_silence_ms`, default now `DEFAULT_HANGOVER_MS` = 600 ms, was 200). A longer hangover merges phrases separated by a short pause into one Whisper call; each call costs a near-fixed encoder pass over a 30 s window, so fewer calls is the lever.
- `host_bench` takes `[hangover-ms] [listen]` and reports calls, talk ratio and the process CPU share.

## Method
`cargo build --release --example host_bench`; the clip is LibriVox "Aesop 52" (public domain, 53.7 s, talk ratio 0.84-0.90 counting padding; denser than the 50 % in the budget, so these CPU shares are high for a real stream), played at 1x in 10 ms blocks. Process CPU (`/proc/self/stat`, user + system, all threads) over wall time / (8 x wall) = the §10 `cpu.total` basis. Machine: a high-end desktop CPU in WSL2, **shared with other jobs (load 10 to 21), so numbers are noisy and unscaled** (no conversion to `R`). Every configuration was run twice, interleaved; the table gives both runs (run 1 / run 2). "Latency" = last spoken sound to final text, wall clock, p50 / p95.

| Model | Threads | Hangover | Whisper calls | CPU, % of 8 cores | Latency p50 (s) | Latency p95 (s) |
|---|---|---|---|---|---|---|
| any | n/a | listening only | 0 | 0.26 / 0.22 | n/a | n/a |
| base.en | 4 | 200 ms (old default) | 16 | 12.3 / 15.4 | 1.07 / 1.24 | 1.22 / 1.68 |
| base.en | 4 | 400 ms | 13 | 12.0 / 10.7 | 1.45 / 1.26 | 1.65 / 1.76 |
| base.en | 4 | **600 ms (new default)** | **10** | **7.7 / 6.7** | 1.52 / 1.32 | 1.74 / 1.80 |
| base.en | 4 | 800 ms | 6 | 5.9 / 6.7 | 1.94 / 2.26 | 2.22 / 2.64 |
| base.en | 2 | 600 ms | 10 | 9.3 / 7.5 | 2.50 / 2.03 | 3.47 / 2.95 |
| base.en | 1 | 200 ms | 16 | 11.7 / 11.7 | 11.6 / 11.3 | 15.4 / 14.2 |
| base.en | 1 | 600 ms | 10 | 9.2 / 7.4 | 8.7 / 4.1 | 9.9 / 4.5 |
| tiny.en | 4 | 200 ms | 16 | 7.4 / 8.3 | 0.74 / 0.77 | 1.04 / 1.14 |
| tiny.en | 4 | 600 ms | 10 | 4.4 / 4.4 | 1.06 / 1.06 | 1.81 / 1.39 |

## Findings
1. **Listening is almost free**: 0.22 to 0.27 % of 8 cores (VAD thread 0.2 %). The 10 to 22 % was all Whisper, so gating ASR on demand turns the baseline into `asr.listen` (≤ 0.5 %).
2. **A 600 ms hangover removes 6 of 16 calls and about 40 % of the CPU** (best 12.3 to 7.7, 15.4 to 6.7) for roughly 0.3 to 0.45 s more latency at p50 and 0.1 to 0.5 s at p95. 800 ms drops to 6 calls but costs 0.9 to 1.0 s more; the extra saving is small against the noise, and it delays end-of-speech more than a stream's captions want. Merging is bounded by `max_speech_ms` (28 s). An accuracy check on the same clip at 600 ms kept recall at 0.991 over 113 reference words (`asr_real`).
3. **One thread is a loss.** At 4 threads the real-time factor is 0.14 to 0.16; at 1 thread it is 0.6 to 1.2, so Whisper falls behind and latency grows to 4 to 15 s with CPU hardly lower (7.4 to 11.7 % against 6.7 to 7.7 %: the same work spread over more wall time). 2 threads at 600 ms: latency 2.0 to 3.5 s for 7.5 to 9.3 %. Not the default.
4. **`tiny.en` at 600 ms is the floor on the CPU: 4.4 %** at p95 1.4 to 1.8 s. That is still above 3 % at an 84 % talk ratio (about 2.6 % at 50 %, linear in talk time, unmeasured), so the CPU fallback cannot meet `asr.live` and the UI must show the cost.
5. The 600 ms default also delays `SpeechEnded` by 0.4 s; `SpeechStarted` (barge-in) is unaffected. A conversation loop that wants quick turn-taking can lower it per host.

## Caveats
One clip, one machine, shared load; spreads between runs of 20 to 50 % exist. Treat the ratios (calls 16 to 10, CPU about -40 %) as solid and the absolute shares as indicative.

## GPU Whisper
CPU Whisper cost is per call (a fixed encoder window) and per thread count, so a GPU recogniser changes the shape: calls become cheap and the hangover can go back to 200 ms. Its measurements should use `host_bench` (same clip, same columns) and its `Recognizer` slots in behind the demand gate unchanged.
