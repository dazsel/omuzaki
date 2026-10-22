# `render.turnaround` takes a pitch and an expression; `render.compare` counts what shows

- `render.turnaround` takes `pitch` (-30 to 60 degrees, the same for every view) and `expression` (a kit character's expression at full weight on every view) (#1714). An expression the character's face doesn't have, or any expression on an imported avatar, is refused naming `expression` with the expressions it has; `render.preview` refuses the same way, where it used to fail inside the render.
- `render.compare` compares colours premultiplied by their alpha, as they composite: a colour change under zero alpha no longer counts as a changed pixel (#1716).
