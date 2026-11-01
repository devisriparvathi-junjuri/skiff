#!/bin/sh
# Builds one server bundle, skiff-server-<version>-<os>-<arch>.tar.gz, in
# <out>: skiffd, skiff-host, skiff, manifest.json (version, protocol,
# target, commit, build id and the SHA-256 of each file), and on Linux the systemd
# user units, and install.sh (the server script, for --uninstall there). It
# does not sign: sign.sh does, once for every bundle.
#
#   build.sh <rust target> <os> <arch> <out>
#
# CARGO: the build command. Default "cargo build"; "cargo zigbuild" for musl.
set -eu
target=$1 os=$2 arch=$3 out=$4
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
protocol=$(sed -n 's/^pub const PROTOCOL: u32 = \([0-9]*\);/\1/p' "$root/crates/skiff-core/src/lib.rs")
commit=$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
# The build id: the commit, and -dirty when the tree has changes. The
# server keeps each build of a version in its own folder.
build=$commit
[ -z "$(git -C "$root" status --porcelain 2>/dev/null)" ] || build=$commit-dirty
[ -n "$version" ] && [ -n "$protocol" ] || { echo "build.sh: no version or protocol found" >&2; exit 1; }

(cd "$root" && ${CARGO:-cargo build} --release --locked --target "$target" -p skiffd -p skiff-host -p skiff-cli)

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1; else shasum -a 256 "$1" | cut -d ' ' -f 1; fi; }

name=skiff-server-$version-$os-$arch
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir=$stage/$name
mkdir -p "$dir"
for b in skiffd skiff-host skiff; do
  cp "$root/target/$target/release/$b" "$dir/$b"
  chmod 755 "$dir/$b"
done
cp "$root/crates/skiff-client/src/host/server.sh" "$dir/install.sh"
chmod 755 "$dir/install.sh"
files="skiffd skiff-host skiff install.sh"
if [ "$os" = linux ]; then
  mkdir -p "$dir/systemd"
  cp "$root/scripts/server-bundle/systemd/skiffd.service" "$root/scripts/server-bundle/systemd/skiff-host.service" "$dir/systemd/"
  files="$files systemd/skiffd.service systemd/skiff-host.service"
fi

{
  printf '{\n  "name": "skiff-server",\n  "version": "%s",\n  "protocol": %s,\n' "$version" "$protocol"
  printf '  "os": "%s",\n  "arch": "%s",\n  "target": "%s",\n  "commit": "%s",\n  "build": "%s",\n  "files": {\n' "$os" "$arch" "$target" "$commit" "$build"
  sep=
  for f in $files; do
    printf '%s    "%s": "%s"' "$sep" "$f" "$(sha "$dir/$f")"
    sep=',
'
  done
  printf '\n  }\n}\n'
} > "$dir/manifest.json"

mkdir -p "$out"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
