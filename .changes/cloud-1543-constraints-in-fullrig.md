# omuzaki-rig: node constraints in `FullRig`

- `FullRig::with_constraints` adds the VRM node-constraint stage, which runs after IK and before the spring bones (#1543).
- `FullRig::constraints` reads the stage back; a rig without one solves exactly as before.
- The solver must be built against the rig's own skeleton: the rig does not check it.
