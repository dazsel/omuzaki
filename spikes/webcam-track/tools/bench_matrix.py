#!/usr/bin/env python3
"""Interleaved latency / CPU benchmark of every tracker variant (standard library only).

The machine this runs on is shared and often loaded, so each variant is run several rounds in turn (A B C A B C ...)
instead of all rounds of A and then all of B, and the report takes the median across rounds. Every run is paced at the
camera rate (default 30 fps) so CPU use is what a live webcam would cost, and tracks the same clip in video mode.

    python3 -I tools/bench_matrix.py --frames CLIP_DIR --models MODELS_DIR --ort-cpu libonnxruntime.so \
        [--ort-gpu libonnxruntime.so --gpu-ld-path DIRS] [--rounds 5] [--passes 3] --out OUT_DIR

Variants: Rust under `ort` (CPU with 1, 2 and 4 threads, with and without spinning workers; CUDA if --ort-gpu is given), the Python MediaPipe reference
(`ref_mediapipe.py`, run with the interpreter in --python) and the JS tracker in headless Chromium
(`js_tracker_bench.mjs`, needs node; skipped with --no-js).
"""
import argparse
import json
import os
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "..", ".."))


def run(cmd, env=None):
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        sys.exit(f"{' '.join(cmd[:4])} failed:\n{proc.stderr[-800:]}")


def rust_variant(args, label, threads, gpu=False, spin=True):
    def go(round_dir, passes):
        out = os.path.join(round_dir, f"{label}.json")
        cmd = [args.track, "--ort-lib", args.ort_gpu if gpu else args.ort_cpu, "--models", args.models, "--input", args.frames,
               "--out", out, "--mode", "video", "--threads", str(threads), "--repeat", str(passes - 1), "--pace",
               "--backend", "cuda" if gpu else "cpu"] + ([] if spin else ["--no-spin"])
        env = dict(os.environ)
        if gpu:
            env["LD_LIBRARY_PATH"] = args.gpu_ld_path
        run(cmd, env)
        d = json.load(open(out))
        t = d["latency"]["tracked"]["total"]
        return {"mean": t["mean"], "p50": t["p50"], "p95": t["p95"], "cpu": d["latency"]["cpu_percent_of_one_core"]}
    return go


def python_variant(args):
    def go(round_dir, passes):
        out = os.path.join(round_dir, "python-mediapipe.json")
        run([args.python, "-I", os.path.join(HERE, "ref_mediapipe.py"), args.frames, out, "--mode", "video", "--repeat",
             str(passes - 1), "--pace"])
        d = json.load(open(out))
        m = d["ms_per_call"]
        return {"mean": m["mean"], "p50": m["p50"], "p95": m["p95"], "cpu": d["cpu_percent_of_one_core"]}
    return go


def js_variant(args):
    def go(round_dir, passes):
        out = os.path.join(round_dir, "js-tracker.json")
        run(["node", os.path.join(HERE, "js_tracker_bench.mjs"), args.frames, out, "--passes", str(passes)])
        m = json.load(open(out))["ms_per_call"]
        return {"mean": m["mean"], "p50": m["p50"], "p95": m["p95"], "cpu": None}
    return go


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--frames", required=True)
    ap.add_argument("--models", required=True)
    ap.add_argument("--ort-cpu", required=True)
    ap.add_argument("--ort-gpu")
    ap.add_argument("--gpu-ld-path", default="", help="LD_LIBRARY_PATH for the CUDA runs (cudart 12.4+, cuDNN 9, cuBLAS 12)")
    ap.add_argument("--track", default=os.path.join(ROOT, "target", "release", "track"))
    ap.add_argument("--python", default=os.path.expandvars("$TMPDIR/venvs/mp/bin/python"))
    ap.add_argument("--no-js", action="store_true")
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--passes", type=int, default=3, help="passes over the clip per run (first pass included)")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    # Spinning matters a lot: with it off, ONNX Runtime's worker threads sleep between operators and extra threads make
    # this model slower, not faster; with it on, extra threads help but burn several cores.
    variants = [("rust ort-cpu 1 thread, spin (default)", rust_variant(args, "rust-cpu-t1", 1)),
                ("rust ort-cpu 1 thread, no spin", rust_variant(args, "rust-cpu-t1-nospin", 1, spin=False)),
                ("rust ort-cpu 2 threads, spin", rust_variant(args, "rust-cpu-t2", 2)),
                ("rust ort-cpu 4 threads, spin", rust_variant(args, "rust-cpu-t4", 4)),
                ("rust ort-cpu 2 threads, no spin", rust_variant(args, "rust-cpu-t2-nospin", 2, spin=False))]
    if args.ort_gpu:
        variants.append(("rust ort-cuda", rust_variant(args, "rust-cuda", 1, gpu=True)))
    variants.append(("python mediapipe (tflite/xnnpack)", python_variant(args)))
    if not args.no_js:
        variants.append(("js tracker (wasm, headless chromium)", js_variant(args)))

    results = {name: [] for name, _ in variants}
    loads = []
    for r in range(args.rounds):
        round_dir = os.path.join(args.out, f"round{r}")
        os.makedirs(round_dir, exist_ok=True)
        for name, go in variants:
            loads.append(float(open("/proc/loadavg").read().split()[0]))
            results[name].append(go(round_dir, args.passes))
            print(f"round {r} {name}: {results[name][-1]}", flush=True)

    rows = []
    for name, rs in results.items():
        med = lambda k: statistics.median(x[k] for x in rs) if all(x[k] is not None for x in rs) else None
        rows.append({"variant": name, "mean_ms": med("mean"), "p50_ms": med("p50"), "p95_ms": med("p95"), "cpu_percent_of_one_core": med("cpu"),
                     "rounds": rs})
    summary = {"rounds": args.rounds, "passes": args.passes, "load_average_min_max": [min(loads), max(loads)], "rows": rows}
    with open(os.path.join(args.out, "bench.json"), "w") as fh:
        json.dump(summary, fh, indent=1)
    fmt = lambda v, d=1: "n/a" if v is None else f"{v:.{d}f}"
    print(f"\nmedian over {args.rounds} interleaved rounds, 30 fps paced, load average {min(loads):.0f}..{max(loads):.0f}\n")
    print("| variant | mean ms | p50 ms | p95 ms | CPU % of one core |\n|---|---|---|---|---|")
    for row in rows:
        print(f"| {row['variant']} | {fmt(row['mean_ms'], 2)} | {fmt(row['p50_ms'], 2)} | {fmt(row['p95_ms'], 2)} | {fmt(row['cpu_percent_of_one_core'], 0)} |")


if __name__ == "__main__":
    main()
