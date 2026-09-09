# Sourced by scripts/install.sh; bundled for release distribution.

# Override these directories if you prefer a different installation location.
install_root=${INSTALL_ROOT:-"$HOME/.local/share/$package_name"}
bin_directory=${INSTALL_BIN_DIR:-"$HOME/.local/bin"}

mkdir -p "$install_root/releases" "$bin_directory"
install_root=$(cd "$install_root" && pwd)
bin_directory=$(cd "$bin_directory" && pwd)

# Preserve unrelated regular files. Existing launcher symlinks can be updated.
for binary in $binaries; do
  if [ -e "$bin_directory/$binary" ] && [ ! -L "$bin_directory/$binary" ]; then
    fail "$bin_directory/$binary already exists. Choose INSTALL_BIN_DIR."
  fi

  if [ ! -x "$temporary_directory/$package_name-$platform/$binary" ]; then
    fail "Missing executable: $binary."
  fi
done

if [ -e "$install_root/current" ] && [ ! -L "$install_root/current" ]; then
  fail "$install_root/current is not a symlink."
fi

# Keep each installation in its own directory, then update the launchers.
# Older installations remain available until you remove them manually.
destination=$(mktemp -d "$install_root/releases/$version-$platform.XXXXXX")
cp -R "$temporary_directory/$package_name-$platform/." "$destination/"
ln -sfn "$destination" "$install_root/current"

for binary in $binaries; do
  ln -sfn "$install_root/current/$binary" "$bin_directory/$binary"
done

printf 'Installed %s %s to %s\n' "$package_name" "$version" "$bin_directory"
printf 'Skills: %s/current/skills\n' "$install_root"

case ":$PATH:" in
  *":$bin_directory:"*) ;;
  *) printf 'Add this directory to PATH: %s\n' "$bin_directory" ;;
esac

printf 'No project instructions or shell profiles were modified.\n'
