# omuzaki-char: mitten and simple hands

- The `body` part now builds hands: a mitten (smooth palm-and-finger block, separate thumb, finger-hint lines) or a simple hand (four fused fingers parted by shallow grooves, splayed thumb), left built and right mirrored.
- Hands follow `Proportions::hand_size` and the `HandParams` palm width and length, thumb length and thumb spread.
- The palm is skinned to the hand bone (its wrist end fading into the forearm), the finger block to the proximal finger bones and the thumb to the thumb bones, so every hand style shares one skeleton.
- Hands are built from the character's template (`BodyLayout::hand_table`), so they fit the standard, mini and tall bodies as well as soft-sd, and the thumb root lands on the layout's joint at every palm and spread setting. The wrist end reaches back inside the forearm.
- Hands have their own material, `body-hands`, with vertex colours on, so the fingertips take the warmer tint; the body's skin material is unchanged.
