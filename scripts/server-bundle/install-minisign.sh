#!/bin/sh
# Puts minisign 0.12 (Linux x86_64) at <dir>/minisign, from its GitHub
# release, after a SHA-256 check of the download.
#
#   install-minisign.sh <dir>
set -eu
dir=$1
url=https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz
want=9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/m.tgz" "$url"
got=$(sha256sum "$tmp/m.tgz" | cut -d ' ' -f 1)
[ "$got" = "$want" ] || { echo "minisign download: SHA-256 $got, want $want" >&2; exit 1; }
tar -C "$tmp" -xzf "$tmp/m.tgz" 2>/dev/null
mkdir -p "$dir"
cp "$tmp/minisign-linux/x86_64/minisign" "$dir/minisign"
chmod 755 "$dir/minisign"
