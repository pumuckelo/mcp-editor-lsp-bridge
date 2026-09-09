# Editor LSP Bridge

Shared Rust and TypeScript language intelligence for coding agents, through a small **CLI**. Run one local core, then use `bridge` for navigation, diagnostics and refactoring. Agents reuse one language server per workspace instead of starting their own.

Supports **rust-analyzer** and the **native TypeScript 7 LSP** (including JavaScript). An optional Zed companion forwards unsaved editor buffers. MCP is optional; no MCP configuration is needed for the CLI.

## Install

Download prebuilt executables for macOS or Linux (Apple Silicon/ARM64 and x64):

```sh
curl -fsSL https://github.com/pumuckelo/mcp-editor-lsp-bridge/releases/latest/download/install.sh | sh
```

This requires a published GitHub Release; the first release becomes available after the release workflow is pushed and a version tag is published. The installer verifies the archive checksum and installs into `~/.local/share/editor-bridge` with commands in `~/.local/bin`. Add that directory to your PATH. No Rust compiler is required to run the prebuilt executables.

Run the same command to upgrade. Use `INSTALL_VERSION=vX.Y.Z`, `INSTALL_ROOT`, or `INSTALL_BIN_DIR` on the installer process to select a version or location. The installer does not edit shell profiles or repository instructions.

### Agent setup

The bundled [usage skill](skills/editor-lsp-bridge/SKILL.md) has a separate [setup reference](skills/editor-lsp-bridge/references/setup.md). Install the skill in your agent's supported skill directory, then ask it to install and use Editor LSP Bridge in your project. Setup guidance covers installation and adding a short note to the project's `AGENTS.md` and `CLAUDE.md` when adopting the tool. Normal usage does not load the setup reference.

For agent-led setup, give your agent the [setup reference](https://github.com/pumuckelo/mcp-editor-lsp-bridge/blob/main/skills/editor-lsp-bridge/references/setup.md) and ask it to install and use the tool in your repository. It can install the bundled usage skill as part of that setup.

### Build from source

With a current Rust toolchain, run `cargo install --path . --locked` from this repository. Both installation methods provide `bridge` and `mcp-editor-lsp-bridge`.

Rust workspaces still need their Rust development toolchain and `rust-analyzer` (`rustup component add rust-analyzer`).

For TypeScript/JavaScript projects, install **TypeScript 7** in the target project using its package manager, for example:

```sh
npm install --save-dev typescript@^7
```

The bridge verifies version 7 and runs the project's `tsc --lsp --stdio`. It does not download servers automatically or fall back to older TypeScript servers. A separately installed native server can be selected with `typescript_analyzer` in the core configuration.

## Start once

Keep this running in a terminal:

```sh
bridge serve
```

Open [the dashboard](http://127.0.0.1:47831/) to manage workspaces. It supports light, dark and system themes. The core binds to loopback and rejects foreign browser origins and hostnames.

In another terminal, go to your project:

```sh
cd /path/to/project
bridge workspace-connect
bridge workspace-status
bridge diagnostics --check
```

CLI commands connect to the running core and exit. They never start a second core. First-time queries can attach new workspaces automatically; explicit connection is useful for setup.

Optional startup settings:

```sh
bridge serve --port 47831 --workspace /path/to/project --config bridge.json
```

Use `--endpoint http://127.0.0.1:PORT` on CLI commands when using another port. After upgrading, restart the core too so the CLI and core use the same version.

## Use from your agent or shell

Run commands from inside the target workspace. The same interface works for shell-based agents such as Pi; no MCP support is required. The concise [agent skill](skills/editor-lsp-bridge/SKILL.md) documents the workflow for your harness.

```sh
bridge workspace-symbols --query MyType
bridge document-symbols --path src/lib.rs
bridge definition --path src/lib.rs --symbol my_function
bridge references --path src/lib.rs --symbol my_function
bridge hover --path src/lib.rs --symbol my_function
bridge diagnostics --check
```

Use `.ts`/`.tsx`/`.js` paths in TypeScript projects. `--symbol` selects a declaration through the language server. Missing or ambiguous names return candidates; use exact positions when needed:

```sh
bridge definition --path src/lib.rs --line 12 --character 8
bridge tools
bridge rename --help
bridge schema code-actions
```

Positions are **zero-based lines and UTF-16 characters**. Ordinary `--path` arguments are relative to your current directory. JSON paths are workspace-relative, absolute, or file URIs within the workspace.

Workspace inference chooses the nearest supported Cargo/TypeScript/JavaScript project marker, stopping at Git boundaries. Cargo members resolve to their Cargo workspace. Use `--workspace /path/to/root` for a particular monorepo root. Different Git worktree directories get separate analyzers; canonical aliases share a session.

### Rename and code actions

**Rename applies immediately by default**, including references across files:

```sh
bridge rename --path src/lib.rs --symbol old_name --new-name new_name
bridge diagnostics --check
```

When scope needs inspection, preview first and apply that exact plan:

```sh
bridge rename --path src/lib.rs --symbol old_name --new-name new_name --preview
bridge apply-rename --plan-id RETURNED_ID
```

Receipts report changed paths, edit counts and original line positions. `--verbose` adds ranges and old/new text. `--apply` remains an optional compatibility alias; JSON `apply: false` also requests a preview.

```sh
bridge code-actions --path src/lib.rs --line 12 --character 8
bridge apply-code-action --action-id RETURNED_ID
```

Preview plans are single-use, workspace-bound and expire after **five minutes**. Code actions also expire after five minutes. Disconnecting or restarting clears both caches. The core retains at most eight preview plans and 128 code actions.

Refactors check disk snapshots and editor versions before applying. If content changed, inspect the changes and request a fresh plan. Writes are staged, but are not a fully atomic multi-file transaction against external tools; inspect disk after a partial failure or lost response before retrying.

### JSON inputs and output

Complex inputs accept inline JSON or stdin:

```sh
bridge code-actions --json '{"path":"src/lib.rs","line":12,"character":8,"end":{"line":14,"character":0}}'
printf '%s' '{"path":"src/lib.rs","symbol":"my_function"}' | bridge hover --stdin
```

Do not mix JSON/stdin with operation flags, except `--workspace`, `--endpoint` and supported `--verbose`. Conflicting workspace values are rejected. Commands return compact JSON on stdout; `--verbose` expands supported results. Errors go to stderr as `{ "error": { "code", "message" } }`. Exit codes: **0** success, **2** invalid input, **1** connection/operation failure. Always inspect the result: ambiguous symbol selection can return `applied: false` without an error exit.

## Manage workspaces

```sh
bridge workspace-status --all
bridge workspace-disconnect
bridge workspace-connect
```

The dashboard's **Disconnect** stops that workspace's analyzers, file watcher and compiler checks. Other workspaces continue running. After explicit disconnection, queries and companions cannot silently reattach it; use **Reconnect** or `workspace-connect`.

Closing Zed disconnects its companion, **not** the standalone analyzer. Disconnect workspaces you no longer need, or stop the core with Ctrl-C. Disconnected choices are in memory and reset when the core restarts. No source files are deleted or edits rolled back.

## Optional: unsaved buffers from Zed

Standalone operation reads saved files. To include unsaved buffers:

1. Install the executables above.
2. Install Rust via rustup for this optional development-extension build. In Zed, run **zed: install dev extension** and select `~/.local/share/editor-bridge/current/zed-extension` when using the release installer (adjust for `INSTALL_ROOT`). No repository clone is needed. For a source checkout, select its `zed-extension` directory instead. Let Zed build the extension in its required WebAssembly format; do not overwrite `extension.wasm` with a raw Cargo build artifact.
3. Add the companion alongside your existing servers in Zed settings:

```json
{
  "languages": {
    "Rust": { "language_servers": ["...", "editor-lsp-bridge"] },
    "TypeScript": { "language_servers": ["...", "editor-lsp-bridge"] },
    "TSX": { "language_servers": ["...", "editor-lsp-bridge"] },
    "JavaScript": { "language_servers": ["...", "editor-lsp-bridge"] }
  }
}
```

If Zed cannot find the executable on its PATH, add an absolute binary path, replacing the example with your own Cargo install location:

```json
{
  "lsp": {
    "editor-lsp-bridge": {
      "binary": {
        "path": "/home/you/.cargo/bin/mcp-editor-lsp-bridge",
        "arguments": ["companion", "--endpoint", "http://127.0.0.1:47831"]
      }
    }
  }
}
```

Open the actual workspace root in Zed and restart language servers after changing settings. Reload/reinstall the dev extension after updating its manifest. Run `bridge workspace-status` and verify `companionConnected: true`; analyzer readiness alone does not verify the companion. The dashboard shows **Zed companion connected** when registered. To verify unsaved-buffer forwarding, edit a supported file without saving and inspect `bridge workspace-status --verbose` for that document with `source: "zed"`, then run `bridge diagnostics`.

Zed retains its own analyzer. The companion only forwards open/change/save/close events and snapshots; it does not analyze or index code. The bridge runs its dedicated analyzer, so there are two analyzers, not a proxy or a third analyzer. One companion per workspace is supported; VS Code integration and companion switching are not implemented.

Agent disk edits take precedence over stale companion overlays in the bridge. This does not overwrite Zed's unsaved buffer or force a save: discard conflicting old editor changes as usual. A later explicit save is a new disk write. Heartbeats detect an unclean companion disconnect and restore saved-file state.

## Diagnostics and configuration

`bridge diagnostics` reads cached diagnostics; `bridge diagnostics --check` awaits a shared saved-file check. Check **readiness, freshness, running state, errors and success**. Zero live diagnostics alone does not prove the workspace is clean. Tests and project-specific builds remain necessary.

- **Rust:** the core shares `cargo check --workspace --all-targets --message-format=json` and caches it until observed disk changes. Its rust-analyzer check-on-save is disabled to avoid a duplicate check inside the core; Zed's own analyzer may still check separately.
- **TypeScript:** live diagnostics cover bridge-opened files and companion buffers. `ready` means initialization completed. Saved-file checks use `tsc --noEmit --pretty false --project tsconfig.json` (or `jsconfig.json`) at the workspace root. A package-only workspace supports navigation but needs a root configuration for checks. Scope follows that configuration's inclusions/exclusions.
- **Mixed workspaces:** file extensions route to the appropriate server; a second language server starts when needed. Workspace-symbol searches and compiler checks cover active languages. Disconnect stops all servers for the workspace.

Example `bridge.json`, passed to `bridge serve --config bridge.json`:

```json
{
  "analyzer": "rust-analyzer",
  "cargo_features": [],
  "all_features": false,
  "no_default_features": false,
  "cargo_target": null,
  "environment": {},
  "analyzer_settings": { "procMacro": { "enable": true } }
}
```

Optional `typescript_analyzer` specifies the native executable path; `typescript_settings` supplies its LSP settings. Configuration is startup-wide: restart to change it. It inherits the core's environment and workspace toolchain selection, not Zed's configuration. Environment override values are not exposed in the dashboard.

## Troubleshooting and limits

- **`PERMISSION_DENIED` / `Operation not permitted`:** an agent sandbox may block localhost even while the core is running. Retry the authorized command through the harness's permission flow. Do not start another core or change the bind address merely because the sandbox cannot connect.
- **`CONNECTION_REFUSED`:** check that `bridge serve` is running at the selected endpoint.
- **`TIMED_OUT` or a lost mutation response:** inspect disk before retrying; the edit may already have applied.
- Command-based code actions and file create/rename/delete operations are rejected; text edits and cross-file symbol renames are supported.
- No idle shutdown, resource budgets, persistent sessions or automatic analyzer restart. Restart the core after an analyzer crash.
- Watching covers supported source files and common manifests, excluding `.git`, `target`, `node_modules`, `dist` and `.next`. External dependencies and arbitrary build-script inputs are not watched; reconnect if analysis is stale after those change.
- Refactoring snapshots are bounded to 100,000 files / 256 MiB read, with prepared edits capped at 16 MiB. Changes elsewhere in the tracked workspace can invalidate a preview. Symlink traversal is excluded.
- The dashboard is static HTML/CSS/JavaScript embedded in the Rust executable: no frontend build tool or Node server. The npm TypeScript launcher may require Node.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
BRIDGE_TYPESCRIPT=/absolute/path/to/typescript7/tsc cargo test --all-targets -- --include-ignored
cargo build --manifest-path zed-extension/Cargo.toml --target wasm32-wasip1 --release
```

Real-server tests require rust-analyzer, native TypeScript 7, local sockets, filesystem notifications and compiler subprocesses. Use appropriate local permissions in restricted environments. `examples/demo` is a Rust fixture for manual Zed validation. See [validation notes](docs/VALIDATION.md) and the [companion protocol](docs/companion-protocol.md).

`src/application.rs` owns typed semantic operations and caches. The CLI sends requests to the HTTP inbound adapter; MCP is another adapter over the same application service. `Core` owns workspace state and LSP/process/filesystem behavior. `src/language.rs` contains server-specific policy. There is no duplicate semantic implementation per interface.

## Optional MCP interface

MCP is retained for harnesses that prefer native tool discovery. **It is not needed for the recommended CLI workflow.** Run the same core once, then configure a Streamable HTTP connection:

```json
{
  "mcpServers": {
    "editor-lsp-bridge": { "url": "http://127.0.0.1:47831/mcp" }
  }
}
```

Adapt the enclosing configuration to your harness. There is no per-agent stdio launcher. Tool names use underscores (`workspace_symbols`, `apply_rename`, `apply_code_action`) and share the CLI's schemas, edit behavior, caches and diagnostics. Discover the complete operations through the client or `bridge tools`.

The underlying HTTP adapter accepts `{ "operation": "definition", "arguments": { ... } }` at `/api/execute` and returns `{ "data": ... }` or `{ "error": { "code", "message" } }`. CLI paths are cwd-relative; adapter paths are workspace-relative. LSP result bodies remain JSON because their shapes vary by operation and server capabilities.

## Publishing releases

The GitHub Actions release workflow builds macOS and Linux executables for ARM64 and x64, packages the dashboard and skills, and publishes archives, SHA-256 checksums, and the installer. Manual workflow runs on a branch build downloadable artifacts without publishing a release.

Update the package version, commit and push the changes, then push a matching `vX.Y.Z` tag. The workflow checks the tag against `Cargo.toml`. GitHub's built-in token publishes the release; no npm account or separate publishing secret is required. The installer attached to the release is an asset for users, not a command executed by the publish step.

After committing the version and release changes, run from this repository:

```sh
cargo release              # bump if needed, commit the version, push branch and tag
```

Release requires a clean checkout and a branch. The helper fetches tags from origin. If the current version already tags an older commit, it selects the next unused patch version, updates Cargo.toml and Cargo.lock, and commits that version change. It then pushes the branch and tag together atomically. Existing tags are never moved. Repeating a release at the same commit reuses its tag; after a failed push, rerun `cargo release`. Set major, minor, or prerelease versions manually when needed.

Installer sources live in `scripts/installer/`; `scripts/install.sh` loads them when run from a checkout. `sh scripts/bundle-installer.sh > install.sh` produces the standalone release installer. Edit the source modules, not generated bundles.
