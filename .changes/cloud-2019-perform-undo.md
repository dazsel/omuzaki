# perform.calibrate and perform.set_retarget can be undone

- `perform.set_retarget` and `perform.calibrate` now declare `Undo::Explicit` instead of `Undo::None`. Their changes live in the performer, not the document, so each step records the calls that put it back (#2019).
- Undoing `set_retarget` sets all four dials as they were requested before; redoing sets them as this change left them.
- Undoing `calibrate` puts back the calibration that was in use before the capture, through the new `restore` argument (each `ARKit` channel's range by name, plus the rest head pose). Redoing captures again. A `restore` is itself undoable. A channel that isn't `ARKit` is refused by name.
- Both lines are gone from the conformance allowlist's `[edits with Undo::None]` section.
