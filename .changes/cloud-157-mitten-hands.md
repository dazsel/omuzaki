# omuzaki-char: mitten and simple hands

- The `body` part now builds hands: a mitten (smooth palm-and-finger block, separate thumb, finger-hint lines) or a simple hand (four fused fingers parted by shallow grooves, splayed thumb), left built and right mirrored.
- Hands follow `Proportions::hand_size` and the `HandParams` palm width and length, thumb length and thumb spread.
- The palm is skinned to the hand bone (its wrist end fading into the forearm), the finger block to the proximal finger bones and the thumb to the thumb bones, so every hand style shares one skeleton.
