# omuzaki-char: simulated hair follows the head

- The `hair` part's simulated groups are skinned wholly to the head joint, as its static groups already were, so side and back hair follows a head turn.
- The mesh is no longer marked dynamic while no hair runtime moves it; #322 reverts this when the runtime lands.
