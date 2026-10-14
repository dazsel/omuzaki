#!/usr/bin/env python3
"""A coherent synthetic webcam clip: one ICT-FaceKit Light identity (MIT) animated through scripted
expressions, head motion and in-frame movement, rendered with the synthface renderer (Blender Cycles, headless).

The 200 stills of `datasets/synthface/spike01` are independent identities, so they cannot exercise video-mode tracking
(the ROI carried from one frame to the next, landmark smoothing). This clip can. Run it as a background Blender job
through the GPU slot:

  env LD_LIBRARY_PATH=/usr/lib/wsl/lib $BLENDER_DIR/blender-5.2.2-linux-x64/blender -b \
      --factory-startup --python spikes/webcam-track/tools/render_clip.py -- \
      --out $OMUZAKI_ASSETS/datasets/synthface/clip01 [--n 90] [--fps 30] [--seed 7] [--res 480]

Output: `clip_000000.png` ... (square RGB frames) and `labels.jsonl` (exact ARKit values and head pose per frame, in the
synthface conventions documented in `render.py`). The renderer itself is imported unchanged from
the synthface `render.py`, found through `OMUZAKI_SYNTHFACE_DIR` (the folder that holds it; it is not part of this
repository).
"""
import argparse
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SYNTHFACE = os.environ.get("OMUZAKI_SYNTHFACE_DIR")
if not SYNTHFACE:
    sys.exit("set OMUZAKI_SYNTHFACE_DIR to the folder that holds the synthface render.py")
sys.path.insert(0, SYNTHFACE)

import numpy as np  # noqa: E402
import render as synth  # noqa: E402  (imports bpy: only works inside Blender)


def pulse(t, centre, half_width):
    """Smooth 0..1 bump centred on `centre` lasting about 2 * half_width seconds."""
    x = abs(t - centre) / half_width
    return 0.0 if x >= 1 else 0.5 * (1 + math.cos(math.pi * x))


def expression(t):
    """Scripted ARKit coefficients over time: speech-like jaw motion, a smile, blinks, brows, a pucker, a cheek puff."""
    blink = max(pulse(t, 0.5, 0.1), pulse(t, 1.7, 0.1), pulse(t, 2.6, 0.1))
    smile = pulse(t, 1.2, 0.7)
    ark = {
        "jawOpen": 0.55 * max(0.0, math.sin(2 * math.pi * t / 1.2)) ** 2,
        "mouthSmileLeft": 0.8 * smile, "mouthSmileRight": 0.7 * smile,
        "eyeBlinkLeft": blink, "eyeBlinkRight": blink,
        "browInnerUp": 0.7 * pulse(t, 1.5, 0.6),
        "browDownLeft": 0.5 * pulse(t, 2.5, 0.4), "browDownRight": 0.5 * pulse(t, 2.5, 0.4),
        "mouthPucker": 0.8 * pulse(t, 2.2, 0.3),
        "cheekPuff": 0.7 * pulse(t, 2.8, 0.2),
    }
    return {k: float(v) for k, v in ark.items() if v > 1e-4}


def main():
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True)
    ap.add_argument("--n", type=int, default=90)
    ap.add_argument("--fps", type=float, default=30.0)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--res", type=int, default=480)
    ap.add_argument("--samples", type=int, default=32, help="Cycles samples (denoised)")
    args = ap.parse_args(argv)

    ns = argparse.Namespace(res=args.res, samples=args.samples, cache=synth.DEFAULT_CACHE, hdri=synth.DEFAULT_HDRI, device="GPU",
                            no_denoise=False, out=args.out)
    renderer = synth.Renderer(ns)
    synth.MODEL_NAMES = renderer.model.arkit_names
    os.makedirs(args.out, exist_ok=True)

    # One identity, lighting and camera for the whole clip; only expression, pose and framing change.
    base = synth.sample_params(renderer.model, renderer.world, args.seed)
    base.update(dist=0.6, hfov=60.0, crop_scale=2.0, key=None)
    with open(os.path.join(args.out, "labels.jsonl"), "w") as labels:
        for i in range(args.n):
            t = i / args.fps
            s = dict(base)
            s.update(ark=expression(t),
                     yaw=22.0 * math.sin(2 * math.pi * t / 3.0), pitch=12.0 * math.sin(2 * math.pi * t / 2.2 + 1.0),
                     roll=10.0 * math.sin(2 * math.pi * t / 2.6 + 2.0),
                     gx=0.5 * math.sin(2 * math.pi * t / 2.2), gy=0.3 * math.sin(2 * math.pi * t / 1.7),
                     crop_jit=np.array([0.10 * math.sin(2 * math.pi * t / 3.5), 0.06 * math.cos(2 * math.pi * t / 2.9)]))
            s["ark_full"] = synth.full_arkit(s["ark"], (s["gx"], s["gy"]))
            name = f"clip_{i:06d}.png"
            renderer.render_sample(s, os.path.join(args.out, name))
            labels.write(json.dumps({
                "file": name, "t": round(t, 4),
                "arkit": {k: round(float(v), 4) for k, v in s["ark_full"].items()},
                "head": {"yaw": round(s["yaw"], 3), "pitch": round(s["pitch"], 3), "roll": round(s["roll"], 3)},
            }) + "\n")
            labels.flush()
            print(f"[clip] {i + 1}/{args.n}", flush=True)


if __name__ == "__main__":
    main()
