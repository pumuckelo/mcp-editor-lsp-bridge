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
