# Golden v1 audit segment

`segment-000000000001.tjl` holds one of each audit fact (kind 11, written SKIPPABLE) with three ops among them, LSNs 1
to 13. `tests/journal_audit.rs` made it (`OMUZAKI_BLESS=1`) and checks that this build writes the same bytes and reads
them back. Like the rest of `v1/`, it never changes; a format change adds a `v2/` folder.
