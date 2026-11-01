#!/bin/sh
# Prints the base64 line of the public key of SKIFF_SERVER_SIGNING_KEY, so
# the release job can check it against the key the app carries. Prints
# nothing without the key.
# MINISIGN: the minisign program. Default: minisign on PATH.
set -eu
[ -n "${SKIFF_SERVER_SIGNING_KEY:-}" ] || exit 0
umask 077
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
printf '%s\n' "$SKIFF_SERVER_SIGNING_KEY" > "$dir/key"
"${MINISIGN:-minisign}" -R -s "$dir/key" -p "$dir/key.pub" >/dev/null
sed -n 2p "$dir/key.pub"
