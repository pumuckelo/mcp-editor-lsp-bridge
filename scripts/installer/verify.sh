# Sourced by scripts/install.sh; bundled for release distribution.

expected_checksum=$(awk 'NR == 1 { print $1 }' "$temporary_directory/checksum")

case "$expected_checksum" in
  ''|*[!a-fA-F0-9]*) fail "Invalid checksum." ;;
esac

[ ${#expected_checksum} -eq 64 ] || fail "Invalid checksum length."

if command -v sha256sum >/dev/null; then
  actual_checksum=$(sha256sum "$temporary_directory/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null; then
  actual_checksum=$(shasum -a 256 "$temporary_directory/$archive" | awk '{ print $1 }')
else
  fail "Install sha256sum or shasum to verify the download."
fi

if [ "$expected_checksum" != "$actual_checksum" ]; then
  fail "Checksum mismatch; nothing installed."
fi

# Check archive paths before extracting into the temporary directory.
tar -tzf "$temporary_directory/$archive" > "$temporary_directory/entries"

while IFS= read -r entry; do
  case "$entry" in
    "$package_name-$platform"|"$package_name-$platform/"*) ;;
    *) fail "Unexpected archive entry." ;;
  esac

  case "/$entry/" in
    */../*) fail "Unsafe archive path." ;;
  esac
done < "$temporary_directory/entries"

tar -xzf "$temporary_directory/$archive" -C "$temporary_directory"
