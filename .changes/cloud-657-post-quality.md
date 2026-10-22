# One quality switch per post effect

- `Quality::post_effects` is now a `PostQuality` with one switch each for bloom, depth of field, and grain with the
  grade (#657), since they differ about ten-fold in cost. `PostQuality::ALL` and `PostQuality::NONE` are the old `true`
  and `false`, and `true` or `false` in JSON still reads as all or none.
- The frame-budget scheduler drops them one at a time: depth of field is its first rung, bloom its second, and grain
  and grade its last, after resolution, shadows and samples, because they cost almost nothing and are part of the
  author's look. Its change notices name `depth_of_field_off`, `bloom_off` and `grain_and_grade_off` in place of
  `post_effects_off`.
- The presets are unchanged: live runs none, high and offline run every effect the scene asks for.
