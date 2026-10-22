# approvals: waiting, approval_wait, and the outcome in the heads-up

A request that needs the person's approval comes back `waiting`; `approval_wait` times out while it is still pending; once the person has rejected it, the next result's heads-up says so, once, and `approval_wait` still returns the outcome with the person's comment.

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
      "character": "mira"
    },
    "name": "export_vrm"
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
        "text": "export_vrm failed [waiting]: approval #1 asks the person to grant this session the `publish` scope for this request; nothing ran\nHint: don't retry: call approval_wait with id 1 to wait for the answer, or carry on with other work",
        "type": "text"
      }
    ],
    "isError": true,
    "structuredContent": {
      "code": "waiting",
      "detail": {
        "approval": 1,
        "asks": "grant this session the `publish` scope for this request",
        "kind": "queued",
        "need": {
          "need": "scope",
          "scope": "publish"
        }
      },
      "hint": "don't retry: call approval_wait with id 1 to wait for the answer, or carry on with other work",
      "message": "approval #1 asks the person to grant this session the `publish` scope for this request; nothing ran",
      "ok": false,
      "op": "export_vrm"
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
      "id": 1,
      "timeout_seconds": 1
    },
    "name": "approval_wait"
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
        "text": "Approval #1 is still pending: the person has not decided. Call approval_wait again to keep waiting, or carry on with other work; its outcome will be in the next result's heads-up.",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "approval": 1,
      "ok": true,
      "pending": true
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
    "arguments": {},
    "name": "project_ping"
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
        "text": "Heads-up: approvals you were waiting for have been decided since your last call.\n- Your request #1 (export_vrm) was rejected: 'not yet'.\n\nRan project_ping (read-only).\nOutput:\n\"pong\"",
        "type": "text"
      }
    ],
    "isError": false,
    "structuredContent": {
      "approvals": [
        {
          "approval": 1,
          "decision": "rejected",
          "tool": "export_vrm"
        }
      ],
      "ok": true,
      "op": "project_ping",
      "output": "pong"
    }
  }
}
```

`-->`
```json
{
  "id": 5,
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "arguments": {
      "id": 1
    },
    "name": "approval_wait"
  }
}
```

`<--`
```json
{
  "id": 5,
  "jsonrpc": "2.0",
  "result": {
    "content": [
      {
        "text": "Approval #1: The person rejected this: 'not yet'. Nothing ran.",
        "type": "text"
      }
    ],
    "isError": true,
    "structuredContent": {
      "approval": 1,
      "comment": "not yet",
      "decision": "rejected",
      "ok": false,
      "op": "export_vrm"
    }
  }
}
```
