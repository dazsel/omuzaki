# The render ops show poses from the pose library

- `render.preview`, `render.turnaround` and `render.contact_sheet` take `pose: {id, mirror?, blend?: {with, weight}}`, a pose of the pose library (`pose.list`). It can be mirrored left to right or blended with a second library pose, and its hand shapes are fitted to the character's own thumbs (#224).
- An unknown pose id is refused with the nearest id and the list of poses.
- `render.contact_sheet {character, poses: [...]}` renders a labelled pose library sheet, one cell per pose. `pose` also applies to every cell of an expression or variant sheet.
- `PoseArg::resolve_on` gives the same pose for a turnaround sheet's `SheetPose::custom`.
