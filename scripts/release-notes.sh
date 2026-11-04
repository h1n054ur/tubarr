#!/usr/bin/env bash
# Prints the CHANGELOG.md section of one version, without its heading: everything from
# "## [X.Y.Z]" up to the next "## " heading. Used by .github/workflows/release.yml for the GitHub
# release notes.
#
#   scripts/release-notes.sh 0.2.0 [CHANGELOG.md]
#
# A pre-release (0.2.0-rc.1) uses its own section when there is one, else the section of the
# version it leads to (0.2.0), which may still say "not released yet". A final release fails when
# its section is missing, empty or still marked "not released yet", so a tag cannot ship without
# its notes.
set -euo pipefail

version="${1:?usage: release-notes.sh X.Y.Z [CHANGELOG.md]}"
changelog="${2:-CHANGELOG.md}"

# The section's heading line first, then its body. Nothing at all when there is no such section.
section() {
    awk -v want="## [$1]" '
        index($0, want) == 1 { found = 1; print; next }
        found && /^## / { exit }
        found { print }
    ' "$changelog"
}

base="${version%%-*}"
text=$(section "$version")
if [ -z "$text" ] && [ "$base" != "$version" ]; then
    text=$(section "$base")
fi
if [ -z "$text" ]; then
    echo "no \"## [$version]\" section in $changelog" >&2
    exit 1
fi

heading=$(printf '%s\n' "$text" | head -n 1)
if [ "$base" = "$version" ] && [[ "$heading" == *"not released yet"* ]]; then
    echo "$changelog still marks $version as not released yet: put the release date in its heading first" >&2
    exit 1
fi

# The body without the blank lines around it.
notes=$(printf '%s\n' "$text" | tail -n +2 | sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac)
if [ -z "$notes" ]; then
    echo "the \"## [$version]\" section in $changelog is empty" >&2
    exit 1
fi
printf '%s\n' "$notes"
