# io.import takes a stored asset, and the tray decides

- `io.import` takes `asset` (a content hash) beside `path` and `bytes`: a model already in the store, such as a
  candidate accepted in the tray, imports by its hash, as a reference or as an avatar (#1231).
- A pending candidate is refused until it is accepted in the tray, and a rejected one is refused with its reason, in
  `plan` and again when the job starts, before any byte is read from the store.
- The import's record is the store's joined with every tray record for the same bytes, strictest wins, so bytes a
  permissive run stored first can't carry wider rights than a later, stricter record says.
