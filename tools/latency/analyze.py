#!/usr/bin/env python3
"""Analysis tool for the V0 glass-to-glass latency harness (#1967).

Reads one screen recording, decoded to raw 8-bit grey frames, that shows both the stimulus window (`stimulus.py`) and
Omuzaki's output window. For each stimulus mouth-open frame it finds the first output frame where the avatar's jaw
signal crosses half way and reports the delays: p50, p95 and max. Standard library only.

    ffmpeg -i rec.mkv -pix_fmt gray -f rawvideo - | python3 tools/latency/analyze.py - \\
        --width 1920 --height 1080 --fps 60 --layout stimulus/layout.json \\
        --stim-origin 100,100 --out-roi 1200,300,64,64

The output signal is a region of the output window whose mean grey level follows `jawOpen` (a marker patch, bright when
open) or the avatar's mouth pixels (`--out-polarity dark`). The crossing is interpolated between the two frames around
it, so the result is finer than one frame; the stimulus side is exact to one recording frame.
"""
import argparse
import json
import math
import sys

MIN_EVENTS = 50
MAX_LATENCY_MS = 600.0


def roi_mean(frame, width, roi):
    """Mean grey level of `roi = (x, y, w, h)` in a frame of `width` pixels per row."""
    x, y, w, h = roi
    total = 0
    for r in range(y, y + h):
        o = r * width + x
        total += sum(frame[o:o + w])
    return total / (w * h)


def read_frames(stream, width, height):
    """Yield raw grey frames of `width * height` bytes until the stream ends; a truncated last frame is an error."""
    size = width * height
    while True:
        buf = stream.read(size)
        if not buf:
            return
        while len(buf) < size:
            more = stream.read(size - len(buf))
            if not more:
                raise ValueError(f"recording ends in the middle of a frame ({len(buf)} of {size} bytes)")
            buf += more
        yield buf


def counter_value(frame, width, origin, layout):
    """Clip frame number burned into a recording frame, read from the middle row of each counter cell."""
    cx, cy, cw, ch = layout["counter_roi"]
    bits = layout["counter_bits"]
    cell = cw // bits
    value = 0
    for b in range(bits):
        mean = roi_mean(frame, width, (origin[0] + cx + b * cell, origin[1] + cy + ch // 2, cell, 1))
        value = (value << 1) | (1 if mean > 127.5 else 0)
    return value


def percentile(values, p):
    """Nearest-rank percentile (`p` in 0..100) of a non-empty list."""
    ordered = sorted(values)
    rank = max(1, math.ceil(p / 100 * len(ordered)))
    return ordered[rank - 1]


def oriented(signal, bright_when_open):
    """The signal flipped so that "open" is always the high side."""
    return signal if bright_when_open else [255.0 - s for s in signal]


def calibrate(signal, fixed_range=None):
    """Half-way level between the closed and open levels: the observed 2nd and 98th percentiles unless fixed.

    `fixed_range` is `(closed, open)` in the signal's own orientation, so closed is below open."""
    lo, hi = fixed_range if fixed_range else (percentile(signal, 2), percentile(signal, 98))
    if hi - lo < 16:
        raise ValueError(f"signal does not move (levels {lo:.1f} to {hi:.1f}); check the region and the polarity")
    return (lo + hi) / 2


def rising_crossings(signal, threshold):
    """Frame-fraction positions where the signal rises through `threshold`, interpolated between the two frames."""
    found = []
    for i in range(1, len(signal)):
        a, b = signal[i - 1], signal[i]
        if a < threshold <= b:
            found.append(i - 1 + (threshold - a) / (b - a))
    return found


def stimulus_events(stim, counters, threshold, open_frames):
    """Recording frames where the mouth opens and the burned-in counter names a scheduled open frame, and the rest."""
    scheduled = set(open_frames)
    good, rejected = [], 0
    for i in range(1, len(stim)):
        if stim[i - 1] < threshold <= stim[i]:
            if counters[i] in scheduled:
                good.append(i)
            else:
                rejected += 1
    return good, rejected


def pair(events, crossings, fps, max_latency_ms):
    """One entry per event: the delay in ms of its first output crossing within `max_latency_ms`, or `None`."""
    results, j = [], 0
    for e in events:
        while j < len(crossings) and crossings[j] < e:
            j += 1
        if j < len(crossings) and (crossings[j] - e) * 1000 / fps <= max_latency_ms:
            results.append((crossings[j] - e) * 1000 / fps)
            j += 1
        else:
            results.append(None)
    return results


def summarise(delays):
    """p50, p95 and max of the delays in ms."""
    return {"events": len(delays), "p50_ms": percentile(delays, 50), "p95_ms": percentile(delays, 95),
            "max_ms": max(delays)}


def analyse(frames, width, fps, layout, stim_origin, out_roi, out_bright=True, out_range=None,
            max_latency_ms=MAX_LATENCY_MS):
    """Latency report for an iterable of raw grey frames; raises `ValueError` if the recording can't be analysed.

    Events with no response within `max_latency_ms` count as that whole window in the percentiles."""
    mouth = layout["mouth_roi"]
    mouth_roi = (stim_origin[0] + mouth[0], stim_origin[1] + mouth[1], mouth[2], mouth[3])
    stim, out, counters = [], [], []
    for frame in frames:
        stim.append(roi_mean(frame, width, mouth_roi))
        out.append(roi_mean(frame, width, out_roi))
        counters.append(counter_value(frame, width, stim_origin, layout))
    if not stim:
        raise ValueError("the recording has no frames")
    stim = oriented(stim, layout["mouth_open_luma"] == "bright")
    out = oriented(out, out_bright)
    if out_range:
        # The flip puts a dark signal's levels in the samples' orientation; sorting accepts either order.
        flipped = out_range if out_bright else (255 - out_range[0], 255 - out_range[1])
        out_range = tuple(sorted(flipped))
    events, rejected = stimulus_events(stim, counters, calibrate(stim), layout["open_frames"])
    crossings = rising_crossings(out, calibrate(out, out_range))
    results = pair(events, crossings, fps, max_latency_ms)
    delays = [d for d in results if d is not None]
    missed = len(results) - len(delays)
    if not delays or len(events) < MIN_EVENTS:
        raise ValueError(f"{len(events)} usable stimulus events ({rejected} rejected by the counter), {len(delays)} "
                         f"with an output crossing; at least {MIN_EVENTS} events and one response are needed")
    # An event with no response in the window is a failure, not a gap: it counts as the whole window, so it pushes the
    # percentiles up instead of dropping out of them.
    report = summarise(delays + [max_latency_ms] * missed)
    report.update({"frames": len(stim), "stimulus_events": len(events), "rejected_by_counter": rejected,
                   "missed": missed, "window_ms": max_latency_ms,
                   "event_results": [
                       {"frame": e, "status": "answered" if d is not None else "timed_out",
                        "delay_ms": None if d is None else round(d, 2)} for e, d in zip(events, results)]})
    return report


def parse_ints(text, n):
    """`"1,2,3"` as a tuple of `n` integers."""
    parts = [int(p) for p in text.split(",")]
    if len(parts) != n:
        raise argparse.ArgumentTypeError(f"expected {n} comma-separated integers, got {text!r}")
    return tuple(parts)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("raw", help="raw grey frames file, or - for standard input")
    ap.add_argument("--width", type=int, required=True)
    ap.add_argument("--height", type=int, required=True)
    ap.add_argument("--fps", type=float, required=True, help="frame rate of the recording")
    ap.add_argument("--layout", required=True, help="layout.json written by stimulus.py")
    ap.add_argument("--stim-origin", type=lambda t: parse_ints(t, 2), required=True,
                    help="X,Y of the stimulus window's top-left pixel in the recording (shown at 1:1)")
    ap.add_argument("--out-roi", type=lambda t: parse_ints(t, 4), required=True, help="X,Y,W,H of the output signal")
    ap.add_argument("--out-polarity", choices=("bright", "dark"), default="bright",
                    help="whether the output signal is bright (a jawOpen marker patch) or dark when the mouth is open")
    ap.add_argument("--out-range", type=lambda t: parse_ints(t, 2), default=None,
                    help="CLOSED,OPEN grey levels of the signal at jawOpen 0 and 1, as it appears in the recording "
                         "(for --out-polarity dark, CLOSED is above OPEN); default: the observed 2nd and 98th percentile")
    ap.add_argument("--max-latency-ms", type=float, default=MAX_LATENCY_MS)
    ap.add_argument("--budget-ms", type=float, default=None, help="exit 1 if p95 is above this (V0: 100)")
    ap.add_argument("--json", action="store_true", help="print the report as JSON, with every event's frame, status and delay")
    args = ap.parse_args(argv)
    with open(args.layout) as f:
        layout = json.load(f)
    stream = sys.stdin.buffer if args.raw == "-" else open(args.raw, "rb")
    try:
        report = analyse(read_frames(stream, args.width, args.height), args.width, args.fps, layout,
                         args.stim_origin, args.out_roi, args.out_polarity == "bright", args.out_range,
                         args.max_latency_ms)
    except ValueError as e:
        print(f"analyze: {e}", file=sys.stderr)
        return 2
    finally:
        if stream is not sys.stdin.buffer:
            stream.close()
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"events {report['events']} (no response within the window: {report['missed']}, rejected by counter "
              f"{report['rejected_by_counter']}) over {report['frames']} frames at {args.fps:g} fps")
        print(f"glass to glass: p50 {report['p50_ms']:.1f} ms, p95 {report['p95_ms']:.1f} ms, "
              f"max {report['max_ms']:.1f} ms")
    if args.budget_ms is not None and report["p95_ms"] > args.budget_ms:
        print(f"analyze: p95 {report['p95_ms']:.1f} ms is above the {args.budget_ms:g} ms budget", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
