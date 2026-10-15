# omuzaki-char-api: head exponent_lower

- `HeadOutline` gains `exponent_lower`: the superellipse exponent below the cheek line, joined to `exponent` above it (#1686). Unset, the outline keeps one exponent.
- The key is left out when unset, so existing files and the presets serialise byte-identically.
- Blends treat an unset side as its own `exponent`, so they stay continuous. The head does not read the field yet (#1688).
