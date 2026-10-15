# omuzaki-mcp: a reply to a change waits until the journal has saved it

- After a call applies an edit, commits or rolls back a transaction, or undoes or redoes, the MCP reply waits for the journal to save the change (up to 5 s). Queries and previews never wait.
- If the save times out or fails, the call still succeeds, since the change did apply: the reply starts with `Warning: applied, not yet saved: <status>` and carries `warning` in its structured content.
- `McpOptions::with_journal` wires the journal. Without one, replies don't wait, as before (#254).
