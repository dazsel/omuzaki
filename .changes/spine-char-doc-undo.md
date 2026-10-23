# Undo restores a character file exactly

- Undoing an edit that added a value to a table holding only sub-tables, or that swapped a table for one of another shape (a hair group turned into guides), now gives back the file's exact text instead of leaving a stray `[table]` header or reordered keys.
