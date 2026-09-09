#!/bin/sh
# Print a standalone installer to stdout without executing any installation steps.
set -eu

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

printf '#!/bin/sh\n# Generated from scripts/installer/*.sh. Edit those source files instead.\nset -eu\n'

for module in config platform download verify install; do
  printf '\n# --- %s ---\n' "$module"
  cat "$script_directory/installer/$module.sh"
done
