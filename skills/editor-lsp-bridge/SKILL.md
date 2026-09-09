---
name: editor-lsp-bridge
description: Use the bridge CLI for shared Rust analyzer navigation, refactoring, and diagnostics when working on Rust code.
---

- Use `bridge` through the shell for Rust symbols, definitions, references, hover, rename, code actions and diagnostics. Commands connect to the shared running core and analyzer; do not launch a server per agent.
- The CLI infers the Cargo workspace from cwd; override with `--workspace PATH`. Inspect readiness with `bridge workspace-status`; `--all` lists sessions from any directory.
- Discover only what you need: `bridge tools`, `bridge OPERATION --help`, or `bridge schema OPERATION`. Commands use hyphens, such as `document-symbols`. Complex inputs accept `--json` or piped `--stdin`.
- Positions are zero-based lines and UTF-16 characters. CLI `--path` is cwd-relative; JSON paths are workspace-relative, absolute, or file URIs.
- After edits, use `bridge diagnostics`; `bridge diagnostics --check` awaits the shared saved-file Cargo check. Inspect readiness, freshness and success; empty live diagnostics alone do not establish a clean build. Still run tests appropriate to the change.
- Rename previews by default; `--apply` writes edits. Apply listed code actions with `bridge apply-code-action --action-id ID`; IDs remain available until core restart. Agent disk edits take precedence over stale unsaved editor buffers. Refactoring has no stale-version or transactional protection yet; apply against current code and inspect failures before retrying.
- If the core is unavailable, report that and use ordinary search/check tools as needed. CLI errors are JSON on stderr; exit 2 means invalid input, exit 1 means an operation or connection failure.
