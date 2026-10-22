# Imported avatars: the licence join

- `licence.derive`, `licence.set`, `licence.describe` and the export checks read an imported character: its parts are
  the model itself (part `avatar`, kind `External`, row `sources/avatar`) plus each attachment, so the model's own row
  is joined rather than reported as orphaned. An attachment with no row makes the record the most restrictive and is
  named.
- `licence.describe` of an imported model whose terms allow non-profit use only notes `rights.perform_non_profit`. It
  never blocks.
