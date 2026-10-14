#!/usr/bin/env python3
"""Reference tracker: MediaPipe Tasks FaceLandmarker (Python) over a directory of PNG frames.

Same settings and JSON shape as the earlier `track_face.py` tracker, but it reads PNG frames directly (so
the Rust tracker and this one see identical pixels, no video codec in between) and offers IMAGE mode, where every
frame is independent, next to VIDEO mode, where the face is tracked between frames.

    $TMPDIR/venvs/mp/bin/python -I tools/ref_mediapipe.py FRAMES_DIR OUT.json [--mode image|video] [--fps 30]
        [--conf 0.5] [--model $TMPDIR/models/face_landmarker.task] [--landmarks]
"""
import argparse
import glob
import json
import os
import sys
import time

import cv2
import mediapipe as mp
import numpy as np
from mediapipe.tasks.python import BaseOptions, vision

DEFAULT_MODEL = os.path.expanduser("$TMPDIR/models/face_landmarker.task")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("frames")
    ap.add_argument("out")
    ap.add_argument("--mode", choices=["image", "video"], default="image")
    ap.add_argument("--fps", type=float, default=30.0)
    ap.add_argument("--conf", type=float, default=0.5)
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--landmarks", action="store_true", help="include the 478 landmarks (x, y, z normalised)")
    ap.add_argument("--repeat", type=int, default=0, help="extra timed passes over the frames (the JSON holds the first pass)")
    ap.add_argument("--pace", action="store_true", help="sleep between frames to hold --fps, so CPU use matches a live camera")
    args = ap.parse_args()

    files = sorted(glob.glob(os.path.join(args.frames, "*.png")))
    if not files:
        sys.exit(f"no PNG frames in {args.frames}")
    running = vision.RunningMode.IMAGE if args.mode == "image" else vision.RunningMode.VIDEO
    opts = vision.FaceLandmarkerOptions(
        base_options=BaseOptions(model_asset_path=args.model),
        running_mode=running,
        num_faces=1,
        min_face_detection_confidence=args.conf,
        min_face_presence_confidence=args.conf,
        min_tracking_confidence=args.conf,
        output_face_blendshapes=True,
        output_facial_transformation_matrixes=True,
    )
    names, frames, ms_per_call = None, [], []
    images = [mp.Image(image_format=mp.ImageFormat.SRGB, data=np.ascontiguousarray(cv2.cvtColor(cv2.imread(p), cv2.COLOR_BGR2RGB)))
              for p in files]
    wall0, cpu0 = time.perf_counter(), time.process_time()
    with vision.FaceLandmarker.create_from_options(opts) as lm:
        for index in range(len(files) * (args.repeat + 1)):
            i = index % len(files)
            t = index / args.fps
            t0 = time.perf_counter()
            res = lm.detect(images[i]) if args.mode == "image" else lm.detect_for_video(images[i], int(t * 1000))
            ms_per_call.append((time.perf_counter() - t0) * 1000)
            if args.pace:
                time.sleep(max(0.0, wall0 + (index + 1) / args.fps - time.perf_counter()))
            if index >= len(files):
                continue
            if res.face_blendshapes and res.facial_transformation_matrixes:
                cats = res.face_blendshapes[0]
                if names is None:
                    names = [c.category_name for c in cats if c.category_name != "_neutral"]
                frame = {
                    "t": round(t, 4), "found": True,
                    "bs": [float(c.score) for c in cats if c.category_name != "_neutral"],
                    "m": [float(v) for v in np.asarray(res.facial_transformation_matrixes[0], dtype=np.float64).reshape(16)],
                }
                if args.landmarks:
                    frame["lm"] = [[p.x, p.y, p.z] for p in res.face_landmarks[0]]
                frames.append(frame)
            else:
                frames.append({"t": round(t, 4), "found": False})
    wall, cpu = time.perf_counter() - wall0, time.process_time() - cpu0
    found = sum(f["found"] for f in frames)
    out = {"fps": args.fps, "source": os.path.abspath(args.frames), "start": 0.0, "names": names or [], "frames": frames,
           "detect_rate": found / len(frames),
           "settings": {"mode": args.mode, "confidence": args.conf, "model": os.path.basename(args.model), "mediapipe": mp.__version__},
           "ms_per_call": {"mean": float(np.mean(ms_per_call)), "p50": float(np.percentile(ms_per_call, 50)),
                           "p95": float(np.percentile(ms_per_call, 95))},
           "cpu_percent_of_one_core": 100 * cpu / wall, "wall_seconds": wall, "timed_calls": len(ms_per_call)}
    with open(args.out, "w") as fh:
        json.dump(out, fh, separators=(",", ":"))
    print(f"frames {len(frames)}  found {found}  mean {out['ms_per_call']['mean']:.2f} ms/call  p95 {out['ms_per_call']['p95']:.2f}  "
          f"CPU {out['cpu_percent_of_one_core']:.0f}% of one core  wrote {args.out}")


if __name__ == "__main__":
    main()
