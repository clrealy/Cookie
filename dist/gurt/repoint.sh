#!/usr/bin/env bash
# Point the GURT recipe at a new commit, and bump pkgrel with it.
#
# These two have to move together. gurt decides whether a package is
# outdated by comparing pkgver-pkgrel, so changing the pinned commit without
# bumping pkgrel produces a recipe that builds something new while every
# installed copy is told it is already up to date. That happened three times
# before this script existed.
#
# Usage:  dist/gurt/repoint.sh [commit]     (defaults to HEAD)
set -euo pipefail

cd "$(dirname "$0")/../.."
recipe=dist/gurt/GURTBUILD

commit=${1:-$(git rev-parse HEAD)}
# Accept a short hash and expand it; the recipe needs the full one.
commit=$(git rev-parse "$commit")

current=$(grep -oE '#commit=[0-9a-f]{40}' "$recipe" | head -1 | cut -d= -f2)
if [[ $commit == "$current" ]]; then
  echo "already pinned to ${commit:0:8}; nothing to do"
  exit 0
fi

rel=$(grep -oE '^pkgrel=[0-9]+' "$recipe" | cut -d= -f2)
next=$((rel + 1))

# The hash lives in two places: the source pin and the icon URL. A recipe
# with those out of step builds the right code and shows the wrong icon.
sed -i "s|[0-9a-f]\{40\}|$commit|g; s|^pkgrel=$rel\$|pkgrel=$next|" "$recipe"

echo "pinned  ${current:0:8} -> ${commit:0:8}"
echo "pkgrel  $rel -> $next"

# Keep .gurtinfo in step if gurt is reachable; CI regenerates it either way.
for gurt in ../clrealy/gurt/gurt ../GURT/gurt ../gurt/gurt; do
  if [[ -x $gurt ]]; then
    mkdir -p "$(dirname "$gurt")/packages/cookie"
    cp "$recipe" "$(dirname "$gurt")/packages/cookie/GURTBUILD"
    "$gurt" srcinfo "$(dirname "$gurt")/packages/cookie" > dist/gurt/.gurtinfo
    cp dist/gurt/.gurtinfo "$(dirname "$gurt")/packages/cookie/.gurtinfo"
    echo "regenerated .gurtinfo"
    break
  fi
done
