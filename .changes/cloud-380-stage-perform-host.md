# omuzaki-stage: `perform.calibrate`, `perform.set_retarget`, `perform.status`

- `perform.calibrate` starts a neutral-face capture (30 to 600 tracked frames, default 150); an agent needs a person to confirm it (#380).
- `perform.set_retarget` tunes smoothing (0 to 1), exaggeration (1 to 3), head gain (0 to 3) and mirror; out-of-range values are refused, never clamped.
- `perform.status` reads tracking, calibration progress, the retarget settings and the tracker's frame rate.
- The ops reach the live performer through `PerformHost`; the perform thread calls `PerformControl::advance` in place of `Performer::advance`.
