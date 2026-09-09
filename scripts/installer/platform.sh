# Sourced by scripts/install.sh; bundled for release distribution.

# Select the archive for this operating system and CPU.
case "$(uname -s)" in
  Darwin) operating_system="darwin" ;;
  Linux) operating_system="linux" ;;
  *) fail "Supported systems: macOS and Linux." ;;
esac

case "$(uname -m)" in
  arm64|aarch64) architecture="arm64" ;;
  x86_64|amd64) architecture="x64" ;;
  *) fail "Unsupported CPU architecture." ;;
esac

for required_command in curl tar mktemp; do
  command -v "$required_command" >/dev/null || fail "Missing $required_command."
done
