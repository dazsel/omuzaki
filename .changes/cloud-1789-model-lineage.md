# xtask: `cargo xtask models` checks each shipped model's lineage

- The shipped-model licence gate now also validates `card.lineage`: a model needs at least one base-model or training-data link, and every link needs an `https://` source `url` and a licence on the policy allow-list (anything unlisted, banned or non-commercial fails), naming the model and the link (#1789).
- `docs/licensing/policy.md` says what counts as documented lineage. No model shipped on `main` yet, so none needed a fix.
