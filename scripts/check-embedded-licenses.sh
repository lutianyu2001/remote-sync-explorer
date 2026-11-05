#!/bin/sh
# Checks that a release build prints the license texts it was built with: the end of
# `PROGRAM --licenses` must be LICENSES (scripts/third-party-licenses.sh's licenses.txt)
# byte for byte. The release job runs it after the build and before packaging, so that a
# build without THIRD_PARTY_LICENSES, or with other texts, is never released.
#
#   scripts/check-embedded-licenses.sh PROGRAM LICENSES
#
# Copied into a project as scripts/check-embedded-licenses.sh; nothing in it changes.
set -eu

program=${1:?usage: $0 PROGRAM LICENSES}
licenses=${2:?usage: $0 PROGRAM LICENSES}
[ -s "$licenses" ] || { echo "$licenses: missing or empty" >&2; exit 1; }

printed=$(mktemp)
trap 'rm -f "$printed"' EXIT
"$program" --licenses > "$printed"
size=$(wc -c < "$licenses")
if ! tail -c "$size" "$printed" | cmp -s - "$licenses"; then
    echo "$program --licenses does not end with $licenses: built without it, or with other texts" >&2
    exit 1
fi
echo "$program prints the license texts of $licenses"
