---
title: "How to measure glass-to-glass latency (V0 harness)"
summary: "Run the frame-counting latency harness on Windows with a real webcam, and what to attach as V0 evidence."
area: maintainers
---

# How to measure glass-to-glass latency

V0's bar is **100 ms or less at p95, glass to glass** (#1423, exit criterion 3): a change in front of the camera lens,
through capture, tracking, retarget and render, to the avatar changing on the screen. The harness (#1967) measures it
with one screen recording and no special hardware, by frame counting.

## How it works

1. `tools/latency/stimulus.py` draws a synthetic face (never a human clip) whose mouth snaps from closed to open every
   1.5 s, with the clip frame number burned in as a row of 16 black and white cells.
2. You play the clip in a window and point the webcam at it. Omuzaki tracks that face as usual.
3. One screen recording at 60 fps or more holds the stimulus window and Omuzaki's output window.
4. `tools/latency/analyze.py` finds each frame where the stimulus mouth opens (and checks the burned-in counter names a
   scheduled event), finds the first later frame where the output's jaw signal crosses half way, and reports p50, p95
   and max of the delays over at least 50 events. The crossing is interpolated between frames, so the output side is
   finer than one frame; the stimulus side is exact to one recording frame, so read the result as ±1 frame (17 ms at 60 fps).

The tool needs Python 3 (standard library only). Decoding the recording needs [ffmpeg](https://ffmpeg.org), which is
called as a separate program here and is never linked, vendored or shipped.

## The output signal

The analysis reads a small rectangle of the output window whose mean grey level follows `jawOpen`:

- **Marker patch (preferred, once the output can draw one):** a patch that is black at `jawOpen` 0 and white at 1, drawn
  by the output in test mode. Omuzaki has no such mode yet, so use the avatar pixels below until it does.
  Pass its rectangle as `--out-roi`. If the jaw never reaches 0 and 1 during the run, pass the grey levels for
  `jawOpen` 0 and 1 as `--out-range LOW,HIGH`; otherwise the tool takes the 2nd and 98th percentile of what it saw.
- **Avatar pixels:** the avatar's mouth interior (dark when open). Pass its rectangle and `--out-polarity dark`. The
  half-way crossing of the pixels stands in for `jawOpen` 0.5.

## Run on Windows with a real webcam

1. Generate the clip (60 events, 90 frames each, about 90 s):

   ```sh
   python tools/latency/stimulus.py stimulus --fps 60 --events 60
   ffmpeg -framerate 60 -i stimulus/frame_%05d.png -c:v libx264 -crf 12 -pix_fmt yuv420p stimulus.mp4
   ```

2. Set the display to 60 Hz or more. Play `stimulus.mp4` in a player at 100 % scale (one clip pixel is one screen
   pixel), in a window the camera can see, loop off. Aim the webcam so the face fills the frame, steady and well lit.
3. Start Omuzaki with that webcam and the default avatar, with the output window on the same display as the player,
   or on a second display captured in the same recording.
4. Record the whole screen with OBS (Display Capture, 60 fps, Output mode Advanced, a lossless or high-quality
   recording format such as MKV with x264 CRF 12 or lower). Start recording, then press play, then stop once the clip
   ends.
5. Take a screenshot of the same layout. Read from it the top-left pixel of the stimulus window (`--stim-origin`) and
   the rectangle of the output signal (`--out-roi`), and the recording size.
6. Decode and analyse:

   ```sh
   ffmpeg -i rec.mkv -pix_fmt gray -f rawvideo - | python tools/latency/analyze.py - \
     --width 1920 --height 1080 --fps 60 --layout stimulus/layout.json \
     --stim-origin 100,100 --out-roi 1200,300,64,64 --budget-ms 100
   ```

   The command exits 0 when p95 is within the budget, 1 when it is above it, and 2 when the recording can't be
   analysed (fewer than 50 paired events, a flat signal, a truncated recording). `--json` adds every delay.

If a run reports many "rejected by counter" events, the player dropped or repeated frames: lower the load and record
again. A recording whose frame rate is variable gives wrong times; record at a constant rate.

## What to attach as V0 evidence

- The command line and the tool's output (events, p50, p95, max).
- The `--json` report, which lists every delay.
- The screenshot used for the regions, and the machine's CPU, GPU, webcam model and capture mode (the webcam's own
  exposure and USB time is part of the glass-to-glass figure).
- The Omuzaki build and the tracker settings.

Do not attach the recording itself (it is large, and it shows the screen); describe it.

## Testing the tool

`bash tools/tests/latency.sh` runs the analysis tests CI runs. They build a synthetic recording in memory with a known
injected delay and check the tool recovers it; no recording is committed. For an injected 83 ms ± 12 ms of jitter at
60 fps the true p95 is 94.3 ms and the tool reports 94.3 ms.
