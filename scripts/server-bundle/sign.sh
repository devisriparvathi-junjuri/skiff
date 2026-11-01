#!/bin/sh
# Writes SHA256SUMS for every server bundle in <dir>, then signs each
# bundle and SHA256SUMS with minisign. The trusted comment of each
# signature names its file ("file:<name>"); the app checks it. It also
# writes skiff-server.pub, the public key, from the secret key.
#
#   sign.sh <dir>
#
# SKIFF_SERVER_SIGNING_KEY: the contents of a minisign secret key made with
# no password (minisign -G -W). Absent: SHA256SUMS only, and a warning.
# MINISIGN: the minisign program. Default: minisign on PATH.
set -eu
dir=$1
m=${MINISIGN:-minisign}
cd "$dir"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi; }
: > SHA256SUMS
for f in skiff-server-*.tar.gz; do sha "$f" >> SHA256SUMS; done
cat SHA256SUMS
if [ -z "${SKIFF_SERVER_SIGNING_KEY:-}" ]; then
  echo "::warning::SKIFF_SERVER_SIGNING_KEY is not set. The server bundles are not signed, and the app does not use them."
  exit 0
fi
umask 077
key=$(mktemp)
trap 'rm -f "$key"' EXIT
printf '%s\n' "$SKIFF_SERVER_SIGNING_KEY" > "$key"
"$m" -R -s "$key" -p skiff-server.pub >/dev/null
for f in skiff-server-*.tar.gz SHA256SUMS; do
  "$m" -S -s "$key" -m "$f" -x "$f.minisig" -t "file:$f" </dev/null >/dev/null
  "$m" -V -q -p skiff-server.pub -m "$f" -x "$f.minisig"
done
echo "signed: $(ls ./*.minisig | wc -l | tr -d ' ') files"
