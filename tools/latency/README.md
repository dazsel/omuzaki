# How to measure glass-to-glass latency (V0 harness)

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

- **Avatar pixels (what Omuzaki offers today):** the avatar's mouth interior, which is dark when the mouth is open. Pass
  its rectangle and `--out-polarity dark`. The half-way crossing of the pixels stands in for `jawOpen` 0.5.
- **Marker patch (once the output can draw one):** a patch that is black at `jawOpen` 0 and white at 1, drawn by the
  output in test mode. Omuzaki has no such mode yet. Pass its rectangle with the default `--out-polarity bright`.

By default the closed and open levels are the 2nd and 98th percentile of what the recording shows. If the jaw never
reaches both ends during the run, pass the grey levels the signal has at `jawOpen` 0 and 1 as
`--out-range CLOSED,OPEN`, exactly as they appear in the recording (so `--out-range 230,40` for a dark-when-open mouth).

An event whose avatar does not respond within `--max-latency-ms` (600 by default) is reported as "no response within
the window" and counts as that whole window in p50, p95 and max, so a stuck tracker fails the budget instead of
disappearing from the numbers.

## Run on Windows with a real webcam

Commands below are for PowerShell (Python 3 and ffmpeg on the `PATH`). Run them from the repository root.

1. Generate the clip (60 events, 90 frames each, about 90 s):

   ```powershell
   python tools\latency\stimulus.py stimulus --fps 60 --events 60
   ffmpeg -framerate 60 -i stimulus\frame_%05d.png -c:v libx264 -crf 12 -pix_fmt yuv420p stimulus.mp4
   ```

2. Set the display to 60 Hz or more. Play `stimulus.mp4` in a player at 100 % scale (one clip pixel is one screen
   pixel), in a window the camera can see, loop off. Aim the webcam so the face fills the frame, steady and well lit.
3. Start Omuzaki with that webcam and the default avatar, with the output window on the same display as the player,
   or on a second display captured in the same recording.
4. Record the whole screen with OBS (Display Capture, 60 fps, Output mode Advanced, a lossless or high-quality
   recording format such as MKV with x264 CRF 12 or lower). Start recording, then press play, then stop once the clip
   ends.
5. Take a screenshot of the same layout, once with the avatar's mouth closed and once open. Read from it the top-left
   pixel of the stimulus window (`--stim-origin`), the rectangle of the avatar's mouth (`--out-roi`; it must lie inside
   the mouth in both states) and the recording size.
6. Decode and analyse. The decoded frames are binary, and PowerShell's own pipeline would re-encode them as text, so
   run the pipe inside `cmd`, which passes the bytes through unchanged:

   ```powershell
   cmd /c "ffmpeg -hide_banner -loglevel error -i rec.mkv -pix_fmt gray -f rawvideo - | python tools\latency\analyze.py - --width 1920 --height 1080 --fps 60 --layout stimulus\layout.json --stim-origin 100,100 --out-roi 1200,300,40,20 --out-polarity dark --budget-ms 100"
   ```

   Add `--json` for every delay. In Git Bash or any POSIX shell the same pipeline works without `cmd /c`.

   The command exits 0 when p95 is within the budget, 1 when it is above it, and 2 when the recording can't be
   analysed (fewer than 50 usable events, no response at all, a flat signal, a truncated recording).

If a run reports many "rejected by counter" events, the player dropped or repeated frames: lower the load and record
again. A recording whose frame rate is variable gives wrong times; record at a constant rate.

## Owner run

CI checks the analysis on generated frames only. The end-to-end run on Windows with a real webcam is an owner step and
has not been done by the change that added this harness: follow "Run on Windows with a real webcam" above on the
owner's machine, with the avatar's mouth rectangle as `--out-roi` and `--out-polarity dark`, check that the report holds
at least 50 events with the mouth closed and open on the screenshots, and attach the evidence below.

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
