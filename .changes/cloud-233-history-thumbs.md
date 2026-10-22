# History thumbnails: a picture for every committed transaction

- `omuzaki_stage::history::thumbs::Thumbnails` follows a runner's history and renders a 96x96 thumbnail, at 2x and live quality, of the character each committed transaction touched, in its default framing (#233).
- It renders on its own thread (`omuzaki-thumbs`), one picture at a time, never on the UI or the frame thread. It goes through `render.preview`'s work outside the runner, so a thumbnail never adds a step to the history it shows.
- Bursts coalesce: a burst renders once, 250 ms after its last commit (at most 2 s after its first), and every transaction in it points at that picture. A burst of 100 commits renders once.
- The pictures go to the asset store, and the index (transaction id to hash) lives in the project's `.omuzaki/history/thumbs.toml`, written atomically, so it is there again when the project is reopened.
