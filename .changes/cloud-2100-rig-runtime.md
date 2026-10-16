# omuzaki-rig: the rig runtime finished (springs proven against three-vrm, constraint order)

- Spring bones interpolate the animated pose across a frame's sub-steps: at 30 fps the hair follows a 120 fps run to
  0.9 mm instead of 47 mm, for 2% (60 fps) to 3% (30 fps) more spring time. `VrmSpringSolver::set_pose_interpolation(false)`
  holds the pose as VRM viewers do (#79).
- Spring bones are checked against three-vrm 3.5.5 with inside-sphere, inside-capsule and plane colliders (#1703).
- The spring solver and its oracle agree at 30, 50, 60, 90, 120 and 144 fps (#1849).
- Node constraints: an aim runs after the constraints on its source's and destination's ancestors, and a loop through
  them is refused as a cycle, as three-vrm orders them (#1619).
- Bone drivers run in the stage between the constraints and the springs (`FullRig::solve_and_skin_with`), reading the
  frame's channels, which survive a time pin, a variant swap and a redraw (#800). `Follow` drivers export as
  `VRMC_node_constraint`s; a `Channel` driver is reported, not exported (part of #801).
- Design: the physics dial, scene wind and spring presets (`docs/design/physics-dial-wind.md`, #1961).
