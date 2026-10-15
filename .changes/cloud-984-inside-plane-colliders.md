# omuzaki-anim-api, omuzaki-rig: inside and plane spring colliders

- `ColliderShape` gains `SphereInside`, `CapsuleInside` and `Plane` (VRM 1.0 extended colliders) and is now `#[non_exhaustive]`.
- `SpringSet::validate` rejects a plane normal that is not unit length and a negative or non-finite radius on every shape.
- The spring solver's collision push puts a joint back inside an inside shape and on the front side of a plane, but the spec keeps the bone length afterwards, so a tail whose root is closer than a bone length can still end a step on the wrong side. Cloth contacts skip these shapes (`Shape::from_collider` returns `Option`).
- The VRM gather keeps the new shapes in the avatar scene, scaled like the others. VRM 1.0 export does not write them yet: it writes the spec's inert sphere in their place and warns `vrm.collider_dropped`.
