# long jobs as MCP tasks

A client that declares the tasks capability gets a task handle for a job op, progress notifications, a task status push when the job changes, the finished task's result (the same result the blocking call returns), and `tasks/cancel`.

Each block is one JSON-RPC message as it crossed an in-memory stream.

`-->`
```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "method": "initialize",
  "params": {
    "capabilities": {
      "extensions": {
        "io.modelcontextprotocol/tasks": {}
      }
    },
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
      "extensions": {
        "io.modelcontextprotocol/tasks": {}
      },
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
    "_meta": {
      "progressToken": "export-1"
    },
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
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "resultType": "task",
    "status": "working",
    "statusMessage": "waiting for a worker",
    "taskId": "job-1",
    "ttlMs": null
  }
}
```

`-->`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "method": "tasks/get",
  "params": {
    "taskId": "job-1"
  }
}
```

`<--`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "result": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "resultType": "complete",
    "status": "working",
    "statusMessage": "waiting for a worker",
    "taskId": "job-1",
    "ttlMs": null
  }
}
```

`<--`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/progress",
  "params": {
    "message": "encoding",
    "progress": 0.5,
    "progressToken": "export-1",
    "total": 1.0
  }
}
```

`<--`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/tasks",
  "params": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "status": "working",
    "taskId": "job-1",
    "ttlMs": null
  }
}
```

`<--`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/tasks",
  "params": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "result": {
      "content": [
        {
          "text": "Applied export_vrm (log #1, transaction 1, document version 0): no values changed.\nEstimated cost: about 12 s, 2048 MiB of GPU memory.\nTo revert, call history_undo.\nOutput:\n{\n  \"file\": \"mira.vrm\"\n}",
          "type": "text"
        }
      ],
      "isError": false,
      "resultType": "complete",
      "structuredContent": {
        "applied": true,
        "cost": {
          "gpu_mib": 2048,
          "seconds": 12.0,
          "usd": null
        },
        "log": {
          "seq": 1,
          "tx": 1,
          "version": 0
        },
        "mode": "apply",
        "ok": true,
        "op": "export_vrm",
        "output": {
          "file": "mira.vrm"
        }
      }
    },
    "status": "completed",
    "taskId": "job-1",
    "ttlMs": null
  }
}
```

`-->`
```json
{
  "id": 4,
  "jsonrpc": "2.0",
  "method": "tasks/get",
  "params": {
    "taskId": "job-1"
  }
}
```

`<--`
```json
{
  "id": 4,
  "jsonrpc": "2.0",
  "result": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "result": {
      "content": [
        {
          "text": "Applied export_vrm (log #1, transaction 1, document version 0): no values changed.\nEstimated cost: about 12 s, 2048 MiB of GPU memory.\nTo revert, call history_undo.\nOutput:\n{\n  \"file\": \"mira.vrm\"\n}",
          "type": "text"
        }
      ],
      "isError": false,
      "resultType": "complete",
      "structuredContent": {
        "applied": true,
        "cost": {
          "gpu_mib": 2048,
          "seconds": 12.0,
          "usd": null
        },
        "log": {
          "seq": 1,
          "tx": 1,
          "version": 0
        },
        "mode": "apply",
        "ok": true,
        "op": "export_vrm",
        "output": {
          "file": "mira.vrm"
        }
      }
    },
    "resultType": "complete",
    "status": "completed",
    "taskId": "job-1",
    "ttlMs": null
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
      "character": "mira"
    },
    "name": "export_vrm"
  }
}
```

`<--`
```json
{
  "id": 5,
  "jsonrpc": "2.0",
  "result": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "resultType": "task",
    "status": "working",
    "statusMessage": "waiting for a worker",
    "taskId": "job-2",
    "ttlMs": null
  }
}
```

`-->`
```json
{
  "id": 6,
  "jsonrpc": "2.0",
  "method": "tasks/cancel",
  "params": {
    "taskId": "job-2"
  }
}
```

`<--`
```json
{
  "id": 6,
  "jsonrpc": "2.0",
  "result": {
    "resultType": "complete"
  }
}
```

`-->`
```json
{
  "id": 7,
  "jsonrpc": "2.0",
  "method": "tasks/get",
  "params": {
    "taskId": "job-2"
  }
}
```

`<--`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/tasks",
  "params": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "status": "cancelled",
    "taskId": "job-2",
    "ttlMs": null
  }
}
```

`<--`
```json
{
  "id": 7,
  "jsonrpc": "2.0",
  "result": {
    "createdAt": "<time>",
    "lastUpdatedAt": "<time>",
    "pollIntervalMs": 1000,
    "resultType": "complete",
    "status": "cancelled",
    "taskId": "job-2",
    "ttlMs": null
  }
}
```
