# omuzaki-io-ops: `io.import`

- `io.import` reads a model file (or bytes), stores the original unchanged and each image in the asset store with the licence record derived from the file's rights, and records the asset under `assets/<hash>` in the document (#200).
- It is a job: progress and cancellation go through the job manager, and a cancelled import stores nothing.
- Only `as: reference` is available; importing as an editable part is a later task.
