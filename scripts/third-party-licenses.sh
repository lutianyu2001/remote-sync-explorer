#!/bin/sh
# Collects the license texts that must ship with a release binary:
#   OUT/rust-licenses.html   Rust dependencies (cargo-about)
#   OUT/go/                  rclone and its Go dependencies (go-licenses)
#
#   scripts/third-party-licenses.sh OUT
#
# Needs cargo-about (cargo install cargo-about) and go-licenses
# (go install github.com/google/go-licenses@latest).
set -eu

out=${1:?usage: $0 OUT}
here=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^const RCLONE_VERSION: &str = "\(v[^"]*\)";/\1/p' "$here/build.rs")
go=${GO:-go}

mkdir -p "$out"
out=$(cd "$out" && pwd)

(cd "$here" && cargo about generate --fail about.hbs -o "$out/rust-licenses.html")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local}
"$go" mod init rext-librclone >/dev/null 2>&1
"$go" get "github.com/rclone/rclone/librclone@$version" >/dev/null 2>&1
rm -rf "$out/go"
go-licenses save github.com/rclone/rclone/librclone --save_path "$out/go"
go-licenses report github.com/rclone/rclone/librclone > "$out/go-licenses.csv"
echo "license texts in $out"
