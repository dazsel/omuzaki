# Turnaround sheets are framed from the posed character

- `render_turnaround` frames the shot from the pose as drawn in all four views (#1854): a custom pose (sitting, jumping, a raised arm) fills the cell as the rest pose does, where it used to be refused for leaving the bind-pose frame.
- A pose that reaches further across than a narrow cell holds widens the shot until it fits, on both axes.
- Only hair or cloth that settles beyond the margin is still refused, with `TurnaroundError::Unsupported`.
