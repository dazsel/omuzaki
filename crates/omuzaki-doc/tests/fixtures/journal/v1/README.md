# Frozen journal v1 fixture

`segment-000000000001.tjl` is a version 1 segment (journal.md 4.1 to 4.3): the 32-byte header for first LSN 1, then
three frames, the second one SKIPPABLE. It was written once, with a script that does not use the journal code, and
committed as bytes. It must never be regenerated or edited: it is what a build from today must still read. If the format
changes, add a `v2/` folder and keep this one.
