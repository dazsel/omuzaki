"""Draws the UX wireframes in this folder as SVG, using the dark design-system tokens (`kit.py`).

Regenerate with `python3 scenes.py .` from this folder. The SVGs are generated: edit this file and re-run, never
edit an SVG by hand. Sizes are the real window sizes in points (1920x1080 and 1366x768); the numbered gold
callouts match the legends in `../shell.md`. The stand-in character is an original silhouette, not a real
character (docs/licensing/policy.md).
"""
from kit import *


TABS = [('Concept', 0), ('Build', 0), ('Face', 0), ('Rig', 0), ('Look', 0), None,
        ('Perform', 0), ('Stage', 0), ('Broadcast', 0), None, ('Shots', 0), None, ('Theatre', 1)]


def topbar(s, w, active='Build', compact=False, project='sample-show', agent_text='Designer  4/9', live=True, theatre_dot=True, saved=True):
    s.rect(0, 0, w, 40, BG2)
    s.line(0, 40, w, 40, LINE)
    s.rect(8, 6, 28, 28, BG3, None, 4)
    s.text(22, 25, '≡', 16, TXT, 500, 'middle')
    s.text(46, 25, 'Omuzaki', 15, TXT, 700)
    px = 104 if not compact else 100
    s.text(px, 25, project, 13, TXT2)
    s.circle(px + 8 * len(project) + 12, 21, 3.5, OK)
    if not compact: s.text(px + 8 * len(project) + 22, 25, 'saved', 12, TXT3)
    # tabs
    x = 250 if not compact else 214
    for t in TABS:
        if t is None:
            s.line(x + 3, 10, x + 3, 30, LINE); x += 10; continue
        name, special = t
        is_act = name == active
        if compact and not is_act:
            wdt = 36
            s.rect(x + 11, 13, 14, 14, 'none', TXT3, 3)
        else:
            wdt = 8 * len(name) + 40
            if is_act:
                s.rect(x, 5, wdt, 30, BG3, None, 5)
                s.rect(x + 10, 33, wdt - 20, 2, ACC)
            s.rect(x + 10, 13, 14, 14, 'none', TXT if is_act else TXT3, 3)
            s.text(x + 30, 25, name, 13, TXT if is_act else TXT2, 600 if is_act else 500)
        if name == 'Theatre' and theatre_dot:
            s.circle(x + wdt - 8, 12, 4, AGENT)
        x += wdt
    tabs_end = x
    # right cluster
    rx = w - 12
    if live:
        s.rect(rx - 96, 6, 96, 28, 'none', DANGER, 5, 1.5)
        s.circle(rx - 80, 20, 4, DANGER)
        s.text(rx - 36, 25, 'Go Live', 13, DANGER, 600, 'middle')
        rx -= 108
    # agent chip
    cw = 8 * len(agent_text) + 40
    s.rect(rx - cw, 6, cw, 28, 'none', AGENT, 14, 1.2)
    s.circle(rx - cw + 16, 20, 5, AGENT)
    s.text(rx - cw + 28, 25, agent_text, 12.5, TXT, 500)
    rx -= cw + 12
    pw = 230 if not compact else 36
    s.rect(rx - pw, 6, pw, 28, BG1, CONTROL, 5)
    if not compact:
        s.text(rx - pw + 12, 25, 'Search or run…', 13, TXT3)
        s.text(rx - 12, 25, 'Ctrl K', 12, TXT3, anchor='end', mono=True)
    else:
        s.text(rx - pw / 2, 25, 'K', 13, TXT2, 600, 'middle')
    return tabs_end, rx - pw


def statusbar(s, w, h, compact=False, left='Face profile · 1 selected', undo='Undo: Head › face profile width 0.62', fence='Fence: Designer holds Head, Hair'):
    y = h - 24
    s.rect(0, y, w, 24, BG2)
    s.line(0, y, w, y, LINE)
    s.text(12, y + 16, left, 12, TXT2)
    ux = 220 if not compact else 190
    s.text(ux, y + 16, undo, 12, TXT2)
    s.text(ux + 7.1 * len(undo) + 8, y + 16, 'Ctrl Z', 11, TXT3, mono=True)
    if not compact:
        s.text(ux + 7.1 * len(undo) + 78, y + 16, fence, 12, TXT3)
    s.text(w - 12, y + 16, '60 fps  ·  GPU 0.9 ms  ·  38 ms  ·  $0.12 / $2.00', 12, TXT2, anchor='end', mono=True)


def outliner(s, x, y, w, h, tabbed=False):
    s.panel(x, y, w, h, 'Outliner', tabs=['Outliner', 'Library', 'Assets'] if tabbed else None)
    s.rect(x + 8, y + 36, w - 16, 28, BG2, CONTROL, 5)
    s.text(x + 18, y + 55, 'Variant: game', 13, TXT, 500)
    s.text(x + w - 22, y + 55, '▾', 12, TXT2, anchor='end')
    s.rect(x + 8, y + 70, w - 16, 26, BG2, CONTROL, 5)
    s.text(x + 18, y + 88, 'Filter parts…', 13, TXT3)
    rows = [(0, 'Sample character', 0, 1, 0, 0, 0), (1, 'Body', 0, 0, 0, 0, 0), (1, 'Head', 1, 1, 1, 0, 0), (2, 'Face profile', 0, None, 0, 0, 0),
            (2, 'Eyes', 0, None, 0, 0, 0), (2, 'Brows', 0, None, 0, 1, 0), (2, 'Mouth', 0, None, 0, 0, 0), (1, 'Hair', 0, 1, 0, 0, 0),
            (2, 'Fringe', 0, None, 0, 1, 1), (2, 'Side locks', 0, None, 0, 0, 0), (2, 'Braid crown', 0, None, 0, 0, 0), (1, 'Outfit', 0, 0, 0, 0, 0), (1, 'Accessories', 0, 0, 0, 0, 0)]
    yy = y + 104
    for d, lab, sel, exp, lock, ag, wn in rows:
        if yy > y + h - 30: break
        sel_row = lab == 'Face profile'
        s.tree_row(x, yy, w, d, lab, selected=sel_row, expanded=(bool(exp) if exp is not None else None), lock=bool(lock), agent=bool(ag), warn=bool(wn))
        yy += 26


def library(s, x, y, w, h):
    s.panel(x, y, w, h, 'Library', tabs=['Library', 'Assets'], active=0)
    s.rect(x + 8, y + 36, w - 16, 26, BG2, CONTROL, 5)
    s.text(x + 18, y + 54, 'Search by text, tag or image…', 13, TXT3)
    cx = x + 8
    for i, t in enumerate(['Hair', 'Eyes', 'Outfit', 'Poses']):
        cw = 8 * len(t) + 22
        s.chip(cx, y + 70, t, ACC if i == 0 else TXT2, None, cw)
        cx += cw + 6
    gx, gy = x + 8, y + 100
    cell = (w - 16 - 8) // 2
    for i, nm in enumerate(['Hime cut', 'Twintails', 'Wolf cut', 'Bob']):
        cxx = gx + (i % 2) * (cell + 8); cyy = gy + (i // 2) * (cell * 0.8 + 26)
        if cyy + cell * 0.8 > y + h - 8: break
        s.rect(cxx, cyy, cell, cell * 0.8, BG2, LINE, 6)
        s.ellipse(cxx + cell / 2, cyy + cell * 0.4, cell * 0.22, cell * 0.26, '#B88FE0', None, op=0.8)
        s.text(cxx + 4, cyy + cell * 0.8 + 16, nm, 12, TXT2)


def inspector(s, x, y, w, h, compact=False):
    s.panel(x, y, w, h, 'Inspector')
    s.text(x + 12, y + 50, 'Face profile', 15, TXT, 600)
    s.text(x + 12, y + 68, 'Head › Face', 12, TXT3)
    s.actor(x + w - 150, y + 56, 'agent', 'Designer')
    s.text(x + w - 12, y + 72, '2m ago', 11, TXT3, anchor='end')
    s.rect(x + 8, y + 78, w - 16, 26, BG2, CONTROL, 5)
    s.text(x + 18, y + 96, 'Search settings…', 13, TXT3)
    yy = y + 112
    s.section(x, yy, w, 'Essentials'); yy += 32
    s.text(x + 12, yy + 18, 'Profile', 13, TXT2)
    sw = [34, 42, 56] if compact else [40, 52, 66]
    s.seg(x + int(w * 0.36), yy - 1, ['U', 'Egg', 'Mochi'], 1, wd=sw); yy += 34
    s.slider(x, yy, w, 'Width', 0.62, '0.62', modified=True, default_t=0.5, agent=True); yy += 30
    s.slider(x, yy, w, 'Chin', 0.31, '0.31', default_t=0.31); yy += 30
    s.slider(x, yy, w, 'Cheeks', 0.5, '0.50', default_t=0.5); yy += 30
    s.text(x + 12, yy + 18, 'Jaw taper', 13, TXT2)
    s.toggle(x + int(w * 0.36), yy + 3, True); s.text(x + int(w * 0.36) + 44, yy + 17, 'Follow profile', 12, TXT3); yy += 34
    s.section(x, yy, w, 'Advanced', open_=False, badge='6 more'); yy += 32
    s.section(x, yy, w, 'Variants'); yy += 32
    s.text(x + 12, yy + 14, 'game', 13, TXT2); s.text(x + 90, yy + 14, 'base values', 12, TXT3); yy += 22
    s.text(x + 12, yy + 14, 'close-up', 13, TXT2); s.text(x + 90, yy + 14, '3 overrides', 12, ACC)


def agent_panel(s, x, y, w, h):
    s.panel(x, y, w, h, '', tabs=['Agent', 'History'], active=0)
    yy = y + 38
    s.actor(x + 10, yy, 'agent', 'Designer (Claude)'); s.chip(x + w - 84, yy - 2, 'working', AGENT, None, 72); yy += 22
    s.text(x + 10, yy + 12, 'Plan: make the face more oval', 13, TXT, 500); yy += 20
    s.rect(x + 10, yy + 4, w - 20, 6, BG4, None, 3); s.rect(x + 10, yy + 4, (w - 20) * 4 / 9, 6, AGENT, None, 3)
    s.text(x + 10, yy + 26, 'Step 4 of 9 · Hair: adding fringe', 12, TXT3); yy += 36
    s.btn(x + 10, yy, 70, 'Pause'); s.btn(x + 86, yy, 60, 'Stop', 'ghost'); s.text(x + w - 12, yy + 19, '$0.12 / $2.00', 12, TXT3, anchor='end', mono=True); yy += 40
    s.line(x + 8, yy, x + w - 8, yy)
    # step cards
    for i, (lab, tm, ok) in enumerate([('Head › face profile 0.62', '4 s ago', 1), ('Hair › bangs 9 locks', 'now', 0)]):
        cy = yy + 8 + i * 58
        if cy + 52 > y + h - 74: break
        s.rect(x + 8, cy, w - 16, 52, BG2, LINE, 6)
        s.rect(x + 14, cy + 6, 40, 40, BG3, None, 4); s.ellipse(x + 34, cy + 26, 11, 13, '#FBE3D1', None)
        s.text(x + 62, cy + 22, lab, 13, TXT, 500)
        s.text(x + 62, cy + 40, 'likeness +0.03 · ' + tm, 12, TXT3)
    ay = y + h - 66
    s.rect(x + 8, ay, w - 16, 58, BG2, CONTROL, 6)
    s.text(x + 18, ay + 22, 'Ask or tell…', 13, TXT3)
    cw1 = s.chip(x + 14, ay + 32, '@ Face profile', TXT2, None, None, 20)
    s.chip(x + 14 + cw1 + 6, ay + 32, '+ reference', TXT2, None, None, 20)
    s.text(x + w - 18, ay + 48, '⏎', 14, TXT2, anchor='end')


def viewport(s, x, y, w, h, compact=False):
    s.rect(x, y, w, h, BG0, LINE, 6)
    s.rect(x + 1, y + 1, w - 2, h - 2, '#241F2D', None, 5, op=0.55)         # stage backdrop
    # top toolbar
    s.rect(x + 8, y + 8, w - 16, 34, BG1, LINE, 6, op=0.94)
    tx = x + 16
    for i, t in enumerate(['Select', 'Move', 'Guide', 'Pen']):
        ww = 8 * len(t) + 18
        if i == 0: s.rect(tx - 2, y + 12, ww + 4, 26, BG4, None, 4)
        s.text(tx + ww / 2, y + 30, t, 12.5, TXT if i == 0 else TXT2, 500, 'middle'); tx += ww + 4
    s.line(tx + 2, y + 16, tx + 2, y + 34); tx += 12
    for i, t in enumerate(['Front', '¾', 'Side', 'Back']):
        ww = 8 * len(t) + 18
        if i == 1: s.rect(tx - 2, y + 12, ww + 4, 26, BG4, None, 4)
        s.text(tx + ww / 2, y + 30, t, 12.5, TXT if i == 1 else TXT2, 500, 'middle'); tx += ww + 4
    if not compact:
        s.line(tx + 2, y + 16, tx + 2, y + 34); tx += 12
        for t in ['Compare ▾', 'Underlay ▾', 'Overlays ▾']:
            ww = 8 * len(t) + 14
            s.text(tx + ww / 2, y + 30, t, 12.5, TXT2, 500, 'middle'); tx += ww + 4
    s.text(x + w - 18, y + 30, 'Live preview · 60 fps', 12, TXT2, anchor='end', mono=True)
    # character + selection overlay
    cx, cy = x + w / 2, y + h * 0.5
    sc = min(w / 900, h / 640) * 1.15
    s.character(cx, cy, sc, glow=AGENT)
    # agent highlight on fringe, selection outline on face (two-tone: dark outer + colour inner)
    s.ellipse(cx, cy - 52 * sc, 100 * sc, 96 * sc, 'none', '#000000', 4, op=0.7)
    s.ellipse(cx, cy - 52 * sc, 100 * sc, 96 * sc, 'none', ACC, 2)
    s.rect(cx - 106 * sc, cy - 170 * sc, 212 * sc, 70 * sc, AGENT, AGENT, 14, 2, dash='6 4', op=0.18)
    # caption
    cap_w = 330
    s.rect(x + w / 2 - cap_w / 2, y + h - 74, cap_w, 30, BG1, AGENT, 15, 1.2, op=0.96)
    s.circle(x + w / 2 - cap_w / 2 + 16, y + h - 59, 5, AGENT)
    s.text(x + w / 2 - cap_w / 2 + 30, y + h - 54, 'Designer · Hair: adding fringe, 9 locks', 13, TXT, 500)
    # bottom strip
    s.rect(x + 8, y + h - 36, w - 16, 28, BG1, LINE, 6, op=0.94)
    s.text(x + 18, y + h - 17, '▴ Expressions', 12.5, TXT2, 500)
    s.text(x + w - 18, y + h - 17, 'Follow agent  ◉    Fit    Home', 12.5, TXT2, anchor='end')


def build_svg(w, h, compact, callouts=True, title=None):
    s = Svg(w, h, title or f'Build workspace at {w} by {h}')
    s.rect(0, 0, w, h, BG0)
    topbar(s, w, 'Build', compact)
    statusbar(s, w, h, compact)
    top, bot = 44, h - 24 - 4
    lw, rw = (240, 300) if compact else (300, 360)
    outliner_h = (bot - top) if compact else 560
    outliner(s, 4, top, lw - 4, (bot - top) if compact else outliner_h, tabbed=compact)
    if not compact: library(s, 4, top + outliner_h + 4, lw - 4, bot - top - outliner_h - 4)
    viewport(s, lw + 4, top, w - lw - rw - 8, bot - top, compact)
    ih = 470 if compact else 640
    inspector(s, w - rw, top, rw - 4, ih, compact)
    agent_panel(s, w - rw, top + ih + 4, rw - 4, bot - top - ih - 4)
    # callouts (placed in free header space, never over content)
    if compact:
        co = {1: (780, 20), 2: (1090, 38), 3: (1350, 38), 4: (120, 570), 5: (lw + 24, 100), 6: (w - 60, 57), 7: (w - 60, top + ih + 14), 8: (560, h - 12), 10: (lw + 460, 104)}
    else:
        co = {1: (1190, 20), 2: (1425, 40), 3: (1700, 40), 4: (250, 57), 5: (lw + 28, top + 70), 6: (w - 60, 57), 7: (w - 60, top + ih + 4 + 14), 8: (1100, h - 12), 9: (250, top + outliner_h + 18), 10: (1180, 58)}
    if callouts:
        for n, (cx_, cy_) in co.items():
            s.call(n, cx_, cy_)
    return s


def build(w, h, path, compact):
    build_svg(w, h, compact).save(path)




def palette(path):
    s = build_svg(1920, 1080, False, callouts=False, title='Command palette over the Build workspace at 1920 by 1080')
    s.rect(0, 0, 1920, 1080, BG0, op=0.62)
    x, y, w = 600, 96, 720
    h = 612
    s.rect(x - 2, y - 2, w + 4, h + 4, '#000000', None, 12, op=0.35)
    s.rect(x, y, w, h, BG2, LINE, 10)
    # input
    s.text(x + 18, y + 38, '>', 18, ACC, 700, mono=True)
    s.text(x + 44, y + 38, 'face prof', 18, TXT, 500)
    s.line(x + 44 + 82, y + 22, x + 44 + 82, y + 44, ACC, 2)
    s.text(x + w - 18, y + 36, 'Esc', 12, TXT3, anchor='end', mono=True)
    s.line(x, y + 56, x + w, y + 56)
    # mode chips
    cx = x + 16
    for t in ['> ops', '@ parts', '# expressions', '/ panels', '? help']:
        cw = 7.4 * len(t) + 16
        s.chip(cx, y + 66, t, TXT2 if t != '> ops' else ACC, None, cw, 22); cx += cw + 6
    s.text(x + w - 16, y + 82, '4 results · ↑↓ select · ⏎ run · Tab edit args', 12, TXT3, anchor='end')
    # rows
    rows = [
        ('op', 'face.set_profile', 'Set the head\'s face-shape profile', 'edit:character', 'Ctrl Alt F', True),
        ('op', 'face.blend_profiles', 'Blend two face profiles', 'edit:character', '', False),
        ('part', 'Head › Face profile', 'Select and open in the Inspector', '', '', False),
        ('lib', 'Face profiles › Egg oval', 'Library item · applies as an op', 'edit:library', '', False),
    ]
    ry = y + 100
    for kind, name, summ, scope, key, sel in rows:
        if sel:
            s.rect(x + 6, ry, w - 12, 44, BG4, None, 6); s.rect(x + 6, ry + 6, 3, 32, ACC)
        kc = {'op': ACC, 'part': TXT2, 'lib': TXT2}[kind]
        s.chip(x + 18, ry + 11, kind, kc, None, 46, 22)
        s.text(x + 80, ry + 19, name, 14, TXT, 600, mono=(kind == 'op'))
        s.text(x + 80, ry + 36, summ, 12, TXT3)
        if scope: s.chip(x + w - 18 - (7 * len(scope) + 16) - (86 if key else 0), ry + 11, scope, TXT3, None, 7 * len(scope) + 16, 22)
        if key: s.text(x + w - 18, ry + 27, key, 12, TXT3, anchor='end', mono=True)
        ry += 46
    # assistant row
    s.rect(x + 6, ry, w - 12, 40, 'none', AGENT, 6, 1, dash='4 3')
    s.circle(x + 28, ry + 20, 6, AGENT)
    s.text(x + 44, ry + 25, 'Ask the assistant: "face prof"', 14, TXT, 500)
    s.text(x + w - 18, ry + 25, 'Ctrl ⏎', 12, TXT3, anchor='end', mono=True)
    ry += 52
    # inline args form
    s.line(x, ry, x + w, ry)
    s.rect(x + 12, ry + 12, w - 24, 186, BG1, LINE, 8)
    s.text(x + 26, ry + 36, 'face.set_profile', 14, TXT, 600, mono=True)
    s.chip(x + w - 26 - 168, ry + 22, 'live preview in viewport', OK, None, 168, 22)
    s.text(x + 26, ry + 62, 'character', 13, TXT2); s.rect(x + 140, ry + 46, 200, 26, BG2, CONTROL, 5); s.text(x + 150, ry + 64, 'Sample character', 13, TXT)
    s.text(x + 26, ry + 96, 'profile', 13, TXT2); s.seg(x + 140, ry + 80, ['U', 'Egg oval', 'Mochi', 'Heart'], 1, wd=[40, 84, 64, 62])
    s.slider(x + 12, ry + 110, w - 24, 'blend', 0.62, '0.62', modified=True, default_t=1.0, lw=128)
    s.text(x + 26, ry + 160, 'Scope edit:character · Undo: automatic · Cost: instant · same as MCP tool face.set_profile', 12, TXT3)
    s.text(x + 26, ry + 180, 'Changes 1 value on Head › Face profile', 12, TXT2)
    ry += 208
    # footer actions
    s.btn(x + 14, ry + 4, 100, 'Run  ⏎', 'primary')
    s.btn(x + 122, ry + 4, 110, 'Dry run  ⇧⏎')
    s.btn(x + 240, ry + 4, 150, 'Copy as MCP call', 'ghost')
    s.btn(x + 394, ry + 4, 130, 'Copy as CLI', 'ghost')
    s.text(x + w - 16, ry + 23, 'Esc cancels the preview', 12, TXT3, anchor='end')
    # callouts
    for n, (cx_, cy_) in {1: (x - 24, y + 36), 2: (x - 24, y + 122), 3: (x - 24, y + 330), 4: (x - 24, y + h - 30)}.items():
        s.call(n, cx_, cy_)
    s.save(path)


def theatre(path):
    w, h = 1920, 1080
    s = Svg(w, h, 'Build Theatre at 1920 by 1080')
    topbar(s, w, 'Theatre', False, agent_text='Designer  4/9')
    statusbar(s, w, h)
    # control strip
    top = 44
    s.rect(4, top, w - 8, 48, BG1, LINE, 6)
    s.actor(16, top + 14, 'agent', 'Designer (Claude) ▾')
    s.btn(190, top + 10, 84, 'Pause')
    s.btn(280, top + 10, 70, 'Step', 'secondary')
    s.btn(356, top + 10, 90, 'Resume', 'ghost')
    s.text(470, top + 29, 'Pause:', 13, TXT3); s.rect(516, top + 10, 190, 28, BG2, CONTROL, 5); s.text(526, top + 29, 'after each step ▾', 13, TXT)
    s.btn(w - 780, top + 10, 150, 'Draw to direct', 'secondary')
    s.text(w - 600, top + 29, 'Budget', 13, TXT3)
    s.rect(w - 540, top + 20, 150, 8, BG4, None, 4); s.rect(w - 540, top + 20, 150 * 0.06, 8, AGENT, None, 4)
    s.text(w - 372, top + 29, '$0.12 / $2.00  ·  14k tok  ·  3:12', 12, TXT2, mono=True)
    s.btn(w - 92, top + 10, 80, 'Stop', 'danger')
    # plan board
    py = top + 56
    bot = h - 28
    lw = 380
    s.panel(4, py, lw, bot - py, 'Plan board')
    s.text(16, py + 50, 'Goal: make the face more oval, like the sheet', 13, TXT, 600)
    items = [('done', 'Read the locked design and sheet views', '2 steps'), ('done', 'Measure face outline and feature spacing', '3 steps'),
             ('done', 'Choose a face profile (Egg oval)', '1 step'), ('run', 'Adjust the fringe so brows stay visible', '4 steps · running'),
             ('todo', 'Match eye size and spacing to the sheet', ''), ('todo', 'Compare against the sheet at matched camera', ''),
             ('gate', 'Gate: likeness ≥ 0.85  (now 0.81)', 'failed · evidence in step 11'), ('todo', 'Run the expression range test', ''), ('todo', 'Write the character report card', '')]
    iy = py + 66
    for st, lab, meta in items:
        if st == 'run': s.rect(10, iy - 4, lw - 12, 50, BG3, AGENT, 6, 1.2)
        ix = 26
        if st == 'done': s.circle(ix, iy + 10, 8, OK); s.path(f'M{ix-4} {iy+10} l3 3 l5 -6', 'none', ON_ACC, 1.8)
        elif st == 'run': s.circle(ix, iy + 10, 8, 'none', AGENT, 2); s.circle(ix, iy + 10, 3.5, AGENT)
        elif st == 'gate': s.path(f'M{ix} {iy+1} l9 16 h-18 z', 'none', WARN, 2)
        else: s.circle(ix, iy + 10, 8, 'none', CONTROL, 1.5)
        s.text(44, iy + 15, lab, 13, TXT if st in ('run', 'gate') else TXT2, 600 if st == 'run' else 400)
        if meta: s.text(44, iy + 33, meta, 12, WARN if st == 'gate' else TXT3)
        iy += 50 if meta else 36
    # branches
    s.text(16, bot - 94, 'Branches', 13, TXT, 600)
    for i, (nm, sel) in enumerate([('main', 1), ('oval A', 0), ('oval B', 0)]):
        s.chip(16 + i * 92, bot - 80, nm, ACC if sel else TXT2, None, 82, 24)
    s.text(16, bot - 36, 'Agents explore variants in parallel; you pick one.', 12, TXT3)
    # viewport
    vx = lw + 8
    rw = 520
    vw = w - vx - rw - 8
    vh = 560
    s.rect(vx, py, vw, vh, BG0, LINE, 6)
    s.rect(vx + 1, py + 1, vw - 2, vh - 2, '#241F2D', None, 5, op=0.55)
    s.character(vx + vw / 2, py + vh * 0.5, 1.2, glow=AGENT)
    cx, cy, sc = vx + vw / 2, py + vh * 0.5, 1.2
    s.rect(cx - 106 * sc, cy - 170 * sc, 212 * sc, 70 * sc, AGENT, AGENT, 14, 2, dash='6 4', op=0.22)
    s.rect(vx + 14, py + 14, 300, 30, BG1, AGENT, 15, 1.2, op=0.96)
    s.circle(vx + 30, py + 29, 5, AGENT); s.text(vx + 44, py + 34, 'Hair: adding fringe, 9 locks', 13, TXT, 500)
    s.text(vx + vw - 16, py + 34, 'Following Designer ◉', 12.5, TXT2, anchor='end')
    # drawn note
    s.ellipse(cx + 120, cy - 40, 62, 40, 'none', DANGER, 3)
    s.rect(cx + 150, cy - 130, 230, 64, BG1, DANGER, 8, 1.2)
    s.text(cx + 162, cy - 106, 'make this rounder', 13, TXT, 500)
    s.text(cx + 162, cy - 86, 'sends screenshot + region', 12, TXT3)
    # compare strip
    cyy = py + vh + 8
    s.panel(vx, cyy, vw, bot - cyy, 'Compare · matched camera (¾, 50 mm)')
    tw = (vw - 16 - 24) / 3
    for i, (lab, tint) in enumerate([('Reference (sheet)', '#FBE3D1'), ('Previous step', '#F6D8C4'), ('Current', '#FBE3D1')]):
        tx = vx + 8 + i * (tw + 12)
        s.rect(tx, cyy + 36, tw, bot - cyy - 72, BG2, LINE, 6)
        s.ellipse(tx + tw / 2, cyy + 36 + (bot - cyy - 72) / 2, 54 + (6 if i == 1 else 0), 56, tint, None)
        s.ellipse(tx + tw / 2 - 17, cyy + 36 + (bot - cyy - 72) / 2 + 4, 6, 8, '#2B2140', None); s.ellipse(tx + tw / 2 + 17, cyy + 36 + (bot - cyy - 72) / 2 + 4, 6, 8, '#2B2140', None)
        s.text(tx + 6, bot - 14, lab, 12, TXT2)
    s.text(vx + vw - 40, cyy + 19, 'likeness 0.81 (+0.03)   spacing err 2.1%', 12, TXT2, anchor='end', mono=True)
    # right: approvals + timeline
    tx0 = w - rw - 4
    s.panel(tx0, py, rw, bot - py, 'Steps', tabs=['Steps', 'Log'], active=0)
    # approval card
    ay = py + 38
    s.rect(tx0 + 8, ay, rw - 16, 150, BG2, AGENT, 8, 1.5)
    s.circle(tx0 + 26, ay + 22, 6, AGENT)
    s.text(tx0 + 40, ay + 27, 'Designer needs you', 13, TXT, 600)
    s.chip(tx0 + rw - 24 - 110, ay + 12, 'render', TXT2, None, 110, 22)
    s.text(tx0 + 20, ay + 52, 'Render 24 contact sheets to check expressions', 13, TXT, 500)
    s.text(tx0 + 20, ay + 72, '~40 s · 1.2 GB GPU · $0 · scope render (in default grant)', 12, TXT3)
    s.text(tx0 + 20, ay + 92, 'Changes nothing in the character. Reversible: yes.', 12, TXT3)
    s.btn(tx0 + 16, ay + 108, 100, 'Approve', 'primary'); s.btn(tx0 + 124, ay + 108, 168, 'Approve for session'); s.btn(tx0 + 300, ay + 108, 90, 'Reject…', 'ghost')
    s.text(tx0 + rw - 22, ay + 127, '6 similar ▸', 12, TXT3, anchor='end')
    # step cards
    cards = [('11  Gate: likeness 0.81 < 0.85', 'failed gate', 'likeness −0.04  ·  spacing err 2.1%', True), ('10  Hair › bangs 9 locks', 'now', 'likeness +0.03  ·  budget 18%', False), ('9   Head › face profile = Egg oval', '4 s ago', 'likeness +0.06  ·  by Designer', False), ('8   Measure face outline', '41 s ago', 'query · no change', False)]
    cy2 = ay + 158
    for ci, (lab, tm, meta, bad) in enumerate(cards):
        ch = 104
        if cy2 + ch > bot - 8: break
        s.rect(tx0 + 8, cy2, rw - 16, ch, BG2, WARN if bad else LINE, 8, 1.5 if bad else 1)
        s.rect(tx0 + 16, cy2 + 10, 76, 76, BG3, None, 6); s.ellipse(tx0 + 54, cy2 + 48, 22, 26, '#F6D8C4', None)
        s.rect(tx0 + 98, cy2 + 10, 76, 76, BG3, None, 6); s.ellipse(tx0 + 136, cy2 + 48, 20, 27, '#FBE3D1', None)
        s.text(tx0 + 56, cy2 + 100, 'before', 11, TXT3, anchor='middle'); s.text(tx0 + 136, cy2 + 100, 'after', 11, TXT3, anchor='middle')
        if bad: s.path(f'M{tx0+190} {cy2+22} l8 14 h-16 z', 'none', WARN, 2)
        s.text(tx0 + 206 if bad else tx0 + 188, cy2 + 32, lab, 13, TXT, 600)
        s.text(tx0 + 188, cy2 + 54, meta, 12, TXT2)
        s.text(tx0 + 188, cy2 + 74, tm, 12, TXT3)
        s.text(tx0 + rw - 22, cy2 + 74, 'Explain', 12, AGENT, 500, 'end')
        s.text(tx0 + rw - 76, cy2 + 74, 'Rewind', 12, TXT2, 500, 'end')
        if ci < 2: s.text(tx0 + rw - 22, cy2 + 94, '✓ Approve   ✕ Reject', 12, TXT2, anchor='end')
        cy2 += ch + 8
    for n, (cx_, cy_) in {1: (lw - 14, py + 14), 2: (vx + vw - 220, py + 14), 3: (vx + vw - 330, cyy + 14), 4: (tx0 + rw - 56, py + 14), 5: (tx0 + 8, ay + 2), 6: (1116, top + 24), 7: (200, bot - 100)}.items():
        s.call(n, cx_, cy_)
    s.save(path)


def perform(path):
    w, h = 1366, 768
    s = Svg(w, h, 'Perform workspace at 1366 by 768')
    topbar(s, w, 'Perform', True, agent_text='Assistant idle', live=True)
    statusbar(s, w, h, True, 'Webcam · face found', 'Undo: Expression Smug on hotkey F2', 'Output: Spout → OBS')
    top, bot = 44, h - 28
    lw, rw = 250, 330
    # inputs panel
    s.panel(4, top, lw - 4, bot - top, 'Inputs', tabs=['Inputs', 'Hotkeys'], active=0)
    y = top + 38
    def card(y, title, status, kind, lines, h=92):
        s.rect(12, y, lw - 20, h, BG2, LINE, 8)
        s.text(24, y + 24, title, 13, TXT, 600)
        col = {'ok': OK, 'off': TXT3, 'warn': WARN}[kind]
        s.circle(lw - 26, y + 20, 5, col)
        s.text(24, y + 44, status, 12.5, col if kind != 'off' else TXT3, 500)
        for i, l in enumerate(lines): s.text(24, y + 62 + 16 * i, l, 12, TXT3)
    card(y, 'Webcam', '● Face found · 60 fps', 'ok', ['Logi C920 · 12 ms tracking', 'Low light: fine'], 96)
    s.btn(lw - 98, y + 66, 70, 'Change', 'ghost', 22)
    card(y + 104, 'iPhone', 'Not paired', 'off', ['Same Wi-Fi as this PC'], 96)
    s.btn(24, y + 104 + 70, 90, 'Pair phone', 'secondary', 22)
    card(y + 208, 'VMC / OSC in', 'Off', 'off', ['port 39539'], 80)
    card(y + 296, 'Microphone', 'Level ok · lip sync on', 'ok', [], 74)
    s.rect(24, y + 296 + 54, 180, 6, BG4, None, 3); s.rect(24, y + 296 + 54, 104, 6, OK, None, 3)
    s.rect(12, y + 378, lw - 20, 108, BG2, LINE, 8)
    s.text(24, y + 402, 'Calibration', 13, TXT, 600)
    s.text(24, y + 422, 'Done 2 days ago for this webcam', 12, TXT3)
    s.btn(24, y + 438, 160, 'Recalibrate · 40 s', 'primary', 28)
    # viewport
    vx = lw + 4
    vw = w - lw - rw - 8
    s.rect(vx, top, vw, bot - top, BG0, LINE, 6)
    s.rect(vx + 1, top + 1, vw - 2, bot - top - 2, '#241F2D', None, 5, op=0.55)
    s.rect(vx + 8, top + 8, vw - 16, 34, BG1, LINE, 6, op=0.94)
    s.text(vx + 18, top + 30, 'Mirror ◉   Live mode', 12.5, TXT2, 500)
    s.text(vx + vw - 18, top + 30, 'PROGRAM · output to OBS (Spout)', 12.5, DANGER, 600, anchor='end')
    s.character(vx + vw / 2, top + (bot - top) * 0.48, 0.95)
    # camera PiP
    px, py_, pw, ph = vx + vw - 196, bot - 190, 184, 140
    s.rect(px, py_, pw, ph, '#0A090C', CONTROL, 8)
    s.ellipse(px + pw / 2, py_ + ph / 2, 36, 46, '#6B5A55', None, op=0.9)
    for dx, dy in [(-14, -8), (14, -8), (0, 6), (-8, 20), (8, 20), (0, -26), (-22, -18), (22, -18)]:
        s.circle(px + pw / 2 + dx, py_ + ph / 2 + dy, 2.2, OK)
    s.text(px + 8, py_ + 16, 'camera (mirrored)', 11, TXT2)
    # expression strip
    ey = bot - 52
    s.rect(vx + 8, ey, vw - 216, 44, BG1, LINE, 6, op=0.94)
    ex = vx + 18
    for i, (nm, key) in enumerate([('Happy', 'F1'), ('Smug', 'F2'), ('Pout', 'F3'), ('Sad', 'F4'), ('Shy', 'F5')]):
        cw = 8 * len(nm) + 54
        s.rect(ex, ey + 8, cw, 28, BG3, CONTROL, 5)
        s.text(ex + 10, ey + 27, nm, 13, TXT, 500)
        s.text(ex + cw - 10, ey + 27, key, 11, TXT3, anchor='end', mono=True)
        ex += cw + 6
    # right: channels
    cx0 = w - rw
    s.panel(cx0, top, rw - 4, 470, 'Channels', tabs=['Face', 'Head', 'Body'], active=0)
    s.slider(cx0, top + 36, rw - 4, 'Smoothing', 0.4, '0.40', modified=False, default_t=0.4, lw=96)
    s.text(cx0 + 12, top + 74, 'Costs about +8 ms latency', 12, TXT3)
    s.text(cx0 + 12, top + 98, 'Channel', 12, TXT3); s.text(cx0 + rw - 16, top + 98, 'raw ▏ processed', 12, TXT3, anchor='end')
    chans = [('Blink L', 0.1, 0.05, 'T'), ('Blink R', 0.1, 0.05, 'T'), ('Jaw open', 0.45, 0.52, 'T'), ('Smile L', 0.62, 0.6, 'T'), ('Smile R', 0.58, 0.6, 'T'), ('Brow up', 0.2, 0.2, 'T'),
             ('Cheek puff', 0.0, 0.0, 'L'), ('Tongue out', 0.0, 0.0, 'H'), ('Eye look L/R', 0.5, 0.5, 'T')]
    my = top + 108
    for nm, raw, proc, own in chans:
        s.text(cx0 + 12, my + 15, nm, 12.5, TXT2)
        s.rect(cx0 + 12 + 96, my + 6, 120, 10, BG4, None, 5)
        s.rect(cx0 + 12 + 96, my + 6, 120 * proc, 10, ACC, None, 5)
        s.line(cx0 + 12 + 96 + 120 * raw, my + 3, cx0 + 12 + 96 + 120 * raw, my + 19, TXT, 1.5)
        s.chip(cx0 + rw - 12 - 56, my + 1, {'T': 'tracker', 'L': 'life', 'H': 'hotkey'}[own], TXT3, None, 56, 18)
        my += 28
    s.text(cx0 + 12, my + 14, '+ 43 more · search channels…', 12.5, TXT3)
    # health + life layer
    s.panel(cx0, top + 474, rw - 4, bot - top - 474, 'Health')
    s.text(cx0 + 12, top + 474 + 54, '60 fps · 38 ms · budget 31%', 12.5, TXT2, mono=True)
    s.text(cx0 + 12, top + 474 + 78, 'Life layer', 13, TXT2); s.toggle(cx0 + 110, top + 474 + 64, True)
    s.rect(cx0 + 160, top + 474 + 62, 140, 26, BG2, CONTROL, 5); s.text(cx0 + 170, top + 474 + 80, 'Calm ▾', 13, TXT)
    for n, (cx_, cy_) in {1: (lw - 24, top + 14), 2: (lw + 4 + 30, top + 60), 3: (vx + vw - 220, bot - 200), 4: (vx + 30, bot - 66), 5: (cx0 + rw - 40, top + 14), 6: (cx0 + rw - 40, top + 474 + 14)}.items():
        s.call(n, cx_, cy_)
    s.save(path)


def firstrun(path):
    w, h = 1366, 768
    s = Svg(w, h, 'First-run setup, step 2 of 5, at 1366 by 768')
    s.rect(0, 0, w, 40, BG2); s.line(0, 40, w, 40, LINE)
    s.rect(8, 6, 28, 28, BG3, None, 4); s.text(22, 25, '≡', 16, TXT, 500, 'middle'); s.text(46, 25, 'Omuzaki', 15, TXT, 700)
    s.text(w / 2, 25, 'Setup · 2 of 5', 13, TXT2, 500, 'middle')
    s.text(w - 120, 25, 'Skip setup', 13, TXT2, 500, 'end'); s.text(w - 12, 25, 'Esc', 12, TXT3, anchor='end', mono=True)
    top, bot = 44, h - 28
    cw = 420
    # viewport live behind
    vx = cw + 8
    s.rect(vx, top, w - vx - 4, bot - top, BG0, LINE, 6)
    s.rect(vx + 1, top + 1, w - vx - 6, bot - top - 2, '#241F2D', None, 5, op=0.55)
    s.character(vx + (w - vx) / 2, top + (bot - top) * 0.46, 1.05, glow=ACC)
    s.rect(vx + 14, top + 14, 280, 30, BG1, OK, 15, 1.2)
    s.circle(vx + 30, top + 29, 5, OK); s.text(vx + 44, top + 34, 'Mirroring your face · 60 fps', 13, TXT, 500)
    # PiP
    px, py_, pw, ph = w - 4 - 200, bot - 160, 188, 146
    s.rect(px, py_, pw, ph, '#0A090C', CONTROL, 8)
    s.ellipse(px + pw / 2, py_ + ph / 2, 38, 48, '#6B5A55', None, op=0.9)
    for dx, dy in [(-14, -8), (14, -8), (0, 6), (-8, 20), (8, 20), (0, -26)]:
        s.circle(px + pw / 2 + dx, py_ + ph / 2 + dy, 2.2, OK)
    s.text(px + 8, py_ + 16, 'your camera (mirrored)', 11, TXT2)
    # wizard card as dock panel
    s.rect(4, top, cw, bot - top, BG1, LINE, 6)
    # progress rail
    labels = ['Character', 'Input', 'Calibrate', 'Output', 'Go live']
    rx = 40
    for i, l in enumerate(labels):
        cxp = rx + i * 80
        if i < 1: s.circle(cxp + 8, top + 28, 8, OK); s.path(f'M{cxp+4} {top+28} l3 3 l5 -6', 'none', ON_ACC, 1.8)
        elif i == 1: s.circle(cxp + 8, top + 28, 8, ACC); s.text(cxp + 8, top + 33, '2', 11, ON_ACC, 700, 'middle')
        else: s.circle(cxp + 8, top + 28, 8, 'none', CONTROL, 1.5); s.text(cxp + 8, top + 33, str(i + 1), 11, TXT3, 600, 'middle')
        s.text(cxp + 8, top + 52, l, 11.5, TXT if i == 1 else TXT3, 600 if i == 1 else 400, 'middle')
        if i < 4: s.line(cxp + 20, top + 28, cxp + 68, top + 28, OK if i < 1 else LINE, 2)
    s.text(24, top + 96, 'How will you track your face?', 22, TXT, 700)
    s.text(24, top + 120, 'Your camera feed stays on this computer.', 13, TXT2)
    # options
    oy = top + 144
    opts = [('Webcam', 'Ready now. Good for most streams.', 'Face found · Logi C920', True), ('iPhone', 'Best mouth and tongue detail. Free tracker app.', 'Pair in about a minute', False), ('Skip for now', 'The avatar will idle with life animation.', '', False)]
    for i, (t, d, st, sel) in enumerate(opts):
        hh = 92 if i < 2 else 64
        s.rect(16, oy, cw - 24, hh, BG2, ACC if sel else LINE, 8, 2 if sel else 1)
        s.circle(40, oy + 26, 9, 'none', ACC if sel else CONTROL, 2)
        if sel: s.circle(40, oy + 26, 4.5, ACC)
        s.text(60, oy + 31, t, 15, TXT, 600)
        s.text(60, oy + 52, d, 12.5, TXT2)
        if st: s.text(60, oy + 74, ('●  ' if sel else '') + st, 12.5, OK if sel else TXT3, 500)
        oy += hh + 8
    # live meter row
    s.text(24, oy + 22, 'Quick check', 13, TXT, 600)
    s.rect(24, oy + 34, 372, 8, BG4, None, 4); s.rect(24, oy + 34, 250, 8, OK, None, 4)
    s.text(24, oy + 60, 'Blink, open your mouth, smile: the avatar should follow.', 12.5, TXT3)
    # footer
    fy = bot - 56
    s.line(4, fy, cw + 4, fy)
    s.btn(20, fy + 14, 80, 'Back', 'ghost')
    s.btn(cw - 150, fy + 14, 130, 'Continue  ⏎', 'primary')
    s.text(cw / 2 - 40, fy + 33, 'About 20 s', 12, TXT3, anchor='middle')
    statusbar_simple(s, w, h)
    for n, (cx_, cy_) in {1: (cw - 24, top + 28), 2: (cw - 40, top + 160), 3: (vx + 360, top + 29), 4: (cw - 168, bot - 74), 5: (w - 76, 20)}.items():
        s.call(n, cx_, cy_)
    s.save(path)


def statusbar_simple(s, w, h):
    y = h - 24
    s.rect(0, y, w, 24, BG2); s.line(0, y, w, y, LINE)
    s.text(12, y + 16, 'Sample project · not saved yet · Setup can be reopened from Help', 12, TXT2)
    s.text(w - 12, y + 16, '60 fps  ·  GPU 0.9 ms', 12, TXT2, anchor='end', mono=True)


if __name__ == '__main__':
    import sys
    out = sys.argv[1] if len(sys.argv) > 1 else '.'
    build(1920, 1080, f'{out}/build-1920.svg', False)
    build(1366, 768, f'{out}/build-1366.svg', True)
    palette(f'{out}/palette-1920.svg')
    theatre(f'{out}/theatre-1920.svg')
    perform(f'{out}/perform-1366.svg')
    firstrun(f'{out}/firstrun-1366.svg')
