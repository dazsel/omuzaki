"""Synthetic recording with a known injected delay, for the analysis tests of the latency harness (#1967).

Builds, at run time and in memory, what a screen recording of the real procedure would hold: the stimulus window
(`stimulus.py`, shown 1:1) and an output marker patch whose grey level is `jawOpen`. The avatar's jaw opens
`delay + jitter` after each stimulus mouth-open frame and takes `RAMP_MS` to open. The truth is returned alongside, so
a test compares the analysis to it. Deterministic: the jitter and the sensor noise come from a fixed-seed generator.
"""
import stimulus

WIDTH = 480
HEIGHT = 340
STIM_ORIGIN = (20, 10)
OUT_ROI = (380, 100, 64, 64)
RAMP_MS = 40.0
NOISE = 3


class Lcg:
    """A small deterministic generator (numerical recipes constants), so no run depends on library versions."""

    def __init__(self, seed):
        self.state = seed

    def next_unit(self):
        """Next value in [0, 1)."""
        self.state = (self.state * 1664525 + 1013904223) % (1 << 32)
        return self.state / (1 << 32)


def jaw_open(t_ms, onsets_ms, ramp_ms=RAMP_MS, open_ms=750.0):
    """`jawOpen` at time `t_ms` given the times each opening starts: a linear ramp up, held, a linear ramp down."""
    for start in onsets_ms:
        if start <= t_ms < start + ramp_ms:
            return (t_ms - start) / ramp_ms
        if start + ramp_ms <= t_ms < start + open_ms:
            return 1.0
        if start + open_ms <= t_ms < start + open_ms + ramp_ms:
            return 1.0 - (t_ms - start - open_ms) / ramp_ms
    return 0.0


def build(delay_ms, jitter_ms, fps=60, events=60, seed=7, slow_every=0, slow_ms=0.0):
    """`(frames, truth_ms)`: an iterator of recording frames (built lazily: a full list is about a gigabyte) and the
    true delay in ms of each event.

    Every `slow_every`-th event (if set) responds after `slow_ms` instead.
    The truth of an event is the time `jawOpen` crosses 0.5 minus the time the recording shows the mouth open.
    """
    rng = Lcg(seed)
    opens = stimulus.schedule(events)
    delays = [delay_ms + (rng.next_unit() * 2 - 1) * jitter_ms for _ in opens]
    for k in range(slow_every - 1, events, slow_every) if slow_every else ():
        delays[k] = slow_ms
    onsets = [o * 1000 / fps + d - RAMP_MS / 2 for o, d in zip(opens, delays)]
    return _frames(fps, events, onsets, rng), delays


def _frames(fps, events, onsets, rng):
    """Recording frames for `build`."""
    faces = (stimulus.base_face(False), stimulus.base_face(True))
    sw, sh = stimulus.WIDTH, stimulus.HEIGHT
    ox, oy = STIM_ORIGIN
    px, py, pw, ph = OUT_ROI
    for n in range(stimulus.clip_length(events) + int(fps)):
        frame = bytearray([128]) * (WIDTH * HEIGHT)
        clip = stimulus.render_frame(min(n, stimulus.clip_length(events) - 1), faces)
        for r in range(sh):
            frame[(oy + r) * WIDTH + ox:(oy + r) * WIDTH + ox + sw] = clip[r * sw:(r + 1) * sw]
        level = round(255 * jaw_open(n * 1000 / fps, onsets))
        row = bytes(min(255, max(0, level + round((rng.next_unit() * 2 - 1) * NOISE))) for _ in range(pw))
        for r in range(py, py + ph):
            frame[r * WIDTH + px:r * WIDTH + px + pw] = row
        yield bytes(frame)
