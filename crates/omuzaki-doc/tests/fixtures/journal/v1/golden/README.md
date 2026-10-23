# Golden v1 journal segments

Three version 1 segments (journal.md 4.1 to 4.5) with consecutive LSNs: 1 to 6, 7 to 11 (a rotation; the last record is
a skippable record of unknown kind 200) and 12 to 14. `tests/journal_golden.rs` makes them (`OMUZAKI_BLESS=1` writes
them the first time) and checks that this build still writes exactly these bytes and still reads them back. They are
synthetic: host `test-host`, pid 1, the mock character.

They must never be regenerated or edited once merged. If the format changes, add a `v2/` folder and keep this one.
(`../segment-000000000001.tjl` is a separate, older fixture of the byte format alone; see `../README.md`.)
