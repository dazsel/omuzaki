# A scene's whole cast on one stage, drawn in one frame

- `omuzaki_stage::cast::CastStage` runs a lead (the scene's subject, which owns the scene, the camera, the lights and
  the target) and any number of other members, each a `Stage`, an `AvatarStage` or an `AnyStage` of its own keyed by
  its `MemberId`. A frame runs every member's frame loop up to the render, appends the members' instances, joint
  palettes, morph weights and particles to the lead's scene with their ranges moved past the lead's, and renders once:
  one render and one report for the whole cast, so the frame-budget scheduler sees one frame.
- Each member keeps the last input its performer gave it (`set_member_input`, or `tick_members` for the whole cast) and
  runs on the lead's clock; each stands at its own root (`set_member_root`, or `set_member_roots` from the resolved
  figures); each performer bakes its triggers against its own member's expressions (`trigger_expressions_of`).
- `CastStage` is a `FrameLoop`, so `LiveStage` runs a cast as it runs one body. The steady-state two-member frame
  allocates nothing.
- Every stage's frame is split at the render (`StageMember::prepare`, `prepare_redraw` and `submit`); `tick` and
  `redraw` are those halves run together, unchanged.
