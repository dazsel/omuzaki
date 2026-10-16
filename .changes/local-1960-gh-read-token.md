# tools: read-only GitHub App token for bulk reads

- `tools/gh-read-token.sh` mints a short-lived GitHub App token, scoped at mint time to read-only permissions on
  `dazsel/omuzaki`, so bulk reads stop spending the shared write budget (#1960).
