# a person's edit between the agent's read and write

The write is refused with the conflict and the heads-up that names who changed it; the retry is an informed overwrite and passes.

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
    "arguments": {
      "blend": 0.2,
      "character": "mira"
    },
    "name": "face_set_blend"
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
        "text": "Applied face_set_blend (log #1, transaction 1, document version 1): changed 1 value.\n  characters/mira/face/blend: absent -> 0.2\nTo revert, call history_undo.\nNext: check it visually with `render_preview` and numerically with the `measure_*` tools.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "applied": true,
      "changes": [
        {
          "after": 0.2,
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
        "text": "Heads-up: since your last call, others changed the project (1 change; the log is now at n=3).\n- user tester applied face_set_blend (n=3): characters/mira/face/blend\nRe-read what you rely on before editing it. For old and new values, read omuzaki://log (or call history_log, with after_n) and look at events with n above 2.\n\nface_set_blend failed [conflict]: `characters/mira/face/blend` changed after you last looked\nHint: re-read it; your next call carries the newer base",
        "type": "text"
      }
    ],
    "isError": true,
    "structuredContent": {
      "code": "conflict",
      "detail": {
        "by": "user tester",
        "kind": "changed",
        "path": "characters/mira/face/blend",
        "version": 2
      },
      "hint": "re-read it; your next call carries the newer base",
      "message": "`characters/mira/face/blend` changed after you last looked",
      "ok": false,
      "op": "face_set_blend",
      "since_your_last_call": {
        "after_n": 2,
        "changes": [
          {
            "action": "applied",
            "actor": "user tester",
            "first_n": 3,
            "last_n": 3,
            "more_paths": 0,
            "paths": [
              "characters/mira/face/blend"
            ],
            "times": 1,
            "what": "face_set_blend"
          }
        ],
        "missed": 0,
        "more": "read omuzaki://log or call history_log, and look at events with n above 2",
        "not_listed": 0,
        "total": 1,
        "up_to_n": 3
      }
    }
  }
}
```

`-->`
```json
{
  "id": 4,
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
  "id": 4,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "Applied face_set_blend (log #3, transaction 3, document version 3): changed 1 value.\n  characters/mira/face/blend: 0.9 -> 0.4\nTo revert, call history_undo.\nNext: check it visually with `render_preview` and numerically with the `measure_*` tools.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "applied": true,
      "changes": [
        {
          "after": 0.4,
          "before": 0.9,
          "path": "characters/mira/face/blend"
        }
      ],
      "log": {
        "seq": 3,
        "tx": 3,
        "version": 3
      },
      "mode": "apply",
      "ok": true,
      "op": "face_set_blend"
    }
  }
}
```
