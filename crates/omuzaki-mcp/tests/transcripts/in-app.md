# the in-process link

The host opens the assistant's session with `connect_in_app` and runs a client on the returned stream. The session is `in_app`, acts for the person, and edits in its own name; when the person revokes it the stream ends.

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
      "name": "assistant-ui",
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
        "text": "Session 1: you are agent assistant#1 (client assistant, model claude-opus-5-5), working for user alice, connected over in-app.\nState: active.\nScopes: read, edit:character, edit:scene, edit:library, edit:settings, render. File roots: none.\nFence: none (the whole project, except parts another session holds).\nBudget: unlimited; spent 0 micro-dollars, 0 tokens, 0 seconds. Rate limits: 600 edits/min (burst 100), 3000 runs/min (burst 300), 16 pending approvals at most.\nPending approvals: none.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "ok": true,
      "session": {
        "actor": "agent assistant#1",
        "budget": {
          "seconds": null,
          "tokens": null,
          "usd_micros": 0
        },
        "client": "assistant",
        "credential": null,
        "fence": [],
        "grants": {
          "file_roots": [],
          "scopes": [
            "read",
            "edit:character",
            "edit:scene",
            "edit:library",
            "edit:settings",
            "render"
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
        "model": "claude-opus-5-5",
        "on_behalf_of": "user alice",
        "open_transaction": null,
        "pending_approvals": [],
        "role": "assistant",
        "spent": {
          "seconds": 0,
          "tokens": 0,
          "usd_micros": 0
        },
        "state": {
          "state": "active"
        },
        "steps": 0,
        "transport": "in_app"
      }
    }
  }
}
```

`-->`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "arguments": {
      "blend": 0.4,
      "character": "mira"
    },
    "name": "face_set_blend"
  }
}
```

`<--`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "Applied face_set_blend (log #1, transaction 1, document version 1): changed 1 value.\n  characters/mira/face/blend: absent -> 0.4\nTo revert, call history_undo.\nNext: check it visually with `render_preview` and numerically with the `measure_*` tools.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "applied": true,
      "changes": [
        {
          "after": 0.4,
          "before": null,
          "path": "characters/mira/face/blend"
        }
      ],
      "log": {
        "seq": 1,
        "tx": 1,
        "version": 1
      },
      "mode": "apply",
      "ok": true,
      "op": "face_set_blend"
    }
  }
}
```
