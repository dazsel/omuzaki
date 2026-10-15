# omuzaki-char: the hair physics core (Verlet chains)

- `hair::physics::verlet::ChainSet` simulates every simulated clump as a Verlet chain that springs back to the styled shape, following the head or blending head and chest (#182).
- Sphere colliders (the hair's own, else the rig's), wind, gravity and damping from `HairPhysics`, at a fixed rate with the remainder carried between frames.
- `reset` snaps to the styled shape and clears the carried time; `settle(steps)` settles after a build.
- No allocation per frame; about 0.03 ms a frame for Mira-class hair.
