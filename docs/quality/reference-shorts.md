---
title: "Reference shorts"
summary: "The bar for short-form character video, and the loop that remakes a reference with the short.* ops"
area: quality
---

# Reference shorts

The quality loop (`CLAUDE.md`, "The quality loop: match a reference") applied to short-form character video. The
references are other creators' videos and stay out of the repository; this page describes them only in words, and
every render in the evidence uses the kit's own characters and the project's own scripts.

## Recreate, never copy

The reference is only looked at and scored against. No pixel of it goes into what Omuzaki renders: no texture, layer,
sprite or part cut, traced or fitted from its frames, and no puppet solved from it. The character is drawn and built
with Omuzaki's own tools (the kit, the 2D puppet's drawing and rigging, the face painter), studying the reference for
proportions, style, colour, motion and expression the way an artist studies a model sheet. The point of the exercise is
what those tools are missing for a character like this at pixel parity: every missing tool, weak result or slow step
met on the way is filed under the epic (#2393) and built. A recreation of another creator's character stays private,
like the reference; the tools it needed go into the repository.

`tools/reference-loop/parity.py` enforces the rule before it scores: it asks how the render was made (`--made-with`,
kept in `score.json`) and refuses when an image in an `--assets` folder is a near-copy of a frame of the reference.

## The two references

- **T1, a talking head.** About a minute, 9:16. One anime character, framed face to bust against near-black with a warm
  rim on the hair, talks to the camera. It changes face or shot more than once every two seconds, with anime marks
  (blank or dot eyes, an anger mark, sparkles, gloom, sharp teeth), burned-in captions on every phrase, punch-ins,
  tilts and handheld drift. The voice is the creator's own.
- **T2, a cinematic short.** About a minute, 9:16. Two characters in a lit interior, with depth of field, gestures,
  hands and touch between them, and a cut every two seconds or faster.

## The bar

`short.plan` and `short.render` report the timeline's measures against the T1 bar (`omuzaki_short::ops::bar_checks`).

| Measure | T1 bar | Where it is measured |
|---|---|---|
| Face or shot changes per minute | at least 40 | `short.plan` (`changes_per_minute`) |
| Distinct faces | at least 12 | `short.plan` (`distinct_faces`) |
| Anime marks used | at least 5 | `short.plan` (`marks`) |
| Spoken time under a caption | at least 95% | `short.plan` (`caption_coverage`) |
| Lip closure on p, b and m | at least 70% recall | by eye on the frame sheets, for now |
| A new user's first short | under 10 minutes | a hand run: script in, video out |
| Render time | at most 2x real time on a mid-range GPU | `short.render` (`realtime_factor`) |

T2 adds at least 25 shots per minute, hands in shot, two characters in contact and a render within 5x real time; its
issues are under the epic.

## The loop

1. Write the short: by hand as a `*.short.toml` (see `crates/omuzaki-short/shorts/gerald.short.toml`), or
   `short.draft { character, script }` from a plain script and then edit it. Beats inside a line go in braces before
   their word: `"I watered him. {happy+sparkle @close+tilt-left} I sang to him."`.
2. `short.plan` until the bar checks pass.
3. `short.render` to a WebM in the project (VP9 with the voice as Opus; 1080x1920 at 30 fps by default; `size` and
   `quality: "live"` for a quick draft).
4. `python3 tools/reference-loop/compare.py ours.webm reference.mp4 OUT` (OUT outside the repository) and read the
   sheets pair by pair: face, framing, light, captions, timing.
5. Score it: `python3 tools/reference-loop/parity.py ours.webm reference.mp4 OUT --made-with "..." --assets <the
   textures and layers it was built from>` for PSNR, SSIM and the worst frames (it refuses copied assets).
6. File each gap under the epic (#2393) with what "matches" means, close them in chunks, and render again.
