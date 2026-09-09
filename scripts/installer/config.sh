# Sourced by scripts/install.sh; bundled for release distribution.

repository="pumuckelo/mcp-editor-lsp-bridge"
package_name="editor-bridge"
binaries="bridge mcp-editor-lsp-bridge"

fail() {
  echo "Error: $*" >&2
  exit 1
}
