#!/bin/sh
# Builds librclone.a and librclone.h into DIR from the Go module in librse/, the same
# way build.rs does, so that RSE_LIBRCLONE_DIR=DIR can skip the Go build (CI caching,
# packaging, offline builds).
#
#   scripts/build-librclone.sh DIR [VERSION]
#
# VERSION defaults to RCLONE_VERSION in build.rs, which librse/go.mod pins; another one
# is fetched into a copy of librse/go.mod, as RSE_RCLONE_VERSION does for build.rs.
set -eu

dir=${1:?usage: $0 DIR [VERSION]}
here=$(cd "$(dirname "$0")/.." && pwd)
pinned=$(sed -n 's/^const RCLONE_VERSION: &str = "\(v[^"]*\)";/\1/p' "$here/build.rs")
[ -n "$pinned" ] || { echo "cannot find RCLONE_VERSION in build.rs" >&2; exit 1; }
version=${2:-$pinned}
go=${GO:-go}

mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local} GOWORK=off
cd "$here/librse"
if [ "$version" = "$pinned" ]; then
    set -- -mod=readonly
else
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    cp go.mod go.sum "$work/"
    "$go" get "-modfile=$work/go.mod" "github.com/rclone/rclone@$version"
    set -- "-modfile=$work/go.mod" -mod=mod
fi
"$go" build "$@" -buildmode=c-archive -trimpath \
    "-ldflags=-X github.com/rclone/rclone/fs.VersionSuffix=" \
    -o "$dir/librclone.a" .
echo "$version" > "$dir/VERSION"
echo "built librclone $version in $dir"
