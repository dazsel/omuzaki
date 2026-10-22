# Post settings out of range are refused by name

- `Post::validate` checks every post-processing number before a frame is drawn (#654): exposure, bloom strength, radius
  and threshold, depth of field focus distance, range and maximum blur, grain and the grade's saturation and contrast.
  A value that is not finite or out of its range fails with `Invalid { what: "post" }` naming the field and the range,
  instead of drawing a black or broken frame. `Scene::validate` runs it, and the stage reports it on `scene.post`.
- The JSON schema of each setting carries its range, so an agent sees the limits before it calls.
