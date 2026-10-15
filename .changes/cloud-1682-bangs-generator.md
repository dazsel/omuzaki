## omuzaki-char: hair bangs generator

- The `bangs` hair generator now builds the close-up fringe: layers of clumps (broad under-layers, a pointed top layer) and loose wisps, with per-lock tip heights, sweep and tip hook.
- It draws no random numbers, so the long hair's stream is unchanged.
- The width is the closest current `Taper`, within 7 mm of the prototype's outline; it becomes exact once #842 lands.
- Every rest point keeps the tube's surface (half its thickness plus 1 mm) off the head, even at extreme hook and sweep.
