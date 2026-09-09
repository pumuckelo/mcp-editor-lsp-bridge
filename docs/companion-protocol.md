# Companion protocol v1

POST JSON to `/api/companion` on the same loopback server as MCP. This interface is independent of Zed; a future VS Code extension can implement it directly.

```json
{
  "workspace": "/absolute/cargo/workspace",
  "companion": "unique-id-for-this-editor-session",
  "kind": "change",
  "uri": "file:///absolute/cargo/workspace/src/lib.rs",
  "text": "pub fn example() {}\n",
  "epoch": 0
}
```

Fields `workspace`, `companion`, and `kind` are required. `uri` and `text` may be omitted/null for lifecycle events; `epoch` defaults to 0.

| kind | Meaning |
|---|---|
| `connect` | Register the sole active companion for this workspace |
| `heartbeat` | Refresh the connection every three seconds |
| `open` | Full editor snapshot, including unsaved text |
| `change` | Full updated snapshot; companion translates incremental LSP changes locally |
| `save` | Saved-file notification; the core independently reads disk |
| `close` | Stop using the editor overlay for this file |
| `disconnect` | Drop all editor overlays and return to disk |

Only `open` and `change` require `text`. A successful document event returns `{"accepted":true,"epoch":0}`. The companion stores the returned epoch per document. When disk has superseded an overlay, the response is `{"accepted":false,"epoch":1,"reason":"Disk edit superseded editor snapshot"}`. Store the new epoch but do not retry the rejected contents. A later editor change may send its new complete snapshot with that epoch.

Keep editor snapshots separate from the analyzer's effective contents: after a disk override, an incremental editor change is relative to the old editor buffer, not the new disk text. The supplied companion reconstructs the full editor text before sending it.

The core leases a connection for 15 seconds. Explicit disconnect is immediate. A second companion ID is rejected; selection/switching is outside the MVP. Non-success HTTP responses are failures and must not be treated as acknowledgements. Retry registration and replay known open-document snapshots after connection loss. A superseded snapshot must remain suppressed until a new editor edit; see validation notes for current reconnect coverage.

An open Zed buffer is not necessarily the focused tab. These events describe documents, not focus or active-file selection.
