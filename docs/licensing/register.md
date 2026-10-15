---
title: "Licence register"
summary: "Licence register"
area: licensing
---

# Licence register

Everything we use or plan to use, with its verdict (see `policy.md`). Evidence and sources are in `docs/research/2026-10-licence-audit.md`; the "Audit" column points at its section. Add or update its file in `docs/licensing/deps/` in the same PR that adds, upgrades or drops the item.

Format rule (the repo licence gate will parse this): one row per item, with columns in this order. `Status` is one of `yes`, `conditions`, `external`, `NO`, `unverified`, `private-test`.

The rows live in `deps/`, one TOML file per item (`deps/<name>.toml`); `cargo xtask lint-licences --print` prints this page with the tables filled in.

## In use now (prototypes and tooling)

<!-- licence-table: in-use -->

## Planned: Rust stack (spec §9)

<!-- licence-table: rust -->

## Planned: models, data and services (spec §14, §16)

<!-- licence-table: models -->
