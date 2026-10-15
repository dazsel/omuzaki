# Docs index

Every doc under `docs/` starts with a front matter block that describes it:

```
---
title: "The doc's heading"
summary: "One line: what the doc covers."
area: design
---
```

`cargo xtask docs-index` prints the index of every doc from those blocks, grouped by `area`, so adding a doc means adding
one file and never editing this one. `cargo xtask lint-docs-index` fails on a doc without all three fields and on an index
list in this file.

Research notes (`research/`) are dated. Licences and facts go stale, so always check the date.
