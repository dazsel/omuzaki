# omuzaki-char: bangs designer ops `hair.bangs.set` and `hair.bangs.list_presets`

- `hair.bangs.set` edits one field of a hair part's bangs group: `layers`, `sweep_deg`, `wisps`, and per layer `clumps`, `tip_heights` and `hook_deg` (`layers/<i>/<field>` for one layer). Each edit is one undoable patch; a value out of range is refused naming the field (#1683).
- `hair.bangs.list_presets` lists the five presets (`blunt`, `side-swept`, `curtain`, `wispy`, `none`) with their `BangsGen` values.
- The preset table and the shared bangs access are public in `omuzaki_char::ops::hair::bangs` for the preset op.
