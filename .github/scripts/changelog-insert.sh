#!/usr/bin/env sh
# Insert a generated release section into CHANGELOG.md, directly above the
# newest existing section. Usage: changelog-insert.sh section.md
set -eu
section="$1"
awk -v f="$section" '
  !done && /^## \[/ { while ((getline line < f) > 0) print line; print ""; done = 1 }
  { print }
  END { if (!done) { while ((getline line < f) > 0) print line } }
' CHANGELOG.md > CHANGELOG.md.new
mv CHANGELOG.md.new CHANGELOG.md
