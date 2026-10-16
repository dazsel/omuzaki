# omuzaki-stage: `trigger.bind`, `trigger.unbind`, `trigger.press` and `trigger.list`

- `trigger.bind` binds a key (1 to 32 printable characters, case-insensitive) to a face expression, toggled or held; up to 16 keys, and a repeated key replaces its binding. An expression that is not on the character is refused with a did-you-mean hint (#381).
- `trigger.unbind` removes a key's binding; an expression it was showing fades out.
- `trigger.press` presses or releases a bound key, as a keyboard would; a binding whose expression is missing does nothing.
- `trigger.list` lists each binding with whether it is showing and whether its expression is missing.
