# omuzaki-anim-api, omuzaki-rig: spring joint limits

- `SpringJoint` gains `limit: Option<JointLimit>` (`VRMC_springBone_limit`): a cone, hinge or spherical bound on the
  direction a spring bone may swing in. It defaults to no limit, so existing sets and files are unchanged.
- `SpringSet::validate` refuses a negative or non-finite angle and a limit rotation that is not a unit quaternion.
- `VrmSpringSolver` applies the limit right after the inertia, stiffness and gravity update and again after each
  collider push, keeping the bone length.
