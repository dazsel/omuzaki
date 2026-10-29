#!/usr/bin/env python3
"""Draws the project's own layered test character ("Pip") as PNG layers plus a `pip.layers.toml` manifest.

Usage: python3 make_layers.py OUT_DIR [--scale N]

The character is drawn from simple shapes (no third-party art), each part on its own layer named the way painters
name them, so `puppet.from_layers` rigs it with no hints. It exists to test and demonstrate the 2D puppet path
(EX-08); the images are generated, never committed (assets stay out of git, ADR 0003). Needs Pillow.
"""

import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

W, H = 1024, 1536
SS = 4  # supersampling for clean edges

SKIN = (255, 228, 214, 255)
SKIN_SHADE = (240, 196, 186, 255)
HAIR = (236, 226, 214, 255)
HAIR_DARK = (196, 180, 168, 255)
HAIR_LINE = (120, 98, 92, 255)
LINE = (70, 46, 48, 255)
IRIS = (190, 40, 60, 255)
IRIS_DARK = (110, 16, 36, 255)
CLOTH = (60, 54, 70, 255)
CLOTH_LIGHT = (92, 84, 104, 255)
GOLD = (214, 170, 80, 255)


def layer():
    img = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    return img, ImageDraw.Draw(img)


def s(*v):
    return [x * SS for x in v]


def done(img, blur=0.0):
    out = img.resize((W, H), Image.LANCZOS)
    if blur:
        out = out.filter(ImageFilter.GaussianBlur(blur))
    return out


def ellipse(d, cx, cy, rx, ry, fill, width=0, outline=None):
    d.ellipse(s(cx - rx, cy - ry, cx + rx, cy + ry), fill=fill, outline=outline, width=width * SS)


def poly(d, pts, fill):
    d.polygon([(x * SS, y * SS) for x, y in pts], fill=fill)


def strand(cx, top, length, width, sway, n=24):
    """A tapering hair lock as a polygon: root at (cx, top), tip `length` below, bending by `sway`."""
    left, right = [], []
    for i in range(n + 1):
        t = i / n
        x = cx + sway * t * t
        y = top + length * t
        w = width * (1 - t) ** 0.8 * 0.5 + 1.0
        left.append((x - w, y))
        right.append((x + w, y))
    return left + right[::-1]


def thick_line(d, pts, width, fill):
    d.line([(x * SS, y * SS) for x, y in pts], fill=fill, width=int(width * SS), joint="curve")


def arc_pts(cx, cy, rx, ry, a0, a1, n=32):
    return [(cx + rx * math.cos(a0 + (a1 - a0) * i / n), cy + ry * math.sin(a0 + (a1 - a0) * i / n)) for i in range(n + 1)]


FACE_C = (512, 560)
FACE_R = (190, 230)
EYES = {"L": 430, "R": 594}  # picture left/right: the character's right/left
EYE_Y = 600


def draw_all():
    layers = []

    def add(name, img, **extra):
        layers.append((name, img, extra))

    # Back hair: a long fall behind the head and shoulders.
    img, d = layer()
    poly(d, [(300, 380), (724, 380), (800, 1200), (690, 1260), (512, 1180), (334, 1260), (224, 1200)], HAIR_DARK)
    ellipse(d, 512, 470, 270, 250, HAIR_DARK)
    add("hair back", done(img))

    # Body: shoulders and a dress with a gold collar band.
    img, d = layer()
    poly(d, [(512, 880), (760, 960), (900, 1120), (930, 1536), (94, 1536), (124, 1120), (264, 960)], CLOTH)
    poly(d, [(512, 900), (700, 980), (760, 1536), (264, 1536), (324, 980)], CLOTH_LIGHT)
    add("body", done(img))

    img, d = layer()
    poly(d, [(452, 760), (572, 760), (586, 930), (438, 930)], SKIN)
    poly(d, [(452, 760), (572, 760), (572, 800), (452, 820)], SKIN_SHADE)
    add("neck", done(img))

    img, d = layer()
    thick_line(d, [(330, 930), (420, 960), (512, 972), (604, 960), (694, 930)], 34, GOLD)
    ellipse(d, 512, 985, 26, 30, (180, 20, 40, 255))
    add("collar", done(img), role="body_accessory")

    # Ears.
    for side, x in (("L", 316), ("R", 708)):
        img, d = layer()
        ellipse(d, x, 590, 34, 60, SKIN)
        ellipse(d, x, 594, 16, 34, SKIN_SHADE)
        add(f"ear {side}", done(img))

    # Face: a soft anime jaw.
    img, d = layer()
    cx, cy = FACE_C
    pts = arc_pts(cx, cy - 20, 196, 210, math.pi, 2 * math.pi)
    pts += [(cx + 190, cy + 40), (cx + 150, cy + 170), (cx + 60, cy + 240), (cx, cy + 252), (cx - 60, cy + 240), (cx - 150, cy + 170), (cx - 190, cy + 40)]
    poly(d, pts, SKIN)
    add("face", done(img))

    img, d = layer()
    for x in EYES.values():
        ellipse(d, x + (8 if x > 512 else -8), 690, 54, 26, (255, 150, 160, 150))
    add("blush", done(img, 6))

    img, d = layer()
    poly(d, [(512, 650), (520, 690), (506, 692)], SKIN_SHADE)
    add("nose", done(img))

    # Eyes: white, iris (with highlight), upper lash line, lower line, closed eye.
    for side, x in EYES.items():
        img, d = layer()
        ellipse(d, x, EYE_Y, 58, 46, (255, 255, 255, 255))
        add(f"eye white {side}", done(img))
        img, d = layer()
        ellipse(d, x, EYE_Y + 4, 34, 42, IRIS)
        ellipse(d, x, EYE_Y + 10, 22, 28, IRIS_DARK)
        ellipse(d, x - 12, EYE_Y - 14, 9, 11, (255, 255, 255, 255))
        ellipse(d, x + 10, EYE_Y + 18, 5, 5, (255, 220, 230, 255))
        add(f"iris {side}", done(img))
        img, d = layer()
        thick_line(d, arc_pts(x, EYE_Y + 18, 66, 64, math.pi * 1.08, math.pi * 1.92), 11, LINE)
        out = 1 if x > 512 else -1
        thick_line(d, [(x + out * 60, EYE_Y - 26), (x + out * 80, EYE_Y - 36)], 7, LINE)
        add(f"upper lash {side}", done(img))
        img, d = layer()
        thick_line(d, arc_pts(x, EYE_Y - 20, 46, 62, math.pi * 0.3, math.pi * 0.7), 4, LINE)
        add(f"lower lash {side}", done(img))
        img, d = layer()
        thick_line(d, arc_pts(x, EYE_Y + 6, 58, 22, math.pi * 0.05, math.pi * 0.95), 9, LINE)
        add(f"eye closed {side}", done(img))
        img, d = layer()
        thick_line(d, arc_pts(x, EYE_Y - 58, 56, 18, math.pi * 1.15, math.pi * 1.85), 9, HAIR_LINE)
        add(f"brow {side}", done(img))

    # Mouth: closed line and open mouth with teeth.
    img, d = layer()
    thick_line(d, arc_pts(512, 718, 30, 10, math.pi * 0.15, math.pi * 0.85), 5, LINE)
    add("mouth", done(img))
    img, d = layer()
    ellipse(d, 512, 742, 34, 26, (120, 30, 44, 255))
    ellipse(d, 512, 756, 20, 10, (230, 110, 120, 255))
    poly(d, [(484, 726), (540, 726), (530, 734), (494, 734)], (255, 255, 255, 255))
    add("mouth open", done(img))

    # Side locks in front of the ears, then bangs.
    img, d = layer()
    for x, sway in ((340, -30), (684, 30)):
        poly(d, strand(x, 470, 520, 78, sway), HAIR)
        thick_line(d, [(x, 520), (x + sway * 0.4, 760)], 3, HAIR_LINE)
    add("side hair", done(img))

    img, d = layer()
    ellipse(d, 512, 420, 232, 150, HAIR)
    for i, (x, length, sway) in enumerate(((360, 230, -20), (420, 260, -8), (480, 210, 6), (540, 250, 4), (600, 230, 14), (660, 220, 24))):
        poly(d, strand(x, 380, length, 92, sway), HAIR)
    for x, length, sway in ((420, 250, -8), (540, 240, 4), (600, 220, 14)):
        thick_line(d, [(x, 420), (x + sway * 0.5, 380 + length * 0.8)], 3, HAIR_LINE)
    add("bangs", done(img))

    # Expressions: squeezed (> <) eyes, white blank eyes, an anger mark, gloom.
    img, d = layer()
    for side, x in EYES.items():
        k = 1 if x < 512 else -1
        thick_line(d, [(x - 40 * k, EYE_Y - 30), (x + 34 * k, EYE_Y), (x - 40 * k, EYE_Y + 30)], 12, LINE)
    add("eyes squeeze", done(img), role="expression", show_when="eyesSqueeze")
    img, d = layer()
    for x in EYES.values():
        ellipse(d, x, EYE_Y, 56, 50, (255, 255, 255, 255), width=8, outline=LINE)
    add("eyes blank", done(img), role="expression", show_when="eyesShock")
    img, d = layer()
    for dx, dy, r in ((0, 0, 0), (40, 6, 1), (16, 40, 2), (-20, 30, 3)):
        a = r * math.pi / 2
        x, y = 690 + dx, 400 + dy
        thick_line(d, [(x - 22 * math.cos(a), y - 22 * math.sin(a)), (x + 10 * math.cos(a + 0.6), y + 10 * math.sin(a + 0.6))], 9, (220, 30, 40, 255))
    add("anger mark", done(img), role="expression", show_when="custom00")
    img, d = layer()
    poly(d, [(320, 440), (704, 440), (704, 640), (320, 640)], (60, 30, 90, 150))
    add("gloom", done(img, 18), role="expression", show_when="eyesGloom")
    return layers


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    toml = ['name = "pip"', f"canvas = [{W}, {H}]", 'artist = "Omuzaki test fixture (generated)"', ""]
    for i, (name, img, extra) in enumerate(draw_all()):
        file = f"{i:02d}_{name.replace(' ', '_')}.png"
        img.save(out / file)
        toml += ["[[layer]]", f'name = "{name}"', f'file = "{file}"']
        toml += [f'{k} = "{v}"' for k, v in extra.items()]
        toml.append("")
    (out / "pip.layers.toml").write_text("\n".join(toml))
    print(f"wrote {out / 'pip.layers.toml'}")


if __name__ == "__main__":
    main()
