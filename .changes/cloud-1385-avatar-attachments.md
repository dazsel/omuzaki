# Imported avatars: attachments

- An imported avatar now has anchors: one per humanoid bone (`left-upper-arm`), `head-top` on the model's own crown
  (from the meshes the head carries, or from the eyes when there are none, by the rule the kit's templates follow), and
  `left-grip` and `right-grip` at the palms (`omuzaki_avatar_api::anchors`).
- A character's `avatar.parts` are built by the kit through its own `Attach` path against those anchors and drawn as
  part of the avatar, so a halo or a prop on an imported model shows in the viewport, stills and live, and follows its
  bone. A part with bones of its own (a halo's bob) or one that reads the kit head's surface (hats, glasses) is refused
  with what to do, for now.
