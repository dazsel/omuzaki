# agent roots: the project folder plus the folders the person shared

`session_info` lists the file roots of the session: the project folder (its real path) and the shared folder, and no other.

Each block is one JSON-RPC message as it crossed an in-memory stream.

`-->`
```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "method": "initialize",
  "params": {
    "capabilities": {},
    "clientInfo": {
      "name": "claude-code",
      "version": "1.0"
    },
    "protocolVersion": "2025-11-25"
  }
}
```

`<--`
```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "result": {
    "capabilities": {
      "prompts": {},
      "resources": {
        "subscribe": true
      },
      "tools": {}
    },
    "instructions": "Omuzaki is a character studio. Every tool is an operation on the project; the tool list is generated from the op registry.\nWorking method:\n- Look before you edi [...the rest of the server guide...]",
    "protocolVersion": "2025-11-25",
    "serverInfo": {
      "name": "omuzaki",
      "title": "Omuzaki Studio",
      "version": "0.0.0"
    }
  }
}
```

`-->`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/initialized",
  "params": {}
}
```

`-->`
```json
{
  "id": 2,
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "arguments": {},
    "name": "session_info"
  }
}
```

`<--`
```json
{
  "id": 2,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "Session 1: you are agent claude-code#1 (client claude-code), working for user alice, connected over stdio.\nState: active.\nScopes: read, edit:character, edit:scene, edit:library, edit:settings, render, fs, review. File roots: <project>, <shared>.\nFence: none (the whole project, except parts another session holds).\nBudget: unlimited; spent 0 micro-dollars, 0 tokens, 0 seconds. Rate limits: 600 edits/min (burst 100), 3000 runs/min (burst 300), 16 pending approvals at most.\nPending approvals: none.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "ok": true,
      "session": {
        "actor": "agent claude-code#1",
        "budget": {
          "seconds": null,
          "tokens": null,
          "usd_micros": 0
        },
        "client": "claude-code",
        "credential": null,
        "fence": [],
        "grants": {
          "file_roots": [
            "<project>",
            "<shared>"
          ],
          "scopes": [
            "read",
            "edit:character",
            "edit:scene",
            "edit:library",
            "edit:settings",
            "render",
            "fs",
            "review"
          ]
        },
        "id": 1,
        "limits": {
          "edit_burst": 100,
          "edits_per_minute": 600,
          "max_pending": 16,
          "run_burst": 300,
          "runs_per_minute": 3000
        },
        "model": null,
        "on_behalf_of": "user alice",
        "open_transaction": null,
        "pending_approvals": [],
        "role": null,
        "spent": {
          "seconds": 0,
          "tokens": 0,
          "usd_micros": 0
        },
        "state": {
          "state": "active"
        },
        "steps": 0,
        "transport": "stdio"
      }
    }
  }
}
```
