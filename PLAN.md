# MCP editor LSP bridge

## Goal

Build a minimal Rust core that exposes a dedicated rust-analyzer to coding agents through MCP. It works with saved files by itself and optionally receives live document state from a Zed companion. The companion performs no analysis. With Zed running, there are two analyzers total: Zed's existing analyzer and the core's dedicated analyzer.

## Agreed scope and decisions

- One persistent Rust core owns the MCP endpoint and analyzer processes. Agents connect to the same endpoint; do not launch an MCP/core/analyzer process per agent. Use a shared local HTTP MCP transport.
- One dedicated rust-analyzer session per workspace directory. Different Git worktree directories are separate workspaces.
- Communicate with the installed rust-analyzer executable using LSP, not internal analyzer crates. No custom semantic index or database.
- Standalone mode reads disk and watches relevant file changes, creations, and deletions. Maintain LSP document state correctly when files are opened for agent queries.
- An optional Zed extension launches a lightweight companion LSP server for Rust alongside Zed's analyzer. The companion forwards open/change/save/close notifications and document contents or deltas to the core.
- Keep the companion protocol editor-neutral so a VS Code companion can be added later. Build only Zed now. Do not implement multi-companion selection or switching in the MVP.
- Agent edits on disk take precedence over conflicting unsaved editor contents. Do not refuse or delay agent work because an editor has unsaved changes. Disk changes to an overlaid document replace the analyzer's effective contents; mark the old editor overlay superseded so it cannot be silently replayed. The user can discard stale edits in the IDE as they do today. No requirement to forcibly discard or save the IDE buffer.
- Specify how subsequent fresh editor changes reestablish an overlay without applying incremental edits against the wrong base. Maintain the companion's editor snapshot separately from the analyzer's effective snapshot. A fresh user edit may submit a new complete editor snapshot; old buffered events must not undo an authoritative disk update. Exercise this behavior explicitly in integration validation.
- Expose diagnostics with readiness, freshness, and checking state. Distinguish analyzer diagnostics of live contents from Cargo diagnostics of saved contents. An empty cache or timeout is not a successful clean check.
- Make workspace toolchain, Cargo features, target, and analyzer settings explicit; do not assume they match the IDE automatically.
- A small local web UI displays workspaces, Zed connection, current source of document state, analyzer/check status, diagnostics, and effective configuration. Serve built static assets from Rust; Vite is development/build tooling only.

## Initial agent tools

Workspace status; document/workspace symbols; definition; references; hover; diagnostics with optional wait for check; rename; code actions. Keep outputs compact and useful (paths, positions, relevant snippets). Implement LSP workspace edits needed by supported refactorings, including explicit handling of unsupported operations. Apply agent edits to disk and synchronize the analyzer immediately. Do not build a generic shell/command execution tool.

## Implementation sequence

1. Scaffold the Rust core, configuration, local shared MCP endpoint, and workspace/session registry. Write down the small editor-neutral sync contract.
2. Implement standalone rust-analyzer lifecycle, bidirectional LSP handling, document synchronization, file watching, and diagnostics/check state.
3. Expose semantic navigation and refactoring through MCP, with compact output and disk-authoritative edit synchronization.
4. Build the Zed extension and companion, proving notification delivery, initial snapshots, reconnect behavior, Unicode position handling, and agent-over-unsaved-editor precedence in a real Zed session.
5. Add the small static web management UI and configuration visibility.
6. Validate the complete workflow and document installation, configuration, and limitations for immediate use.

## Validation that matters

- Two agent clients share the same core and analyzer for a workspace.
- A standalone disk edit updates definitions and diagnostics without restarting the server.
- A Zed unsaved change reaches our analyzer; open/save/close and reconnect restore coherent state.
- Agent edits win when Zed has a dirty buffer; stale companion events cannot restore the superseded contents.
- Rename across files updates disk and subsequent semantic queries; representative code actions work.
- Initialization and in-progress checks never report a false clean result.
- Distinct worktree directories remain isolated. Unicode and URI/path conversion work.
- Closing Zed leaves standalone usage available. The production UI needs no Node process.

## Explicitly deferred

VS Code companion; TypeScript server; proxying or sharing Zed's analyzer; multiple-companion switching UI; idle shutdown/resource budgets/workspace limits; version-checked transactional refactoring safeguards; plugin frameworks; additional indexing; native desktop UI. Reconsider these only when actual usage warrants them.

## Handoff

Implementation is complete and automated real-analyzer/companion integration checks pass. The Rust release binary and Zed WebAssembly extension build successfully. See README.md and docs/VALIDATION.md for setup and evidence. Native Zed UI automation could not reliably operate keyboard/menu actions; the user is performing the remaining live-editor test. SDS tasks 04 and 06 remain in review until that test succeeds. The dashboard uses embedded static HTML/CSS/JavaScript without requiring Vite. Compiler checks are coordinated in the core, with the dedicated analyzer check-on-save disabled to avoid duplicate checks within the core.

## CLI follow-up

SDS task `rl5QK1`: extract a typed application port, operation schemas and shared action cache from MCP. Both MCP and HTTP call the same application instance. The CLI is a short-lived HTTP client, with flags, JSON/stdin and Cargo workspace inference; it never launches another analyzer. Validate real-analyzer parity, shared action IDs in both directions, input errors and worktree isolation. Install both executable names and restart the existing core to expose the new API.
