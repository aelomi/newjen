#!/bin/bash
set -euo pipefail

check=0
if [[ ${1:-} == --check ]]; then
    check=1
    shift
fi
if [[ $# -ne 1 ]]; then
    echo 'Usage from the repo root: scripts/release-notes.sh [--check] <version>' >&2
    echo '  --check only validates, so it can run before the tag exists.' >&2
    exit 1
fi
cd "$(dirname "$0")/.."
version=$1
if [[ ! -f CHANGELOG.md ]]; then
    echo "CHANGELOG.md is missing." >&2
    exit 1
fi
notes=$(LC_ALL=C awk -v prefix="## $version · " '
    index($0, prefix) == 1 { found = 1; next }
    found && /^## / { exit }
    found { print }
' CHANGELOG.md | sed '/./,$!d')
if [[ -z ${notes//[[:space:]]/} ]]; then
    echo "CHANGELOG.md has no \"## $version · <date>\" section with notes. Rename Unreleased to it before tagging." >&2
    exit 1
fi
unreleased=$(LC_ALL=C awk '
    /^## Unreleased/ { found = 1; next }
    found && /^## / { exit }
    found && /^- / { count++ }
    END { print count + 0 }
' CHANGELOG.md)
if [[ $unreleased -ne 0 ]]; then
    echo "CHANGELOG.md still has $unreleased bullet(s) under \"## Unreleased\". They would miss the release: move them into the $version section first." >&2
    exit 1
fi
if [[ $check -eq 1 ]]; then
    echo "CHANGELOG.md is ready for $version."
    exit 0
fi
printf '%s\n' "$notes"
