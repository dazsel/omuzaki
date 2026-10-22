# Characters show in their look on the stage and in renders

- A kit character loaded from the document comes in its stored look (#714). `load_body` draws it in `characters/<id>/look`, so `render.preview`, `render.turnaround` and everything else that loads a body show the look. `body::draw_in_look` is the same step for a caller that builds the character itself.
- `Stage::apply_look` draws a staged character in a new look by updating only the materials it changes, in place: no mesh or texture is created and nothing is rebuilt, so a `look.*` edit shows on the next frame. Going back to the default look gives back the earlier frame byte for byte. A look the materials can't take changes nothing.
- `render.preview` applies the look's frame settings over the scene's: bloom and depth of field off, film grain, and the colour grade.
