# omuzaki-char: hair generators `cap` and `part_sweep`

- A hairstyle's `cap` group now makes a scalp shell (97 x 49 vertices) that follows the head, with a hairline that ends higher at the forehead and lifts over the ears; it keeps at least 2 mm off the skull (#175).
- A `part_sweep` group now makes locks that leave the part line and sweep down each side to the braid line; the braid line's heights are the style's own `end_height_front` and `end_height_back`.
- The default hairstyle now builds a cap and a part sweep instead of leaving both empty.
- Rebuilding either on a slider drag reuses its buffers and allocates nothing once they are warm.
