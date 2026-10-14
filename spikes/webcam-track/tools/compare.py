#!/usr/bin/env python3
"""Per-channel comparison of two tracker outputs in the `track_face.py` JSON shape.

    python3 -I tools/compare.py REFERENCE.json CANDIDATE.json [--markdown OUT.md] [--json OUT.json]

Frames are matched by index. Only frames where both trackers found a face are compared; frames found by one tracker
only are counted and reported. Reports, over the matched frames:
  * the 51 blendshape channels: mean absolute error, RMSE, 99th-percentile and maximum absolute error, bias, Pearson r;
  * head pose as yaw / pitch / roll in degrees (track_face.py's convention) and translation in centimetres;
  * landmarks (when both files carry them): mean and maximum distance in pixels of a 640x480 frame-equivalent
    (normalised x scaled by width and y by height of --frame-size, default the 256x256 of the synthetic frames).
Needs only numpy.
"""
import argparse
import json
import math
import sys

import numpy as np


def euler_deg(m16):
    """(yaw, pitch, roll) in degrees from a row-major 4x4 pose matrix, scale removed (same maths as track_face.py)."""
    r = np.asarray(m16, dtype=np.float64).reshape(4, 4)[:3, :3]
    r = r / np.maximum(np.linalg.norm(r, axis=0, keepdims=True), 1e-9)
    return (math.degrees(math.atan2(r[0, 2], r[2, 2])), math.degrees(math.asin(max(-1.0, min(1.0, r[1, 2])))),
            math.degrees(math.atan2(r[1, 0], r[1, 1])))


def wrap_deg(a):
    return (a + 180.0) % 360.0 - 180.0


def load(path):
    with open(path) as fh:
        return json.load(fh)


def channel_stats(ref, cand):
    err = cand - ref
    a = np.abs(err)
    r = float(np.corrcoef(ref, cand)[0, 1]) if ref.std() > 1e-9 and cand.std() > 1e-9 else float("nan")
    return {"mae": float(a.mean()), "rmse": float(np.sqrt((err ** 2).mean())), "p99": float(np.percentile(a, 99)), "max": float(a.max()),
            "bias": float(err.mean()), "r": r, "ref_std": float(ref.std())}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("reference")
    ap.add_argument("candidate")
    ap.add_argument("--markdown")
    ap.add_argument("--json")
    ap.add_argument("--frame-size", default="256x256", help="WxH used to turn normalised landmarks into pixels")
    ap.add_argument("--label", default="candidate vs reference")
    args = ap.parse_args()
    fw, fh = (int(v) for v in args.frame_size.split("x"))

    ref, cand = load(args.reference), load(args.candidate)
    if ref["names"] != cand["names"]:
        sys.exit("blendshape channel names differ between the two files")
    names = ref["names"]
    n = min(len(ref["frames"]), len(cand["frames"]))
    pairs = [(ref["frames"][i], cand["frames"][i]) for i in range(n)]
    both = [(a, b) for a, b in pairs if a["found"] and b["found"]]
    only_ref = sum(a["found"] and not b["found"] for a, b in pairs)
    only_cand = sum(b["found"] and not a["found"] for a, b in pairs)
    if not both:
        sys.exit("no frame has a face in both files")

    bs_ref = np.array([a["bs"] for a, _ in both])
    bs_cand = np.array([b["bs"] for _, b in both])
    channels = {name: channel_stats(bs_ref[:, i], bs_cand[:, i]) for i, name in enumerate(names)}
    all_abs = np.abs(bs_cand - bs_ref)

    eul_ref = np.array([euler_deg(a["m"]) for a, _ in both])
    eul_cand = np.array([euler_deg(b["m"]) for _, b in both])
    eul_err = np.vectorize(wrap_deg)(eul_cand - eul_ref)
    trans_ref = np.array([[a["m"][3], a["m"][7], a["m"][11]] for a, _ in both])
    trans_cand = np.array([[b["m"][3], b["m"][7], b["m"][11]] for _, b in both])
    pose = {}
    for i, name in enumerate(["yaw", "pitch", "roll"]):
        e = np.abs(eul_err[:, i])
        pose[name + "_deg"] = {"mae": float(e.mean()), "p99": float(np.percentile(e, 99)), "max": float(e.max()),
                               "bias": float(eul_err[:, i].mean()), "ref_range": [float(eul_ref[:, i].min()), float(eul_ref[:, i].max())]}
    for i, name in enumerate(["x", "y", "z"]):
        e = np.abs(trans_cand[:, i] - trans_ref[:, i])
        pose["t" + name + "_cm"] = {"mae": float(e.mean()), "p99": float(np.percentile(e, 99)), "max": float(e.max()),
                                    "bias": float((trans_cand[:, i] - trans_ref[:, i]).mean())}

    landmarks = None
    if all("lm" in a and "lm" in b for a, b in both):
        lr = np.array([a["lm"] for a, _ in both])
        lc = np.array([b["lm"] for _, b in both])
        d = np.hypot((lc[:, :, 0] - lr[:, :, 0]) * fw, (lc[:, :, 1] - lr[:, :, 1]) * fh)
        dz = np.abs(lc[:, :, 2] - lr[:, :, 2]) * fw
        landmarks = {"mean_px": float(d.mean()), "p99_px": float(np.percentile(d, 99)), "max_px": float(d.max()),
                     "z_mean_px": float(dz.mean()), "frame": f"{fw}x{fh}"}

    summary = {"label": args.label, "frames": n, "matched": len(both), "only_reference": only_ref, "only_candidate": only_cand,
               "blendshape_mae_mean": float(all_abs.mean()), "blendshape_mae_max_channel": max(c["mae"] for c in channels.values()),
               "blendshape_abs_err_p99": float(np.percentile(all_abs, 99)), "blendshape_abs_err_max": float(all_abs.max()),
               "channels": channels, "pose": pose, "landmarks": landmarks}
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(summary, fh, indent=1)

    lines = [f"### {args.label}", "",
             f"{len(both)} of {n} frames matched (face in both); only in reference: {only_ref}; only in candidate: {only_cand}.", "",
             f"Blendshapes (51 channels, scores 0..1): mean absolute error over all channels and frames **{summary['blendshape_mae_mean']:.5f}**, "
             f"p99 {summary['blendshape_abs_err_p99']:.4f}, max {summary['blendshape_abs_err_max']:.4f}; worst channel MAE "
             f"{summary['blendshape_mae_max_channel']:.5f}.", "",
             "| head pose | MAE | p99 | max | bias |", "|---|---|---|---|---|"]
    for key, v in pose.items():
        unit = "deg" if key.endswith("_deg") else "cm"
        lines.append(f"| {key[:-len(unit) - 1]} ({unit}) | {v['mae']:.4f} | {v['p99']:.4f} | {v['max']:.4f} | {v['bias']:+.4f} |")
    if landmarks:
        lines += ["", f"Landmarks (478, {landmarks['frame']} px frame): mean distance **{landmarks['mean_px']:.4f} px**, p99 "
                  f"{landmarks['p99_px']:.4f} px, max {landmarks['max_px']:.4f} px; depth (z) mean {landmarks['z_mean_px']:.4f} px."]
    lines += ["", "<details><summary>Per-channel blendshape error</summary>", "", "| channel | MAE | RMSE | p99 | max | bias | r | ref std |",
              "|---|---|---|---|---|---|---|---|"]
    for name, c in channels.items():
        lines.append(f"| {name} | {c['mae']:.5f} | {c['rmse']:.5f} | {c['p99']:.4f} | {c['max']:.4f} | {c['bias']:+.5f} | {c['r']:.5f} | {c['ref_std']:.4f} |")
    lines += ["", "</details>", ""]
    text = "\n".join(lines)
    if args.markdown:
        with open(args.markdown, "w") as fh:
            fh.write(text)
    print(text if not args.markdown else "\n".join(lines[:10]))


if __name__ == "__main__":
    main()
