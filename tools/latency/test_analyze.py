"""Tests for the latency harness analysis (#1967): synthetic recordings with a known injected delay.

Run: python3 -m unittest discover -s tools/latency -p 'test_*.py'
"""
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyze  # noqa: E402
import stimulus  # noqa: E402
import synth  # noqa: E402

FPS = 60
FRAME_MS = 1000 / FPS
LAYOUT = stimulus.layout(FPS, 60)


def invert_output(frame):
    """The recording frame with the output region's grey levels inverted (a dark-when-open signal)."""
    px, py, pw, ph = synth.OUT_ROI
    g = bytearray(frame)
    for r in range(py, py + ph):
        o = r * synth.WIDTH + px
        g[o:o + pw] = bytes(255 - v for v in g[o:o + pw])
    return bytes(g)


def run(frames, **kw):
    return analyze.analyse(frames, synth.WIDTH, FPS, LAYOUT, synth.STIM_ORIGIN, synth.OUT_ROI, **kw)


class RecoversInjectedDelay(unittest.TestCase):
    def test_83ms_with_jitter_p95_within_one_frame(self):
        frames, truth = synth.build(83.0, 12.0)
        report = run(frames)
        self.assertEqual(report["events"], 60)
        true_p95 = analyze.percentile(truth, 95)
        true_p50 = analyze.percentile(truth, 50)
        print(f"\n83 ms +/- 12 ms jitter: truth p50 {true_p50:.1f} p95 {true_p95:.1f} max {max(truth):.1f}; "
              f"reported p50 {report['p50_ms']:.1f} p95 {report['p95_ms']:.1f} max {report['max_ms']:.1f}")
        self.assertLessEqual(abs(report["p95_ms"] - true_p95), FRAME_MS)
        self.assertLessEqual(abs(report["p50_ms"] - true_p50), FRAME_MS)
        self.assertLessEqual(abs(report["max_ms"] - max(truth)), FRAME_MS)

    def test_other_delays_are_recovered(self):
        for delay in (33.0, 120.0, 250.0):
            with self.subTest(delay=delay):
                frames, truth = synth.build(delay, 0.0)
                report = run(frames)
                self.assertAlmostEqual(report["p50_ms"], delay, delta=FRAME_MS / 2)

    def test_p95_follows_the_delay_not_a_constant(self):
        low = run(synth.build(60.0, 5.0)[0])["p95_ms"]
        high = run(synth.build(140.0, 5.0)[0])["p95_ms"]
        self.assertGreater(high - low, 70.0)

    def test_dark_output_polarity(self):
        frames, truth = synth.build(83.0, 0.0)
        report = run((invert_output(f) for f in frames), out_bright=False)
        self.assertAlmostEqual(report["p50_ms"], 83.0, delta=FRAME_MS / 2)


class FixedRange(unittest.TestCase):
    def test_asymmetric_range_moves_the_crossing_the_same_way_in_both_polarities(self):
        # Closed at level 0 and open at level 200 puts half way at 100, a jaw of 0.39, earlier than the 0.5 default.
        bright = run(synth.build(83.0, 0.0)[0], out_range=(0, 200))
        frames, _ = synth.build(83.0, 0.0)
        dark = run((invert_output(f) for f in frames), out_bright=False, out_range=(255, 55))
        self.assertAlmostEqual(bright["p50_ms"], 83.0 - 0.108 * synth.RAMP_MS, delta=1.0)
        self.assertAlmostEqual(dark["p50_ms"], bright["p50_ms"], delta=0.1)
        default = run(synth.build(83.0, 0.0)[0])
        self.assertGreater(default["p50_ms"] - bright["p50_ms"], 3.0)

    def test_range_that_does_not_open_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "does not move"):
            run(synth.build(83.0, 0.0)[0], out_range=(100, 100))


class RejectsBadRecordings(unittest.TestCase):
    def test_flat_output_is_an_error(self):
        frames, _ = synth.build(83.0, 0.0)
        px, py, pw, ph = synth.OUT_ROI
        def flatten(f):
            g = bytearray(f)
            for r in range(py, py + ph):
                g[r * synth.WIDTH + px:r * synth.WIDTH + px + pw] = bytes([40]) * pw
            return bytes(g)
        with self.assertRaisesRegex(ValueError, "does not move"):
            run(flatten(f) for f in frames)

    def test_too_few_events_is_an_error(self):
        frames, _ = synth.build(83.0, 0.0, events=20)
        layout = stimulus.layout(FPS, 20)
        with self.assertRaisesRegex(ValueError, "at least 50"):
            analyze.analyse(frames, synth.WIDTH, FPS, layout, synth.STIM_ORIGIN, synth.OUT_ROI)

    def test_no_response_at_all_is_an_error(self):
        frames, _ = synth.build(700.0, 0.0)
        with self.assertRaisesRegex(ValueError, "one response"):
            run(frames)

    def test_slow_responses_count_as_the_whole_window(self):
        # Fifty events at 83 ms and ten that respond after 700 ms: the ten must fail the p95, not drop out of it.
        frames, truth = synth.build(83.0, 0.0, slow_every=6, slow_ms=700.0)
        report = run(frames)
        self.assertEqual(sum(1 for t in truth if t > 600), 10)
        self.assertEqual(report["missed"], 10)
        self.assertEqual(report["events"], 60)
        self.assertEqual(report["p95_ms"], analyze.MAX_LATENCY_MS)
        self.assertAlmostEqual(report["p50_ms"], 83.0, delta=FRAME_MS / 2)

    def test_wrong_counter_rejects_the_event(self):
        frames, _ = synth.build(83.0, 0.0)
        shifted = dict(LAYOUT, open_frames=[n + 1 for n in LAYOUT["open_frames"]])
        with self.assertRaisesRegex(ValueError, "rejected by the counter"):
            analyze.analyse(frames, synth.WIDTH, FPS, shifted, synth.STIM_ORIGIN, synth.OUT_ROI)

    def test_truncated_stream_is_an_error(self):
        frames, _ = synth.build(83.0, 0.0, events=1)
        data = b"".join(frames)[:-5]
        with self.assertRaisesRegex(ValueError, "middle of a frame"):
            list(analyze.read_frames(io.BytesIO(data), synth.WIDTH, synth.HEIGHT))


class Stimulus(unittest.TestCase):
    def test_counter_round_trips(self):
        faces = (stimulus.base_face(False), stimulus.base_face(True))
        for n in (0, 1, 45, 4321, 5399):
            img = stimulus.render_frame(n, faces)
            self.assertEqual(analyze.counter_value(bytes(img), stimulus.WIDTH, (0, 0), LAYOUT), n)

    def test_mouth_opens_exactly_on_scheduled_frames(self):
        opens = [n for n in range(1, stimulus.clip_length(60))
                 if stimulus.mouth_is_open(n) and not stimulus.mouth_is_open(n - 1)]
        self.assertEqual(opens, stimulus.schedule(60))

    def test_generator_writes_png_frames_and_layout(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(stimulus.main([d, "--events", "1"]), 0)
            with open(os.path.join(d, "frame_00000.png"), "rb") as f:
                head = f.read(24)
            self.assertEqual(head[:8], b"\x89PNG\r\n\x1a\n")
            self.assertEqual(int.from_bytes(head[16:20], "big"), stimulus.WIDTH)
            with open(os.path.join(d, "layout.json")) as f:
                self.assertEqual(json.load(f)["open_frames"], stimulus.schedule(1))

    def test_too_long_clip_is_refused(self):
        self.assertEqual(stimulus.main(["unused", "--events", "100000"]), 2)


class Statistics(unittest.TestCase):
    def test_nearest_rank_percentile(self):
        values = list(range(1, 101))
        self.assertEqual(analyze.percentile(values, 50), 50)
        self.assertEqual(analyze.percentile(values, 95), 95)
        self.assertEqual(analyze.percentile([7], 95), 7)

    def test_rising_crossing_is_interpolated(self):
        self.assertEqual(analyze.rising_crossings([0, 0, 100, 100, 0, 100], 25), [1.25, 4.25])


class Cli(unittest.TestCase):
    def test_budget_exit_codes(self):
        frames, _ = synth.build(83.0, 0.0)
        with tempfile.TemporaryDirectory() as d:
            raw, layout = os.path.join(d, "rec.raw"), os.path.join(d, "layout.json")
            with open(raw, "wb") as f:
                for frame in frames:
                    f.write(frame)
            with open(layout, "w") as f:
                json.dump(LAYOUT, f)
            argv = [raw, "--width", str(synth.WIDTH), "--height", str(synth.HEIGHT), "--fps", "60", "--layout", layout,
                    "--stim-origin", "%d,%d" % synth.STIM_ORIGIN, "--out-roi", "%d,%d,%d,%d" % synth.OUT_ROI]
            self.assertEqual(analyze.main(argv + ["--budget-ms", "100"]), 0)
            self.assertEqual(analyze.main(argv + ["--budget-ms", "50"]), 1)


if __name__ == "__main__":
    unittest.main()
