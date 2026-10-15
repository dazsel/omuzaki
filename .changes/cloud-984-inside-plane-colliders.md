# omuzaki-anim-api, omuzaki-rig: inside and plane spring colliders

- `ColliderShape` gains `SphereInside`, `CapsuleInside` and `Plane` (VRM 1.0 extended colliders) and is now `#[non_exhaustive]`.
- `SpringSet::validate` rejects a plane normal that is not unit length and a negative or non-finite radius on every shape.
- The spring solver keeps joints inside the inside shapes and on the front side of planes; cloth contacts skip them (`Shape::from_collider` returns `Option`).
- The VRM gather writes the new shapes to the avatar file, scaled like the others.
