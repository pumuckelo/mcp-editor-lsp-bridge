# MCP editor LSP bridge

A small Rust service that gives coding agents semantic Rust and TypeScript tools through a shared HTTP MCP endpoint or a thin CLI client. It shares language servers per workspace directory: rust-analyzer for Rust and the native TypeScript 7 LSP for TypeScript/JavaScript. An optional Zed companion sends live editor buffers, including unsaved edits. Zed keeps its own analyzer; the companion does not analyze or index code.

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

Workspaces can also be attached from the dashboard, an MCP tool call, or the Zed companion. Use the directory containing `Cargo.toml`, `tsconfig.json`, `jsconfig.json` or `package.json`. Distinct worktree directories get distinct analyzers. Canonical aliases of a directory share the same session.

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

From anywhere inside a Rust or TypeScript workspace:

```sh
bridge workspace-status
bridge workspace-symbols --query MyType
bridge definition --path src/lib.rs --line 12 --character 8
bridge references --path src/lib.rs --line 12 --character 8
bridge diagnostics --check
bridge rename --path src/lib.rs --line 12 --character 8 --new-name BetterName
bridge rename --path src/lib.rs --line 12 --character 8 --new-name BetterName --preview
bridge apply-rename --plan-id RETURNED_ID
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

`src/application.rs` defines typed requests, generates input schemas, and implements semantic operations and the shared action cache. `src/mcp.rs` and the `/api/execute` HTTP handler are inbound adapters over the same `Arc<Application>`. `src/cli.rs` handles shell input, workspace inference, and HTTP calls. `Core` owns workspace state; its LSP/process/filesystem components perform the actual work. There is no second copy of the semantic implementation in the CLI or MCP adapter.

The HTTP adapter accepts `{ "operation": "definition", "arguments": { ... } }`, and returns `{ "data": ... }` or `{ "error": { "code", "message" } }`. Input validation occurs before workspace attachment. All operations share their schemas across MCP, CLI discovery, and HTTP parsing. LSP result bodies remain JSON because their shapes vary by operation and language-server capability.

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

The binary override is unnecessary if `mcp-editor-lsp-bridge` is on Zed's PATH and you use the default port. Restart language servers after changing the configuration. Open the workspace root as the Zed project; a parent folder without a supported project manifest is not supported.

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
| `definition`, `references`, `hover` | `path` and either `symbol` or `line` + `character` |
| `diagnostics` | optional `check: true` to await a current shared compiler check |
| `rename` | `path`, `new_name`, and either `symbol` or `line` + `character`; applies by default; `preview: true` returns a guarded plan |
| `code_actions` | `path`, `line`, `character`; optional `end: {line, character}` |
| `apply_rename` | `plan_id` returned by rename preview |
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
- Refactors verify snapshots and document versions, then stage replacements before writing. Multi-file writes are not an OS transaction: external tools can still race the final check/replacement, and a replacement failure can leave partially applied files. Errors report these cases; inspect disk before retrying.
- No proxy or reuse of Zed's analyzer. No VS Code companion yet.
- No workspace resource budgets, idle shutdown, persistent sessions, or automatic analyzer restart. Stop the core with Ctrl-C and restart after an analyzer crash.
- Workspace file watching covers supported source files and common project manifests. External dependencies and arbitrary build-script inputs are not watched; reconnect after those change.
- The UI is static HTML/CSS/JavaScript embedded in the Rust binary. No frontend build tool or Node process is needed for the dashboard. The npm TypeScript launcher may require Node.

The concise agent skill lives at `skills/editor-lsp-bridge/SKILL.md`. It supports MCP-capable agents and shell-only agents such as Pi, and is linked into this machine's Codex and Pi skill directories.

## Stop and reconnect workspaces

Use **Disconnect** on an active workspace in the dashboard to stop its analyzer, close its file watcher, and cancel its compiler checks. On Unix, cancellation also kills the check's build-script/compiler process group. Other workspaces remain running. Disconnected workspaces appear in a separate list with a **Reconnect** button.

```sh
bridge workspace-disconnect
bridge workspace-connect
bridge workspace-status --all
```

Both commands infer the workspace from cwd or accept `--workspace PATH`. First-time queries may still attach new workspaces automatically. After an explicit disconnect, ordinary queries and companion notifications cannot reattach that workspace: reconnect explicitly through CLI or the dashboard. Disconnect also clears cached code actions for that workspace. Closing the editor only disconnects its companion, not the standalone analyzer.

Disconnected-workspace choices are held in memory for the lifetime of the core; restarting the core clears them. No files or editor buffers are deleted. Already completed edits remain on disk; disconnect does not roll back operations.

## Native TypeScript 7

Install `typescript@^7` in the target project using its package manager. The bridge resolves `node_modules/.bin/tsc` in that project or its ancestors up to the Git boundary, verifies version 7, then starts `tsc --lsp --stdio`. TypeScript 5/6 and wrapper language servers are intentionally unsupported. To use a separate native installation, configure `typescript_analyzer` with its executable path; `typescript_settings` holds its LSP settings. No downloads happen automatically.

CLI inference picks the nearest Cargo/TypeScript/JavaScript project marker, stopping at a Git boundary; Cargo members still resolve to their Cargo workspace. Use `--workspace` when a monorepo needs a particular root.

```sh
bridge document-symbols --path src/example.ts
bridge references --path src/example.ts --line 0 --character 17
bridge rename --path src/example.ts --line 0 --character 17 --new-name newName --apply
bridge diagnostics --check
```

The same commands support `.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, and `.cjs`. File requests route to the right server. Mixed workspaces start their default server on attachment and add the other language server when a file needs it. Workspace symbols search active servers. Status exposes an `analyzers` array; Disconnect stops every server and check for that workspace. Server-specific startup and language policy live in `src/language.rs`, separate from CLI/MCP/HTTP adapters.

TypeScript live diagnostics are pulled for bridge-opened documents, including companion overlays. `ready` means initialization completed, not that every file has been checked. `--check` awaits a shared, cached `tsc --noEmit --pretty false --project tsconfig.json` (or `jsconfig.json`) for saved files. In a mixed workspace, checks run for active languages. A workspace with only `package.json` supports navigation but must add/select a root tsconfig/jsconfig for saved-file checks. Check scope follows that configuration, including its exclusions; this is not a replacement for repository-specific build/test scripts. TypeScript check messages are returned with `source: "typescript"` and raw compiler text.

The watcher includes supported source files and common Rust/TypeScript manifests; it ignores `.git`, `target`, `node_modules`, `dist`, and `.next`. Changes to external dependencies are outside the bridge watcher; reconnect after dependency installation if server state is stale.

For Zed, reload/reinstall the development extension after updating its manifest, then add the companion alongside the editor's default servers:

```json
{
  "languages": {
    "TypeScript": { "language_servers": ["...", "editor-lsp-bridge"] },
    "TSX": { "language_servers": ["...", "editor-lsp-bridge"] },
    "JavaScript": { "language_servers": ["...", "editor-lsp-bridge"] }
  }
}
```

The existing companion binary configuration and open/change/save/close protocol are unchanged. Zed continues using its own server; the companion only forwards buffers.

Run both real-server integration tests with:

```sh
BRIDGE_TYPESCRIPT=/absolute/path/to/typescript7/tsc cargo test --all-targets -- --include-ignored
```

## Agent workflow improvements (0.2)

**Breaking default:** update/restart the shared core together with the CLI when upgrading to 0.2. `rename` now writes changes unless `preview: true` / `--preview` is supplied. The same semantics apply to CLI, HTTP and MCP. `--apply` / `apply: true` remains accepted; explicit JSON `apply: false` retains legacy preview behavior. A preview no longer returns a raw WorkspaceEdit: it returns a receipt and `planId`.

```sh
bridge rename --path src/lib.rs --symbol old_name --new-name new_name
bridge rename --path src/lib.rs --symbol old_name --new-name new_name --preview
bridge apply-rename --plan-id RETURNED_ID
bridge diagnostics --check
bridge workspace-status --all
bridge workspace-status --all --verbose
```

`--symbol` also works for definition, references and hover. Selection uses the language server's declaration selection range, not text search. Ambiguous or missing names return `applied: false`, a reason and compact candidates without editing. Explicit positions remain zero-based UTF-16; do not mix them with a symbol selector.

Rename/apply receipts contain `applied`, `newName`, `fileCount`, `editCount` and `files` with workspace-relative paths, edit counts and zero-based lines from the original text. `symbol` is included when known. `--verbose` adds exact ranges and old/new text. Edit count is not necessarily reference count.

Status omits document/configuration dumps by default. Diagnostics omit clean-file entries and report live/saved diagnostic counts while retaining check success (including null), errors, running state and freshness. Zero live diagnostics never establishes workspace correctness. Document symbols expose compact names, containers and positions; workspace symbols include relative paths. `verbose: true` in API/JSON, or `--verbose` with flags/JSON/stdin, returns full details.

Preview plans are single-use, workspace-bound, expire after five minutes, and disappear on disconnect or restart. Up to eight plans are retained. Applying uses the stored edits without asking the language server to calculate the rename again. Code actions have the same expiry and stale-input protection, with a cache capped at 128 actions.

Before calculating edits, the bridge fingerprints supported workspace source/configuration files and records open-buffer versions/content. Before writing, it checks those snapshots and any LSP document versions. Changes elsewhere in the tracked workspace can invalidate a plan because they can change references. Dependency/build trees and symlink traversal are excluded; unsupported edit targets are rejected. This adds bounded filesystem reads during refactoring, not a second index. Snapshot limits are 100,000 files / 256 MiB read; prepared edits are capped at 16 MiB per operation. Choose a smaller workspace if these limits are exceeded.

Errors distinguish `PERMISSION_DENIED` (sandbox/OS localhost restriction), `CONNECTION_REFUSED` (no listener), `TIMED_OUT`, and other `UNAVAILABLE` failures. The underlying cause is preserved. Timeout or lost response after a mutation requires inspecting disk before retrying.
