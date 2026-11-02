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
# librse/ itself is rse's own code, under rse's license.
own=github.com/lutianyu2001/remote-sync-explorer

mkdir -p "$out"
out=$(cd "$out" && pwd)

(cd "$here" && cargo about generate --fail about.hbs -o "$out/rust-licenses.html")

cd "$here/librse"
export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local} GOWORK=off GOFLAGS=-mod=readonly
rm -rf "$out/go"
go-licenses save . --ignore "$own" --save_path "$out/go"
go-licenses report . --ignore "$own" > "$out/go-licenses.csv"
echo "license texts in $out"
