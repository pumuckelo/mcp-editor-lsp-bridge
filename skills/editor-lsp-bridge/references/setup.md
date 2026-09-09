# Install and adopt Editor LSP Bridge

Read only for installation, explicit repository adoption, or a missing executable.
Normal language-tool usage does not require this file.

1. Check `command -v bridge` and `bridge --version`. Reuse a working installation unless an upgrade was requested.
2. Install the published macOS/Linux release if missing:

   ```sh
   curl --proto '=https' --tlsv1.2 -fsSL https://github.com/pumuckelo/mcp-editor-lsp-bridge/releases/latest/download/install.sh | sh
   ```

   The installer verifies the archive checksum. Executables go in `~/.local/bin`; release files and this skill are under `~/.local/share/editor-bridge/current`. `INSTALL_VERSION=vX.Y.Z`, `INSTALL_ROOT` and `INSTALL_BIN_DIR` are optional overrides. If no release exists, report it; use the README's source installation only when appropriate.
3. Ensure PATH includes the install directory. Run `bridge --version` and `bridge tools`. Prebuilt bridge binaries do not require Rust to execute.
4. Check language prerequisites: Rust projects need their Rust/Cargo toolchain and rust-analyzer; native TypeScript projects need TypeScript 7. Do not silently change a project's TypeScript dependency. Explain missing prerequisites and use the user's authorized installation scope.
5. Check `bridge workspace-status --all`. A sandbox permission error is not proof that the core is stopped; follow the usage skill's permission guidance. If no core is running, start `bridge serve` once in a suitable persistent terminal. Do not start one core per agent. An upgrade requires restarting the old core with its existing configuration and respecting disconnected workspaces.
6. In the chosen repository, run `bridge workspace-connect`, `bridge workspace-status`, then `bridge diagnostics --check`. Report analyzer/setup errors accurately. Standalone mode works on saved files; Zed is optional.
7. For explicit repository adoption, add concise bridge usage guidance to `AGENTS.md` and `CLAUDE.md`. Preserve unrelated instructions, reuse an existing equivalent section, and point at the usage `SKILL.md`, not this reference. Do not alter instructions merely to run a one-off query.
8. Install/copy the bundled `skills/editor-lsp-bridge` directory into the harness's supported skill location where appropriate. A portable alternative is `.agent-tools/editor-lsp-bridge` in the repository, referenced by both instruction files. Preserve user-edited skill copies.
9. The project note should tell agents to use `bridge` for supported-language navigation, refactors and diagnostics; reuse the shared core; and follow the usage skill's stale-edit and sandbox handling.

For requested Zed integration, follow the README's companion instructions. With the release installer, select `~/.local/share/editor-bridge/current/zed-extension` in Zed's **Install Dev Extension** action (adjust for `INSTALL_ROOT`); no clone is needed. Enable `editor-lsp-bridge` alongside existing language servers for every requested language, preserving other settings. Let Zed build the extension rather than copying a raw Cargo WASM artifact. Open a supported file, restart language servers, and verify `bridge workspace-status` reports `companionConnected: true`; `ready: true` alone is insufficient. Verify an unsaved edit reaches a document with `source: "zed"` in verbose status. The release includes `zed-extension` source, which Zed builds as a development extension; that optional step requires a Rust build toolchain. Do not automate editor installation or change editor settings unless included in the user's request.

The installer never edits project instructions or shell profiles. Do not configure MCP for the CLI workflow.
