#!/usr/bin/env bash
# Bump the version, tag it, and push. The tag starts the Release workflow,
# which builds the Linux packages and attaches them to a draft release.
#
#   scripts/release.sh [major|minor|patch]   (default: minor)
#
# The version lives in the root Cargo.toml ([workspace.package]). Every crate
# and the Tauri app read it from there. package.json follows it.
set -euo pipefail

part="${1:-minor}"
cd "$(git rev-parse --show-toplevel)"

[[ "$part" =~ ^(major|minor|patch)$ ]] || { echo "usage: $0 [major|minor|patch]" >&2; exit 2; }
[[ "$(git branch --show-current)" == main ]] || { echo "Release from main." >&2; exit 1; }
[[ -z "$(git status --porcelain)" ]] || { echo "Commit or stash your changes first." >&2; exit 1; }
git fetch -q origin main
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || { echo "main is not level with origin/main." >&2; exit 1; }

old="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
IFS=. read -r major minor patch <<<"$old"
case "$part" in
  major) new="$((major + 1)).0.0" ;;
  minor) new="$major.$((minor + 1)).0" ;;
  patch) new="$major.$minor.$((patch + 1))" ;;
esac
git rev-parse -q --verify "refs/tags/v$new" >/dev/null && { echo "Tag v$new exists." >&2; exit 1; }

# The host-boundary check needs Python 3.12. It must pass before the bump, so
# the refresh below admits only the version change, not an unreviewed one.
if command -v python3.12 >/dev/null; then boundary=(python3.12 scripts/host-boundary/check.py)
else boundary=(uv run --no-project --python 3.12 scripts/host-boundary/check.py); fi
"${boundary[@]}" >/dev/null || { "${boundary[@]}" >&2; echo "Fix the host-boundary check first." >&2; exit 1; }

read -r -p "Release v$old -> v$new? [y/N] " ok
[[ "$ok" == [yY] ]] || exit 1

sed -i "0,/^version = \"$old\"$/s//version = \"$new\"/" Cargo.toml
sed -i "0,/\"version\": \"$old\"/s//\"version\": \"$new\"/" apps/desktop/package.json
# skiffd has its own version. The app moves a running daemon onto the one it
# ships only when that version is newer, so a release that changes the daemon
# must raise it, or users keep the old daemon until they log out.
last="$(git describe --tags --abbrev=0)"
dold="$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/skiffd/Cargo.toml | head -n1)"
dlast="$(git show "$last:crates/skiffd/Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n1)"
if ! git diff --quiet "$last" HEAD -- crates/skiffd crates/skiff-core && [[ "$dold" == "$dlast" ]]; then
  IFS=. read -r dmaj dmin dpat <<<"$dold"
  dnew="$dmaj.$dmin.$((dpat + 1))"
  sed -i "0,/^version = \"$dold\"$/s//version = \"$dnew\"/" crates/skiffd/Cargo.toml crates/skiff-core/Cargo.toml
  echo "skiffd $dold -> $dnew"
fi
# Software stores list the releases from the AppStream metadata.
metainfo=apps/desktop/src-tauri/linux/com.solutionscay.skiff.metainfo.xml
sed -i "s|^\(\s*\)<releases>$|&\n\1  <release version=\"$new\" date=\"$(date +%F)\"/>|" "$metainfo"
# Refresh the workspace entries in Cargo.lock. Dependencies stay as they are.
cargo metadata --format-version 1 >/dev/null
# The boundary check fingerprints the bumped files it lists in its baseline
# (today only package.json). Re-record those entries alone, never --record.
bumped=(Cargo.toml Cargo.lock apps/desktop/package.json crates/skiffd/Cargo.toml crates/skiff-core/Cargo.toml "$metainfo")
refresh=()
for f in "${bumped[@]}"; do
  if grep -q "^  \"$f\": {" scripts/host-boundary/baseline.json; then refresh+=(--refresh "$f"); fi
done
"${boundary[@]}" "${refresh[@]}"

git add "${bumped[@]}" scripts/host-boundary/baseline.json
git commit -q -m "Release v$new"
git tag -a "v$new" -m "v$new"
git push -q origin main "v$new"
echo "Pushed v$new. The Release workflow builds the packages into a draft release."
