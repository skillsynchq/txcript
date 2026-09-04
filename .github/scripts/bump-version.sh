#!/usr/bin/env sh
# Set the release version everywhere it lives, then refresh the lockfile.
# Usage: bump-version.sh 0.13.0
# Four locations: the library manifest, the CLI manifest, the CLI's pin on
# the library, and package.json. All must agree or the release guards fail.
set -eu
ver="$1"
case "$ver" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "::error::not a version: $ver" >&2; exit 1 ;;
esac
sed -i.bak -E "s/^version = \"[^\"]+\"/version = \"$ver\"/" Cargo.toml cli/Cargo.toml
sed -i.bak -E "s/^(txcript = \{ version = )\"[^\"]+\"/\1\"$ver\"/" cli/Cargo.toml
sed -i.bak -E "s/^(  \"version\": )\"[^\"]+\"/\1\"$ver\"/" package.json
rm -f Cargo.toml.bak cli/Cargo.toml.bak package.json.bak
cargo update --workspace --quiet
for f in Cargo.toml cli/Cargo.toml package.json; do
  grep -q "\"$ver\"" "$f" || { echo "::error::$f did not take version $ver" >&2; exit 1; }
done
[ "$(grep -c "\"$ver\"" cli/Cargo.toml)" = 2 ] || { echo "::error::cli/Cargo.toml needs both its version and the txcript pin at $ver" >&2; exit 1; }
echo "version $ver set in Cargo.toml, cli/Cargo.toml, package.json, Cargo.lock"
