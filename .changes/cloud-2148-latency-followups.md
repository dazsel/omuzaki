# tools/latency: stale frames, timed-out events in the JSON, `--out-range` in either order

- `stimulus.py` deletes the `frame_*.png` files already in the output folder before writing, so a shorter clip no longer leaves the tail of a longer one for ffmpeg to read.
- `analyze.py --json` lists every event with its frame, status (`answered` or `timed_out`) and delay (`null` when timed out), with the window, so the percentiles can be recomputed from the report. This replaces `delays_ms`.
- `--out-range` accepts the two levels in either order, in both polarities (#2148).
