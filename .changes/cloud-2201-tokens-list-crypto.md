# omuzaki-mcp tests: the connected-apps listing installs the rustls crypto provider

- `tests/mcp_http_tokens_list.rs` now calls `common::http::install_crypto_provider()` before it starts its port, like the other HTTP tests. Its tests no longer panic with "No rustls crypto provider is configured" in a whole-workspace check.
- `tests/mcp_journal.rs`: the unsaved-change test waits for the person's edit to reach the event log before calling, so its heads-up check no longer races the log's worker thread (it failed under whole-workspace load).
