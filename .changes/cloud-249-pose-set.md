# omuzaki-anim: a character's pose set (`pose.set`, `pose.delete`, `pose.list_character`)

- `pose.set` stores a pose at `characters/<id>/poses/<name>`, copied from a library pose (`from`) or given inline (`pose`), and mirrored with `mirror: true`.
- The stored rotations are copied, so a later library change never changes the character; undo restores the set exactly.
- `pose.delete` removes one pose from the set, and refuses an unknown name with a did-you-mean.
- `pose.list_character` lists the character's own poses in name order.
