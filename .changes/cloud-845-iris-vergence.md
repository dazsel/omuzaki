# omuzaki-face: iris convergence

- `PaintParams.vergence` now moves each iris toward its own nose (negative apart), so `eyeLookIn*` and `eyeLookOut*` converge the eyes (#845).
- The travel is the profile's `gaze_travel[0]` times the eye scale, the same look units as gaze x; the aperture clip still bounds the iris.
- A vergence of 0 draws what it drew before.
