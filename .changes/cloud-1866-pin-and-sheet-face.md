# A redraw honours a time pin, and the turnaround sheet gives the caller's face back

- `Stage::redraw` after `Stage::pin_time` runs at the pinned tick, as `tick` does: the face is painted at the pinned time and the scene's frame info is the pinned frame, where it used to keep the last frame's (#1866).
- `render_turnaround` paints the caller's last face channels again, at the caller's last time, when it gives the stage back, so the scene after a sheet shows the caller's expression and not the sheet's neutral face (#1867).
