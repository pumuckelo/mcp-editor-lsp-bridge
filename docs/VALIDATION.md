# Validation

Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, Unicode unit tests, the real integration test, the Rust release build, and the Zed WebAssembly release build. The integration test verifies definitions, dirty editor overlays, authoritative disk writes, multi-file rename, resolved code actions, filesystem notifications, compiler diagnostics, shared MCP clients, companion exit, reconnect suppression of superseded buffers, and separate workspace analyzers. Browser validation attached the demo workspace and visually checked the dashboard.

Automated checks are recorded in SDS. The integration fixture exercises a real rust-analyzer and the real companion subprocess, not a mock language server.

## Manual Zed check

1. Start the core, install the development extension, and configure the companion binary.
2. Open `examples/demo` as its own Zed workspace. Open `src/greeting.rs`.
3. Confirm **Zed · Connected** in the dashboard and a `zed` document source.
4. With autosave off, change `hello` to `hello_unsaved` without saving. Query `document_symbols`; it should report the unsaved name.
5. Have an agent edit the same file on disk. Query again; the disk contents should win and the dashboard should show `disk` for that document. Discard the editor's stale changes.
6. Make another fresh editor edit and verify synchronization resumes. Save and close the file.
7. Close the demo workspace. Confirm standalone semantic tools continue working.

The extension has been compiled for `wasm32-wasip1`. Native Zed UI automation could read the window but could not reliably operate keyboard/menu actions; the live-editor portion is being checked manually by the user. Do not confuse subprocess protocol validation with completed live Zed validation.

## CLI/application follow-up — 2026-09-09

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo build --release` passed.
- `cargo test --test integration -- --ignored --nocapture` passed with real rust-analyzer, Cargo, filesystem notifications and local sockets. CLI and MCP returned the same definition and analyzer PID. A code action listed by MCP was applied by CLI, and one listed by CLI was applied by MCP. Rename preview left files unchanged; CLI diagnostics awaited a successful compiler check. Existing companion reconnect/disk precedence and workspace isolation checks still passed.
- CLI transport tests covered nested Cargo member inference, explicit workspace selection, flags/JSON/stdin, schema discovery, invalid input, and unavailable core. Socket tests require local network permissions in a sandbox.
- Installed `bridge` and `mcp-editor-lsp-bridge` on Cargo PATH. Restarted the existing shared core on 47831. Installed CLI reported analyzer readiness and a connected Zed companion; a stdin document-symbol query returned `hello` from the demo. Its existing test syntax error was correctly reported and left intact.
- Concise `skills/editor-lsp-bridge/SKILL.md` validated with the skill-creator validator and linked into Codex and Pi skill directories.

### CLI rename apply verification — 2026-09-09

Extended the real-analyzer integration test to invoke the CLI executable with `rename --apply`: renamed `result` to `cli_renamed_result`, asserted exact definition and call-site changes in `src/helper.rs` and `src/lib.rs`, confirmed CLI references included both files, and awaited a successful compiler check via `diagnostics --check`. Renamed back using CLI JSON input with `apply: true` and verified both files matched their original contents exactly. The complete integration test passed (13.11s), as did Clippy with warnings denied. All mutations ran in a temporary fixture workspace cleaned up by the test.

### Workspace lifecycle controls

Real-analyzer integration passed explicit CLI disconnect/reconnect, rejection of repeated companion reattachment attempts while stopped, isolation of another workspace, and cancellation of a running Cargo check plus its build-script child. Full unit/CLI tests, Clippy with warnings denied, formatting, release build and installation passed. Dashboard buttons were exercised against the demo: Disconnect removed its active analyzer card and showed Reconnect; reconnect created a new analyzer PID while the other workspaces retained their PIDs. The existing dashboard tab was refreshed. Disconnect choices are in-memory and reset on core restart.

### Native TypeScript 7 support

The native TypeScript 7.0.2 integration exercised the real CLI and companion executable: workspace inference, symbols, hover, definitions, references, rename preview and cross-file apply, TSX/JSX files, code-action discovery, unsaved buffer diagnostics, disk-over-overlay precedence, saved-file error/fix checks, and mixed-language shutdown/reconnect. Existing real Rust integration also passed. Protocol tests are not a claim that Zed's updated development-extension manifest has been reloaded in the user's editor.

During implementation the running Rust bridge was used for semantic symbols/references, a real helper rename, and shared compiler diagnostics. Follow-up observations:

- The connection error suggests starting a core even when sandbox localhost access is the cause. Preserve and explain the underlying network error before suggesting startup.
- Document-symbol output is verbose and supplies whole declaration ranges, making exact rename positions awkward. Optional compact output and selection ranges would reduce agent work.
- Existing stale-edit/transactional protection and command-based code actions remain future work; they were not expanded in this language-support change.

The TypeScript launcher can wrap a native child; this change stops owned process groups so workspace disconnect does not leave that child running.

### Agent workflow improvements (0.2)

The full 11-test suite passed: six unit tests, two CLI transport tests, and real Rust, native TypeScript, and guarded-refactoring integrations. Coverage includes default-apply rename by symbol, compact and verbose receipts, relative file paths, exact CLI-preview/MCP-apply reuse, preserved unrelated declarations, ambiguity without mutation, changed unopened files, new files, unsaved overlays, stale LSP versions, and concurrent single-use plan application. Unit tests cover error classification, diagnostic uncertainty, unsupported resource edits, duplicate document groups and staging failure cleanup. Formatting and Clippy with warnings denied passed.

The running Rust bridge was used for semantic navigation, compiler diagnostics and a real cross-file helper rename during implementation. Closed-file snapshot differences are forwarded before requesting edits so delayed watcher events do not leave the server reading an old file.

Refactor guards serialize bridge/editor events and reject observed changes. They do not claim a multi-file OS transaction against other processes; a final external write race or filesystem replacement failure remains possible and is documented.

### Selectable TypeScript backends

Real CLI tests with TypeScript 5.9.3 and 6.0.3 exercise vtsls, switching to typescript-language-server and back, cross-file rename with disk assertions, unsaved companion diagnostics, preservation of companion buffers across switches, saved-file checks, mixed Rust/TypeScript workspaces, and disconnect/reconnect. A rejected native backend on TS5 leaves the current analyzer running. The native TS7 integration remains covered separately. Incremental document changes now use an explicit UTF-16 range over the old text when requested by the server.

To run the wrapper integration, set `BRIDGE_TYPESCRIPT_PACKAGE` to the TypeScript package directory, `BRIDGE_VTSLS` to the vtsls executable, and `BRIDGE_TSLS` to typescript-language-server, then run `cargo test --test typescript legacy_typescript_backends_and_switching -- --ignored`. These are test inputs, not production configuration.
