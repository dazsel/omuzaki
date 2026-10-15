# omuzaki-char: framing locks, with optional lock heights on `FramingGen`

- The `framing` hair generator now builds `count` locks per side along the `edge` outline: the left tucks locks behind the ear, the right hangs them in S-waves (#1676).
- `FramingGen` gains an optional `locks` (`FramingLocks`) with the prototype's hand-placed heights; unset, a lock is built from `edge` and `tail`, and existing files parse and write back unchanged.
- Each lock draws two random numbers (phase, then amplitude), so `HairRng::LegacyLcg` parity holds.
