#!/bin/sh
# Repository entry point. Releases contain a standalone bundle of these files.
set -eu

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

. "$script_directory/installer/config.sh"
. "$script_directory/installer/platform.sh"
. "$script_directory/installer/download.sh"
. "$script_directory/installer/verify.sh"
. "$script_directory/installer/install.sh"
