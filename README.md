# MCP editor LSP bridge

A small Rust service that gives coding agents semantic Rust tools through a shared HTTP MCP endpoint or a thin CLI client. It owns a dedicated rust-analyzer per workspace directory. An optional Zed companion sends live editor buffers, including unsaved edits. Zed keeps its own analyzer; the companion does not analyze or index code.

## Run

Requires a current Rust toolchain, Cargo, and `rust-analyzer` on PATH. Install the analyzer with `rustup component add rust-analyzer` if necessary.

```sh
cargo build --release
./target/release/mcp-editor-lsp-bridge serve
```

Open **http://127.0.0.1:47831/** for the dashboard. Connect agent clients to **http://127.0.0.1:47831/mcp** using Streamable HTTP. Start the service once, not once per agent. The service binds loopback only and rejects foreign browser origins and hostnames.

Optional arguments:

```sh
./target/release/mcp-editor-lsp-bridge serve --port 47831 --workspace /absolute/path/to/project --config bridge.json
```

Workspaces can also be attached from the dashboard, an MCP tool call, or the Zed companion. Use the directory containing your workspace's `Cargo.toml`. Distinct worktree directories get distinct analyzers. Canonical aliases of a directory share the same session.

To install the executable on PATH:

```sh
cargo install --path . --locked
```

A generic MCP client configuration (the enclosing key depends on the client):

```json
{
  "mcpServers": {
    "editor-lsp-bridge": {
      "url": "http://127.0.0.1:47831/mcp"
    }
  }
}
```

This service is HTTP-only. It intentionally has no per-agent stdio server launcher.

## CLI for agents (including Pi)

`cargo install --path . --locked` installs both `bridge` and the existing `mcp-editor-lsp-bridge` executable. Both support the same commands. Run the core once with `bridge serve`; subsequent commands connect to it and exit. They never launch an analyzer or a second core.

From anywhere inside a Cargo workspace:

```sh
bridge workspace-status
bridge workspace-symbols --query MyType
bridge definition --path src/lib.rs --line 12 --character 8
bridge references --path src/lib.rs --line 12 --character 8
bridge diagnostics --check
bridge rename --path src/lib.rs --line 12 --character 8 --new-name BetterName
bridge rename --path src/lib.rs --line 12 --character 8 --new-name BetterName --apply
bridge code-actions --path src/lib.rs --line 12 --character 8
bridge apply-code-action --action-id RETURNED_ID
bridge tools
bridge schema code-actions
```

Lines and characters are **zero-based UTF-16**, the same as MCP. Ordinary `--path` values are relative to your current directory. JSON paths retain the MCP convention: workspace-relative paths, absolute paths or file URIs. Hyphenated command names also accept their MCP underscore names.

Complex calls accept either `--json` or piped `--stdin`:

```sh
bridge code-actions --json '{"path":"src/lib.rs","line":12,"character":8,"end":{"line":14,"character":0}}'
printf '%s' '{"path":"src/lib.rs","line":12,"character":8}' | bridge hover --stdin
```

Workspace inference uses `cargo locate-project --workspace` from cwd, including workspace members and separate worktree directories. This reads Cargo metadata; it does not index or compile the project. Use `--workspace /path/to/workspace` to override inference, or include `workspace` in JSON. Conflicting JSON and flag workspaces are rejected. Use `bridge workspace-status --all` to list sessions from any directory without inference.

Use `--endpoint http://127.0.0.1:PORT` for another local core. JSON/stdin cannot be mixed with operation flags except `--workspace` and `--endpoint`. Output is the same result JSON as MCP; logs and `{ "error": { "code", "message" } }` go to stderr. Exit codes: 0 success, 2 invalid input, 1 unavailable core or failed operation. No automatic mutation retries. Code-action IDs can be listed through one interface and applied through the other; they expire when the core restarts.

### Shared application boundary

`src/application.rs` defines typed requests, generates input schemas, and implements semantic operations and the shared action cache. `src/mcp.rs` and the `/api/execute` HTTP handler are inbound adapters over the same `Arc<Application>`. `src/cli.rs` handles shell input, Cargo workspace inference, and HTTP calls. `Core` owns workspace state; its LSP/process/filesystem components perform the actual work. There is no second copy of the semantic implementation in the CLI or MCP adapter.

The HTTP adapter accepts `{ "operation": "definition", "arguments": { ... } }`, and returns `{ "data": ... }` or `{ "error": { "code", "message" } }`. Input validation occurs before workspace attachment. All ten operations share their schemas across MCP, CLI discovery, and HTTP parsing. LSP result bodies remain JSON because their shapes vary by operation and rust-analyzer capability.

## Zed companion

1. Build/install the executable above.
2. In Zed, run **zed: install dev extension** and select this repository's `zed-extension` directory. Zed builds the extension using Rust's `wasm32-wasip1` target. If needed: `rustup target add wasm32-wasip1`.
3. Add the companion to your project's Zed settings alongside rust-analyzer:

```json
{
  "languages": {
    "Rust": {
      "language_servers": ["rust-analyzer", "editor-lsp-bridge"]
    }
  },
  "lsp": {
    "editor-lsp-bridge": {
      "binary": {
        "path": "/absolute/path/to/mcp-editor-lsp-bridge/target/release/mcp-editor-lsp-bridge",
        "arguments": ["companion", "--endpoint", "http://127.0.0.1:47831"]
      }
    }
  }
}
```

The binary override is unnecessary if `mcp-editor-lsp-bridge` is on Zed's PATH and you use the default port. Restart language servers after changing the configuration. Open the Cargo workspace as the Zed project; a parent folder without `Cargo.toml` is not supported in this MVP.

The dashboard shows **Zed · Connected** when the companion registers. The companion retries while the core is unavailable. It sends full snapshots from its local editor cache, even when it receives incremental LSP changes. Closing the companion restores saved-file state; an unclean disconnect is detected by heartbeats.

Only one companion can register per workspace. VS Code and companion selection are deferred, but the HTTP synchronization protocol is editor-independent.

### Agent edits take precedence

A saved-file change replaces any conflicting editor overlay in our analyzer. The core increments a document epoch; queued snapshots using the previous epoch are rejected. The companion acknowledges the new epoch without replaying the rejected snapshot. A subsequent fresh edit can establish a new overlay.

This does **not** discard your editor buffer or force a save. If Zed shows a conflict, discard your old unsaved changes as usual. Explicitly saving those edits later is a new disk write. Multi-file rename and code-action tools write directly to disk.

## Tools

Positions are **zero-based lines and UTF-16 characters**, as in LSP. Files can be workspace-relative paths, absolute paths, or file URIs within the workspace.

| Tool | Inputs in addition to `workspace` |
|---|---|
| `workspace_status` | none; omit workspace to list sessions |
| `workspace_symbols` | `query` |
| `document_symbols` | `path` |
| `definition`, `references`, `hover` | `path`, `line`, `character` |
| `diagnostics` | optional `check: true` to await a current shared Cargo check |
| `rename` | `path`, `line`, `character`, `new_name`; optional `apply: true` (otherwise preview) |
| `code_actions` | `path`, `line`, `character`; optional `end: {line, character}` |
| `apply_code_action` | `action_id` returned by `code_actions` |

Use navigation tools rather than guessing symbol locations. Ask for diagnostics after edits. Transient analyzer content-modified errors are retried; other failures are returned as readable MCP tool errors.

## Diagnostics and configuration

The core separates rust-analyzer's live diagnostics from Cargo's compiler diagnostics. It runs one shared `cargo check --workspace --all-targets --message-format=json` after saved-file changes settle, and caches the result until another observed disk change. Concurrent requests reuse this check. The dedicated analyzer's own check-on-save is disabled to avoid running a duplicate check inside the same core; Zed's separate analyzer may still run its own check.

`ready` reports analyzer indexing state. `checkFresh` reports whether a completed compiler check matches the observed disk generation. A failed process, a running check, or an uninitialized cache is not a clean result. `matchesDocumentVersion` accompanies live diagnostic publications; missing versions cannot be certified current. Cargo checks saved files, not unsaved editor overlays.

Example `bridge.json`:

```json
{
  "analyzer": "rust-analyzer",
  "cargo_features": [],
  "all_features": false,
  "no_default_features": false,
  "cargo_target": null,
  "environment": {},
  "analyzer_settings": {
    "procMacro": {"enable": true}
  }
}
```

The core inherits your environment and workspace toolchain selection. Use `environment` for explicit overrides such as `RUSTUP_TOOLCHAIN`. Environment values are not exposed in the dashboard. Cargo features/target are applied to both analysis and checks; `checkOnSave` is controlled by the core. Configuration is startup-wide in this MVP; restart to change it. It is not automatically copied from Zed.

## Development and validation

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test integration -- --ignored --nocapture
cargo build --manifest-path zed-extension/Cargo.toml --target wasm32-wasip1 --release
```

The ignored integration test requires a real rust-analyzer, local HTTP sockets, filesystem notifications, and Cargo subprocesses. It uses temporary fixture workspaces. Sandboxed environments that suppress native file notifications must run that test with appropriate local permissions.

`examples/demo` is a tiny standalone Rust workspace for manual Zed testing. See [the companion protocol](docs/companion-protocol.md) and [validation notes](docs/VALIDATION.md).

## MVP limits

- Command-based code actions and file create/rename/delete operations are rejected before applying edits. Text edits and multi-file symbol rename are supported.
- No transactional or stale-version refactoring protection yet; apply previews promptly. Disk write failures can leave a partially applied multi-file edit and are reported.
- No proxy or reuse of Zed's analyzer. No TypeScript or VS Code companion yet.
- No workspace resource budgets, idle shutdown, persistent sessions, or automatic analyzer restart. Stop the core with Ctrl-C and restart after an analyzer crash.
- Workspace file watching covers Rust files and common Cargo/toolchain configuration under the workspace, excluding `target` and `.git`. External path dependencies and non-Rust build-script inputs are not watched in this MVP; restart the core after those change.
- The UI is static HTML/CSS/JavaScript embedded in the Rust binary. No frontend build tool or Node process is needed.

The concise agent skill lives at `skills/editor-lsp-bridge/SKILL.md`. It supports MCP-capable agents and shell-only agents such as Pi, and is linked into this machine's Codex and Pi skill directories.
