# tools/latency: the V0 glass-to-glass latency harness

- `tools/latency/stimulus.py` writes a synthetic face clip whose mouth snaps open at known frames, with a burned-in frame counter.
- `tools/latency/analyze.py` reads a screen recording decoded to grey frames and reports the p50, p95 and max delay from the stimulus mouth opening to the output's jaw signal crossing half way, over at least 50 events (#1967).
- Analysis tests run in CI on a synthetic recording with an injected delay (`tools/tests/latency.sh`); `docs/maintainers/latency-harness.md` is the run procedure for Windows with a real webcam.
