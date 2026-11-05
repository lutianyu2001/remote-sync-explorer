#!/bin/sh
# Collects the license texts of the dependencies a release artifact is built from, which
# must ship with it:
#   OUT/rust-licenses.html   Rust dependencies (cargo-about, with about.toml and about.hbs
#                            at the repository's root)
#   OUT/go/                  Go dependencies of each module in GO_MODULES (go-licenses),
#   OUT/go-licenses.csv      with the list of them
#
#   scripts/third-party-licenses.sh OUT
#
# Copied into a project as scripts/third-party-licenses.sh; only the lines marked
# "Per project" change. Needs cargo-about (cargo install --locked --features cli
# cargo-about@0.9.3) and, with GO_MODULES, go-licenses (go install
# github.com/google/go-licenses@v1.6.0), at the versions the release job installs.
set -eu

# Per project: the directories of the Go modules compiled into the release, relative to the
# repository's root, separated by spaces; empty without Go.
GO_MODULES="librse"
# Per project: the module path of the project's own Go code, which is under the project's
# license and left out; empty without Go.
OWN_GO="github.com/lutianyu2001/remote-sync-explorer"

out=${1:?usage: $0 OUT}
here=$(cd "$(dirname "$0")/.." && pwd)

mkdir -p "$out"
out=$(cd "$out" && pwd)

if [ -f "$here/Cargo.toml" ]; then
    # --fail: a dependency under a license not accepted in about.toml stops the release.
    (cd "$here" && cargo about generate --fail about.hbs -o "$out/rust-licenses.html")
fi

if [ -n "$GO_MODULES" ]; then
    export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local} GOWORK=off GOFLAGS=-mod=readonly
    rm -rf "$out/go"
    : > "$out/go-licenses.csv"
    for module in $GO_MODULES; do
        set -- --save_path "$out/go/$module"
        [ -z "$OWN_GO" ] || set -- "$@" --ignore "$OWN_GO"
        (cd "$here/$module" && go-licenses save . "$@")
        set --
        [ -z "$OWN_GO" ] || set -- --ignore "$OWN_GO"
        (cd "$here/$module" && go-licenses report . "$@") >> "$out/go-licenses.csv"
    done
fi
echo "license texts in $out"
