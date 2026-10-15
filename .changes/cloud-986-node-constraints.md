# omuzaki-anim-api and omuzaki-rig: node constraints

- VRM `VRMC_node_constraint` roll, aim and rotation constraints now simulate, through `omuzaki_rig::constraint::VrmConstraintSolver` and the new `omuzaki_anim_api::constraint` data types (#986).
- A `ConstraintSet` is checked for missing bones, self-sources, two constraints on one bone, bad weights and cycles; constraints run in dependency order.
- `apply` allocates nothing; 32 constraints on a 128-bone skeleton take about 6.5 microseconds.
- Wiring into the composed rig (after IK, before springs) is a separate change.
