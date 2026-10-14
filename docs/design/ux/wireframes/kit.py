"""Wireframe kit: draws the UX docs' screens as SVG using the design-system dark tokens (design-system.md section 2)."""
from html import escape

# Dark theme tokens (design-system.md section 2)
BG0, BG1, BG2, BG3, BG4 = '#0E0D11', '#16141A', '#1E1B23', '#28242F', '#342F3D'
LINE, CONTROL = '#3A3542', '#7D7589'
TXT, TXT2, TXT3, DIS = '#EFE8D8', '#B9B2C0', '#A09AAB', '#6B6575'
ACC, ON_ACC = '#E6C36A', '#1D1508'
AGENT, ON_AGENT = '#B9A2FF', '#17102B'
OK, WARN, DANGER = '#5FD7B8', '#F58A1F', '#F25A72'
UI = "Instrument Sans, Inter, 'Segoe UI', system-ui, sans-serif"
MONO = "JetBrains Mono, 'DejaVu Sans Mono', ui-monospace, monospace"


class Svg:
    def __init__(self, w, h, title):
        self.w, self.h, self.title, self.items = w, h, title, []

    def add(self, s):
        self.items.append(s)

    def rect(self, x, y, w, h, fill='none', stroke=None, r=0, sw=1, dash=None, op=None):
        a = f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"'
        if stroke: a += f' stroke="{stroke}" stroke-width="{sw}"'
        if dash: a += f' stroke-dasharray="{dash}"'
        if op is not None: a += f' opacity="{op}"'
        self.add(a + '/>')

    def line(self, x1, y1, x2, y2, stroke=LINE, sw=1, dash=None):
        d = f' stroke-dasharray="{dash}"' if dash else ''
        self.add(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{stroke}" stroke-width="{sw}"{d}/>')

    def circle(self, cx, cy, r, fill='none', stroke=None, sw=1, op=None):
        a = f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill}"'
        if stroke: a += f' stroke="{stroke}" stroke-width="{sw}"'
        if op is not None: a += f' opacity="{op}"'
        self.add(a + '/>')

    def ellipse(self, cx, cy, rx, ry, fill='none', stroke=None, sw=1, op=None):
        a = f'<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="{fill}"'
        if stroke: a += f' stroke="{stroke}" stroke-width="{sw}"'
        if op is not None: a += f' opacity="{op}"'
        self.add(a + '/>')

    def path(self, d, fill='none', stroke=None, sw=1, op=None):
        a = f'<path d="{d}" fill="{fill}"'
        if stroke: a += f' stroke="{stroke}" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round"'
        if op is not None: a += f' opacity="{op}"'
        self.add(a + '/>')

    def text(self, x, y, s, size=13, fill=TXT, weight=400, anchor='start', mono=False, op=None, italic=False):
        fam = MONO if mono else UI
        a = f'<text x="{x}" y="{y}" font-family="{fam}" font-size="{size}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}"'
        if op is not None: a += f' opacity="{op}"'
        if italic: a += ' font-style="italic"'
        self.add(a + f'>{escape(s)}</text>')

    # ----- components -----
    def panel(self, x, y, w, h, title, tabs=None, active=0):
        self.rect(x, y, w, h, BG1, LINE, 6)
        self.line(x, y + 28, x + w, y + 28)
        if tabs:
            cx = x + 10
            for i, t in enumerate(tabs):
                tw = 8 * len(t) + 16
                if i == active:
                    self.text(cx, y + 19, t, 13, TXT, 600)
                    self.rect(cx, y + 26, 8 * len(t) + 2, 2, ACC)
                else:
                    self.text(cx, y + 19, t, 13, TXT3)
                cx += tw
        else:
            self.text(x + 10, y + 19, title, 13, TXT, 600)
        self.text(x + w - 12, y + 19, '...', 13, TXT3, anchor='end')

    def btn(self, x, y, w, label, kind='secondary', h=28):
        if kind == 'primary':
            self.rect(x, y, w, h, ACC, None, 4); fg = ON_ACC; wt = 600
        elif kind == 'danger':
            self.rect(x, y, w, h, 'none', DANGER, 4, 1.5); fg = DANGER; wt = 600
        elif kind == 'ghost':
            fg = TXT2; wt = 500
        else:
            self.rect(x, y, w, h, BG3, CONTROL, 4); fg = TXT; wt = 500
        self.text(x + w / 2, y + h / 2 + 4.5, label, 13, fg, wt, 'middle')

    def chip(self, x, y, label, color=TXT2, fill=None, w=None, h=20):
        w = w or (7.2 * len(label) + 18)
        self.rect(x, y, w, h, fill or 'none', color, h / 2, 1)
        self.text(x + w / 2, y + h / 2 + 4, label, 12, color, 500, 'middle')
        return w

    def actor(self, x, y, kind, name):
        col = AGENT if kind == 'agent' else (ACC if kind == 'person' else TXT3)
        self.circle(x + 7, y + 7, 7, col)
        self.text(x + 7, y + 11, name[0], 10, ON_AGENT if kind == 'agent' else ON_ACC, 700, 'middle')
        self.text(x + 20, y + 11, name, 12, TXT2)

    def slider(self, x, y, w, label, t, val, modified=False, default_t=None, agent=False, lw=None):
        lw = lw or int(w * 0.36)
        self.text(x + 12, y + 18, label, 13, TXT2)
        if modified: self.circle(x + 5, y + 14, 2.5, ACC)
        tx, tw = x + lw, w - lw - 96
        ty = y + 14
        self.rect(tx, ty - 2, tw, 4, BG4, None, 2)
        if agent: self.rect(tx - 2, ty - 6, tw + 4, 12, AGENT, None, 6, op=0.22)
        self.rect(tx, ty - 2, tw * t, 4, ACC if not agent else AGENT, None, 2)
        if default_t is not None: self.line(tx + tw * default_t, ty - 6, tx + tw * default_t, ty + 6, TXT3, 1)
        self.circle(tx + tw * t, ty, 7, TXT, BG1, 2)
        self.rect(tx + tw + 8, y + 3, 52, 22, BG2, CONTROL, 4)
        self.text(tx + tw + 8 + 46, y + 18, val, 12, TXT, 400, 'end', mono=True)
        if modified: self.text(x + w - 12, y + 19, '↺', 14, TXT2, anchor='end')

    def seg(self, x, y, labels, sel, h=28, wd=None):
        wd = wd or [8 * len(l) + 22 for l in labels]
        total = sum(wd)
        self.rect(x, y, total, h, BG2, CONTROL, 5)
        cx = x
        for i, (l, w) in enumerate(zip(labels, wd)):
            if i == sel:
                self.rect(cx + 2, y + 2, w - 4, h - 4, BG4, None, 4)
                self.rect(cx + 8, y + h - 5, w - 16, 2, ACC)
                self.text(cx + w / 2, y + h / 2 + 4.5, l, 13, TXT, 600, 'middle')
            else:
                self.text(cx + w / 2, y + h / 2 + 4.5, l, 13, TXT2, 500, 'middle')
            cx += w
        return total

    def toggle(self, x, y, on=True):
        self.rect(x, y, 34, 18, ACC if on else BG4, None if on else CONTROL, 9)
        self.circle(x + (25 if on else 9), y + 9, 6, ON_ACC if on else TXT2)
        if on: self.path(f'M{x+21} {y+9} l3 3 l5 -6', 'none', ON_ACC, 1.6)

    def section(self, x, y, w, label, open_=True, badge=None):
        self.text(x + 10, y + 18, '▾' if open_ else '▸', 12, TXT2)
        self.text(x + 28, y + 18, label, 13, TXT, 600)
        if badge: self.chip(x + w - 12 - (7.2 * len(badge) + 18), y + 4, badge, TXT3)
        self.line(x + 8, y + 26, x + w - 8, y + 26, LINE)

    def tree_row(self, x, y, w, depth, label, kind='', selected=False, expanded=None, vis=True, lock=False, agent=False, warn=False, h=26):
        if selected:
            self.rect(x + 4, y, w - 8, h, BG4, None, 4)
            self.rect(x + 4, y + 3, 2, h - 6, ACC)
        ix = x + 12 + depth * 16
        if expanded is not None: self.text(ix, y + 17, '▾' if expanded else '▸', 11, TXT2)
        self.rect(ix + 14, y + 7, 12, 12, 'none', TXT3, 3)
        self.text(ix + 32, y + 17.5, label, 13, TXT if selected else TXT2, 600 if selected else 400)
        rx = x + w - 18
        if warn: self.path(f'M{rx} {y+8} l7 12 h-14 z', 'none', WARN, 1.5); rx -= 20
        if agent: self.circle(rx + 3, y + 13, 4, AGENT); rx -= 18
        if lock: self.rect(rx - 4, y + 11, 10, 8, 'none', TXT3, 2); rx -= 18
        if vis: self.ellipse(rx, y + 13, 6, 4, 'none', TXT3, 1.3)

    def call(self, n, x, y):
        self.circle(x, y, 13, ACC, BG0, 2)
        self.text(x, y + 5.5, str(n), 15, ON_ACC, 700, 'middle')

    def character(self, cx, cy, s=1.0, glow=None):
        """A generic stand-in chibi (original silhouette, no real character)."""
        def sc(v): return v * s
        if glow:
            self.ellipse(cx, cy - sc(40), sc(150), sc(190), glow, None, op=0.10)
        self.ellipse(cx, cy + sc(150), sc(70), sc(10), '#000000', None, op=0.35)         # contact shadow
        self.path(f'M{cx-sc(52)} {cy+sc(60)} q{sc(52)} {sc(-26)} {sc(104)} 0 l{sc(12)} {sc(86)} h{sc(-128)} z', '#5C5470', None)   # robe
        self.ellipse(cx, cy - sc(70), sc(118), sc(112), '#C9A7E8', None)                  # hair back
        self.ellipse(cx, cy - sc(52), sc(96), sc(92), '#FBE3D1', None)                    # face
        self.path(f'M{cx-sc(104)} {cy-sc(70)} q{sc(104)} {sc(-96)} {sc(208)} 0 q{sc(-30)} {sc(-10)} {sc(-52)} {sc(34)} q{sc(-20)} {sc(-30)} {sc(-52)} {sc(-8)} q{sc(-30)} {sc(30)} {sc(-52)} {sc(-26)} q{sc(-22)} {sc(24)} {sc(-52)} {sc(0)} z', '#B88FE0', None)  # fringe
        for dx in (-34, 34):
            self.ellipse(cx + sc(dx), cy - sc(44), sc(15), sc(21), '#2B2140', None)
            self.circle(cx + sc(dx) - sc(4), cy - sc(52), sc(5), '#FFFFFF')
            self.circle(cx + sc(dx) + sc(6), cy - sc(38), sc(2.5), '#FFFFFF')
        self.path(f'M{cx-sc(10)} {cy-sc(14)} q{sc(10)} {sc(8)} {sc(20)} 0', 'none', '#9A5A63', sc(2.2))
        self.ellipse(cx - sc(56), cy - sc(26), sc(13), sc(7), '#F2A0A8', None, op=0.55)
        self.ellipse(cx + sc(56), cy - sc(26), sc(13), sc(7), '#F2A0A8', None, op=0.55)

    def save(self, path, bg=BG0):
        head = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" height="{self.h}" viewBox="0 0 {self.w} {self.h}" '
                f'role="img" aria-label="{escape(self.title)}">\n<title>{escape(self.title)}</title>\n'
                f'<rect width="{self.w}" height="{self.h}" fill="{bg}"/>\n')
        open(path, 'w').write(head + '\n'.join(self.items) + '\n</svg>\n')
