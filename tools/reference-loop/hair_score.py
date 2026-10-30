#!/usr/bin/env python3
"""Scores the kit's hair against a VRoid sample's in our own renderer, at matching framings (#2536).

    python3 tools/reference-loop/hair_score.py SAMPLE.vrm OUT_DIR [--template tall] [--style long-straight]
        [--op NAME JSON ...] [--bin target/release/omuzaki]

Builds a kit character in a fresh project under OUT_DIR (`character.create`, then `hair.style.apply` and any `--op`,
each an op name and its arguments, run on the character's hair part), imports SAMPLE.vrm beside it as an avatar
(`io.inspect`, `io.import`), renders both with `render.preview` at the same framings, and writes to OUT_DIR:

  - side-by-side.png: ours on the left, the sample on the right, one row per framing;
  - scores.json: for each framing and each character, the numbers below over the hair region, and ours minus theirs.

The hair region is the character against the backdrop (whatever differs from the render's corners), above the brows
in the front views (the eyes' height from `render.preview`'s face landmarks, less a third of the eyes-to-mouth
distance) and in the top 55% of the character's height in the back view. The numbers, in CIE L*a*b* (D65):

  - lightness: mean L*; chroma: mean C*ab, how colourful the hair reads;
  - highlight: p98 minus p50 of L*, how far the highlights stand out of the body colour;
  - shadow: p50 minus p5 of L*, how deep the shadows go;
  - edges: the share of the region whose L* gradient (Sobel) passes 12 per pixel: the lines the eye reads as strands,
    lock edges and outline;
  - grain: the mean absolute Laplacian of L*, the pixel-scale noise that reads as shimmer in motion.

The sample is only studied: keep SAMPLE.vrm and OUT_DIR outside the repository. It reads images with Pillow and numpy.
"""

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

import numpy as np
from PIL import Image, ImageDraw

SIZE = [640, 640]
FRAMINGS = {"front": dict(yaw=25, pitch=5), "side": dict(yaw=80, pitch=5), "back": dict(yaw=160, pitch=8)}
EDGE = 12.0  # L* per pixel of Sobel gradient that counts as an edge
BACKDROP = 18.0  # Delta E from the corners' colour beyond which a pixel is the character


class Omuzaki:
    """The `omuzaki` command line on one project, with its data and settings kept under `home`."""

    def __init__(self, binary: pathlib.Path, project: pathlib.Path, home: pathlib.Path):
        self.binary, self.project = str(binary), str(project)
        self.env = dict(os.environ, OMUZAKI_DATA_DIR=str(home / "data"), OMUZAKI_CONFIG_DIR=str(home / "cfg"))

    def new(self) -> None:
        shutil.rmtree(self.project, ignore_errors=True)
        subprocess.run([self.binary, "new", self.project], env=self.env, check=True, capture_output=True)

    def op(self, name: str, args: dict) -> dict:
        run = subprocess.run(
            [self.binary, "--project", self.project, "--json", "run", name, "--args", json.dumps(args)],
            env=self.env, capture_output=True, text=True,
        )
        if run.returncode != 0:
            sys.exit(f"{name} was refused: {run.stderr.strip()[:600]}")
        return json.loads(run.stdout)

    def render(self, character: str, out: pathlib.Path, **view) -> dict:
        answer = self.op("render.preview", {"character": character, "size": SIZE, "quality": "high",
                                            "framing": "face", **view})
        text = json.dumps(answer)
        start = text.index("omuzaki://render/")
        uri = text[start:text.index('"', start)]
        subprocess.run([self.binary, "store", "export", uri, str(out)], env=self.env, check=True, capture_output=True)
        return answer


def lab(rgb: np.ndarray) -> np.ndarray:
    """sRGB bytes (H x W x 3) to CIE L*a*b* under D65."""
    c = rgb.astype(np.float64) / 255.0
    lin = np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)
    xyz = lin @ np.array([[0.4124, 0.2126, 0.0193], [0.3576, 0.7152, 0.1192], [0.1805, 0.0722, 0.9505]])
    xyz /= np.array([0.95047, 1.0, 1.08883])
    f = np.where(xyz > 216 / 24389, np.cbrt(xyz), (24389 / 27 * xyz + 16) / 116)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], axis=-1)


def landmarks(answer: dict) -> dict | None:
    """The first face's eye and mouth positions in pixels, if the render found one."""
    faces = answer.get("plan", {}).get("output", {}).get("face_landmarks") or []
    return faces[0] if faces else None


def region(image: np.ndarray, answer: dict, framing: str) -> np.ndarray:
    """The hair region of `image`: the character, above the brows (front views) or in its top 55% (back)."""
    l = lab(image)
    corners = np.concatenate([l[:8, :8].reshape(-1, 3), l[:8, -8:].reshape(-1, 3)])
    subject = np.linalg.norm(l - np.median(corners, axis=0), axis=-1) > BACKDROP
    rows = np.where(subject.any(axis=1))[0]
    if rows.size == 0:
        return subject
    top, bottom = rows[0], rows[-1]
    face = landmarks(answer)
    if framing != "back" and face:
        eyes = (face["eye_left"][1] + face["eye_right"][1]) / 2
        cut = int(eyes - (face["mouth"][1] - eyes) / 3)
    else:
        cut = int(top + 0.55 * (bottom - top))
    mask = subject.copy()
    mask[max(cut, 0):] = False
    return mask


def scores(image: np.ndarray, mask: np.ndarray) -> dict:
    l = lab(image)
    L = l[..., 0]
    gx = np.zeros_like(L)
    gy = np.zeros_like(L)
    gx[1:-1, 1:-1] = (L[:-2, 2:] + 2 * L[1:-1, 2:] + L[2:, 2:] - L[:-2, :-2] - 2 * L[1:-1, :-2] - L[2:, :-2]) / 8
    gy[1:-1, 1:-1] = (L[2:, :-2] + 2 * L[2:, 1:-1] + L[2:, 2:] - L[:-2, :-2] - 2 * L[:-2, 1:-1] - L[:-2, 2:]) / 8
    lap = np.zeros_like(L)
    lap[1:-1, 1:-1] = L[:-2, 1:-1] + L[2:, 1:-1] + L[1:-1, :-2] + L[1:-1, 2:] - 4 * L[1:-1, 1:-1]
    inside = mask.copy()
    inside[[0, -1], :] = False
    inside[:, [0, -1]] = False
    values = L[inside]
    if values.size == 0:
        return {"pixels": 0}
    p5, p50, p98 = np.percentile(values, [5, 50, 98])
    return {
        "pixels": int(values.size),
        "lightness": round(float(values.mean()), 2),
        "chroma": round(float(np.hypot(l[..., 1], l[..., 2])[inside].mean()), 2),
        "highlight": round(float(p98 - p50), 2),
        "shadow": round(float(p50 - p5), 2),
        "edges": round(float((np.hypot(gx, gy)[inside] > EDGE).mean()), 4),
        "grain": round(float(np.abs(lap[inside]).mean()), 3),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("sample", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--template", default="tall")
    parser.add_argument("--style", default="long-straight")
    parser.add_argument("--op", nargs=2, action="append", default=[], metavar=("NAME", "JSON"))
    parser.add_argument("--bin", type=pathlib.Path, default=pathlib.Path("target/release/omuzaki"))
    a = parser.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    cli = Omuzaki(a.bin.resolve(), a.out / "project", a.out)
    cli.new()
    hair = {"character": "kit", "part": "hair"}
    cli.op("character.create", {"id": "kit", "name": "Kit", "template": a.template})
    if a.style != "default":
        cli.op("hair.style.apply", {**hair, "style": a.style})
    for name, args in a.op:
        cli.op(name, {**hair, **json.loads(args)})
    # The avatar import reads only files inside the project.
    sample = a.out / "project" / "sample" / a.sample.name
    sample.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(a.sample, sample)
    inspected = json.dumps(cli.op("io.inspect", {"path": str(sample), "as": "avatar"}))
    confirm = re.search(r'"confirm": "([0-9a-f]+)"', inspected)
    if not confirm:
        sys.exit("io.inspect gave no confirmation token")
    cli.op("io.import", {"path": str(sample), "as": "avatar", "id": "sample", "confirm": confirm.group(1)})

    report, rows = {}, []
    for framing, view in FRAMINGS.items():
        row, entry = [], {}
        for who in ["kit", "sample"]:
            path = a.out / f"{framing}-{who}.png"
            answer = cli.render(who, path, **view)
            image = np.asarray(Image.open(path).convert("RGB"))
            mask = region(image, answer, framing)
            entry[who] = scores(image, mask)
            shown = image.copy()
            shown[~mask] = (shown[~mask] * 0.35).astype(np.uint8)
            row.append(Image.fromarray(np.concatenate([image, shown], axis=1)))
        entry["ours_minus_sample"] = {k: round(entry["kit"][k] - entry["sample"][k], 4)
                                      for k in entry["kit"] if k != "pixels" and k in entry["sample"]}
        report[framing] = entry
        rows.append((framing, row))
    width = 4 * SIZE[0]
    sheet = Image.new("RGB", (width, len(rows) * (SIZE[1] + 24)), "white")
    draw = ImageDraw.Draw(sheet)
    for i, (framing, (ours, theirs)) in enumerate(rows):
        y = i * (SIZE[1] + 24)
        draw.text((6, y + 5), f"{framing}: ours (render, scored region)  |  sample (render, scored region)", fill="black")
        sheet.paste(ours, (0, y + 24))
        sheet.paste(theirs, (2 * SIZE[0], y + 24))
    sheet.save(a.out / "side-by-side.png")
    (a.out / "scores.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
