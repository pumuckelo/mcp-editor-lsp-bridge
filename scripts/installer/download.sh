# Sourced by scripts/install.sh; bundled for release distribution.

# INSTALL_VERSION can pin a release; otherwise use the latest published version.
version=${INSTALL_VERSION:-}

if [ -z "$version" ]; then
  latest_url=$(curl --proto '=https' --tlsv1.2 -fsSL \
    -o /dev/null -w '%{url_effective}' \
    "https://github.com/$repository/releases/latest")
  version=${latest_url##*/}
fi

case "$version" in
  v[0-9]*) ;;
  *) fail "No published release found. Set INSTALL_VERSION=vX.Y.Z to select one." ;;
esac

case "$version" in
  *[!a-zA-Z0-9._-]*) fail "Invalid version." ;;
esac

platform="$operating_system-$architecture"
archive="$package_name-$platform.tar.gz"
release_url="https://github.com/$repository/releases/download/$version"
temporary_directory=$(mktemp -d)

# Remove only the temporary download directory when the installer exits.
trap 'rm -rf "$temporary_directory"' 0
trap 'exit 1' HUP INT TERM

# Download the archive and its published SHA-256 checksum over HTTPS.
curl --proto '=https' --tlsv1.2 -fsSL \
  "$release_url/$archive" -o "$temporary_directory/$archive"
curl --proto '=https' --tlsv1.2 -fsSL \
  "$release_url/$archive.sha256" -o "$temporary_directory/checksum"
