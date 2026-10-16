# omuzaki-anim: pose clips (`PoseClip`)

- `PoseClip` plays looped or one-shot key poses from the pose library, eased, with a foot contact flag pair per key (#223).
- `ClipLibrary::builtin()` ships the `wave`, `wave-both`, `yawn` (4.2 s) and `walk` clips; `mirrored()` flips any clip.
- Clips fail closed on load: key times, pose ids, easing and the loop seam are checked.
