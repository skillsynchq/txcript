#!/usr/bin/env sh
# Print the CHANGELOG.md section for one version, followed by links to the
# published packages. Usage: release-notes.sh 0.12.1 > notes.md
# Exits 1 when the version has no section, so a tag without changelog
# coverage fails the release job instead of shipping empty notes.
set -eu
ver="$1"
section="$(awk -v ver="$ver" '
  /^## \[/ { inside = ($0 ~ "^## \\[" ver "\\]") ; next }
  inside && /^\[/ { next }
  inside { print }
' CHANGELOG.md)"
# Trim leading/trailing blank lines.
section="$(printf '%s\n' "$section" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')"
if [ -z "$section" ]; then
  echo "::error::CHANGELOG.md has no section for $ver" >&2
  exit 1
fi
printf '%s\n\n' "$section"
cat <<NOTES
---

- crates.io: https://crates.io/crates/txcript/$ver
- npm: https://www.npmjs.com/package/txcript/v/$ver
- docs.rs: https://docs.rs/txcript/$ver
NOTES
