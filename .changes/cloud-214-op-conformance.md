# Op conformance: registry-wide test

- Adds a registry-wide conformance test for ops: every registered op needs an honest schema and undo metadata, and each
  example it declares must decode, dry-run without touching anything and, for `Undo::Auto`, undo exactly. Ops without
  examples are on a list that may only shrink. It runs with the CLI's full test suite.
