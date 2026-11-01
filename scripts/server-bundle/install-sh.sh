#!/bin/sh
# Writes <dir>/install.sh, the one-command install for a server with only a
# shell: the app's own server script (crates/skiff-client/src/host/server.sh)
# with this release's version, the SHA-256 of each bundle (from
# <dir>/SHA256SUMS) and the public key of their signatures written in.
#
#   install-sh.sh <dir>
set -eu
dir=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
pubkey=$(sed -n 2p "$root/crates/skiff-client/src/host/server-bundle.pub")
[ -s "$dir/SHA256SUMS" ] || { echo "install-sh.sh: no SHA256SUMS in $dir" >&2; exit 1; }
# One "<sha256>  <name>" per line: hex and plain names only, so they sit in quotes.
if grep -qv '^[0-9a-f]\{64\}  skiff-server-[A-Za-z0-9._+-]*\.tar\.gz$' "$dir/SHA256SUMS"; then
  echo "install-sh.sh: SHA256SUMS has a line it does not expect" >&2; exit 1
fi
awk -v v="$version" -v k="$pubkey" -v sums="$dir/SHA256SUMS" '
  $0 == "release='"''"'" { print "release=\x27" v "\x27"; next }
  $0 == "pubkey='"''"'" { print "pubkey=\x27" k "\x27"; next }
  $0 == "sums='"''"'" { printf "sums=\x27"; while ((getline l < sums) > 0) print l; print "\x27"; next }
  { print }
' "$root/crates/skiff-client/src/host/server.sh" > "$dir/install.sh"
grep -q "^release='$version'" "$dir/install.sh" || { echo "install-sh.sh: the release line is missing" >&2; exit 1; }
echo "$dir/install.sh"
