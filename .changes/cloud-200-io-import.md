# omuzaki-io-ops: `io.import`

- `io.import` reads a model file (or bytes), stores the original unchanged and each image in the asset store with the licence record derived from the file's rights, and records the asset under `assets/<hash>` in the document (#200).
- It is a job: progress and cancellation go through the job manager. A cancelled import commits no asset record; objects it had already stored may stay in the store, whole and unreferenced, until garbage collection removes them.
- An object already in the store is shared only under an identical record; any difference is a conflict (#1773 tracks sharing across different rights).
- Only `as: reference` is available; importing as an editable part is a later task.
