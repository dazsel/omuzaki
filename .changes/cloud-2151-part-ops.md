# omuzaki-char: part ops tell the truth

- `part.set_param` switches a tagged-enum variant (set a hair generator's `type`) and removes a map entry (`null` on a hair group); `replace: true` writes an object over the current one instead of merging (#128).
- `part.describe` lists every variant of a tag in `values`, and an invalid part is described with its own map entries listed, so it can be repaired by path; a repair writes only what it changes (#669).
- `part.describe` costs time in proportion to the part, not its square: a number's range is probed in its own map entry (#671).
- `part.add` lists the unknown-kind hint from the same sorted list as `part.list_kinds` (#665); the `omuzaki-char` ops docs build under `-D warnings` (part of #683).
