# Bindings run in the studio, from keys, pads, MIDI controllers and OSC

- The studio starts the OS input source with the live slice (#1824, ADR 0060). Capture stays off until a person starts it from the drawn indicator, and the `input.*` operations work on the running studio.
- A binding runs its action as the person (#469, PF-05): an operation binding runs its operation on the press, with `cause: binding` and the binding's name, so it shows in history and undoes like any edit. A held binding runs an operation that takes `down` on the press and again on the release, so a key, a pad, a MIDI pad or a Stream Deck button can show an expression while it is held, through `trigger.press`.
- MIDI controllers are binding sources: a note, a controller or a program change is learned like a key.
- OSC messages are binding sources: `/omuzaki/binding/<id>` on 127.0.0.1 port 39570 presses a binding with a value of 0.5 or more and releases it below, and taps it with no argument. A Stream Deck reaches it through Bitfocus Companion or an OSC plugin.
