#!/bin/sh
# Builds librclone.a and librclone.h into DIR, the same way build.rs does, so that
# RSE_LIBRCLONE_DIR=DIR can skip the Go build (CI caching, packaging, offline builds).
#
#   scripts/build-librclone.sh DIR [VERSION]
#
# VERSION defaults to RCLONE_VERSION in build.rs.
set -eu

dir=${1:?usage: $0 DIR [VERSION]}
here=$(cd "$(dirname "$0")/.." && pwd)
version=${2:-$(sed -n 's/^const RCLONE_VERSION: &str = "\(v[^"]*\)";/\1/p' "$here/build.rs")}
[ -n "$version" ] || { echo "cannot find RCLONE_VERSION in build.rs" >&2; exit 1; }
go=${GO:-go}

mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local}
"$go" mod init rse-librclone
"$go" get "github.com/rclone/rclone/librclone@$version"
"$go" build -buildmode=c-archive -trimpath \
    "-ldflags=-X github.com/rclone/rclone/fs.VersionSuffix=" \
    -o "$dir/librclone.a" github.com/rclone/rclone/librclone
echo "$version" > "$dir/VERSION"
echo "built librclone $version in $dir"
