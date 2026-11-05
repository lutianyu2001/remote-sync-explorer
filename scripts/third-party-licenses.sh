#!/bin/sh
# Collects the license texts of the dependencies a release artifact is built from, which
# must ship with it:
#   OUT/rust-licenses.html   Rust dependencies (cargo-about, with about.toml and about.hbs
#                            at the repository's root)
#   OUT/go/<dir>/            Go dependencies of each package in GO_MODULES (go-licenses;
#                            <dir> is its directory with / as _, root for "."),
#   OUT/go-licenses.csv      with the list of them; first each is checked: a license not in
#                            GO_ALLOWED fails the run, but in GO_ALLOWED_MODULES
#   OUT/GPL-3.0.txt          the GPL-3.0's text, when an LGPL-3.0 library is among them
#   OUT/licenses.txt         all of the above as plain text, one block a component (written
#                            by scripts/notices.py), which a release build embeds in the
#                            program to print (distribution.md)
#
#   scripts/third-party-licenses.sh OUT
#
# Copied into a project as scripts/third-party-licenses.sh; only the lines marked
# "Per project" change. Needs cargo-about (cargo install --locked --features cli
# cargo-about@0.9.3) and, with GO_MODULES, go-licenses (go install
# github.com/google/go-licenses@v1.6.0), at the versions the release job installs, python3
# and curl.
set -eu

# Per project: the Cargo.toml of the crate the artifact is built from, relative to the
# repository's root (for a PyO3 binding, crates/<binding>/Cargo.toml); empty for the root's,
# with the whole workspace when it is a virtual one.
RUST_MANIFEST=""
# Per project: the feature flags the artifact is built with, as cargo takes them, features
# separated by commas (--features python,cli; --no-default-features; --all-features: from
# maturin's [tool.maturin] or the release job); empty for the defaults. Otherwise the
# licenses of optional dependencies the artifact holds are left out, or others put in.
RUST_FEATURE_FLAGS=""
# Per project: the directory of each Go main package compiled into the release (go-licenses
# reads that package and what it imports, not the rest of its module; for cmd/<x>, that
# directory), relative to the repository's root, separated by spaces; empty without Go.
GO_MODULES="librse"
# Per project: the module path of the project's own Go code, which is under the project's
# license and left out; empty without Go.
OWN_GO="github.com/lutianyu2001/remote-sync-explorer"
# Per project: the licenses accepted for any Go dependency, separated by commas: those of
# compat.py --accepted A <the owner's license> that the dependencies use (the same list as
# about.toml's accepted).
GO_ALLOWED="Apache-2.0,BSD-2-Clause,BSD-3-Clause,CC0-1.0,ISC,MIT"
# Per project: the module paths let through one by one, after the user decided so for each
# (an MPL or LGPL module, one under a license table A marks ⚠), separated by spaces: like
# [<crate>] accepted in about.toml. Their dependencies are still checked.
GO_ALLOWED_MODULES="github.com/cloudsoda/sddl github.com/hashicorp/errwrap github.com/hashicorp/go-cleanhttp github.com/hashicorp/go-multierror github.com/hashicorp/go-retryablehttp github.com/hashicorp/go-uuid"

out=${1:?usage: $0 OUT}
rule='================================================================================'
thin='--------------------------------------------------------------------------------'
here=$(cd "$(dirname "$0")/.." && pwd)

mkdir -p "$out"
out=$(cd "$out" && pwd)

if [ -f "$here/${RUST_MANIFEST:-Cargo.toml}" ]; then
    # A license not accepted in about.toml fails the run as it is; --fail also fails it on a
    # crate whose license cannot be read or worked out.
    # --locked: the versions of Cargo.lock, which the artifact is built from. -c: about.toml
    # at the root, also where RUST_MANIFEST is a member's.
    set -- --fail --locked -c about.toml
    [ -z "$RUST_MANIFEST" ] || set -- "$@" -m "$RUST_MANIFEST"
    # Split into words on purpose: one flag a word.
    # shellcheck disable=SC2086
    set -- "$@" $RUST_FEATURE_FLAGS
    (cd "$here" && cargo about generate "$@" about.hbs -o "$out/rust-licenses.html")
    (cd "$here" && cargo about generate "$@" --format json -o "$out/.rust.json")
    set --
fi

if [ -n "$GO_MODULES" ]; then
    [ -n "$GO_ALLOWED" ] || { echo "GO_ALLOWED is empty: the licenses to accept" >&2; exit 1; }
    export CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-local} GOWORK=off GOFLAGS=-mod=readonly
    rm -rf "$out/go"
    : > "$out/go-licenses.csv"
    : > "$out/.go-index"
    for module in $GO_MODULES; do
        # go-licenses save refuses a save path that exists, so no entry may lie inside
        # another's: cmd/a becomes cmd_a, and "." root.
        dir=$(printf '%s' "$module" | tr / _)
        [ "$dir" != . ] || dir=root
        set --
        [ -z "$OWN_GO" ] || set -- --ignore "$OWN_GO"
        for allowed in $GO_ALLOWED_MODULES; do set -- "$@" --ignore "$allowed"; done
        (cd "$here/$module" && go-licenses check . --allowed_licenses="$GO_ALLOWED" "$@")
        set --
        [ -z "$OWN_GO" ] || set -- --ignore "$OWN_GO"
        (cd "$here/$module" && go-licenses save . --save_path "$out/go/$dir" "$@")
        (cd "$here/$module" && go-licenses report . "$@") > "$out/.report"
        cat "$out/.report" >> "$out/go-licenses.csv"
        # For notices.py: each library once, with where go-licenses saved it.
        while IFS=, read -r lib url license; do
            cut -f1 "$out/.go-index" | grep -qxF "$lib" ||
                printf '%s\t%s\t%s\t%s\n' "$lib" "$url" "$license" "$out/go/$dir/$lib" >> "$out/.go-index"
        done < "$out/.report"
    done
    rm -f "$out/.report"
fi

rust_json=-
go_index=-
[ ! -f "$out/.rust.json" ] || rust_json="$out/.rust.json"
[ ! -f "$out/.go-index" ] || go_index="$out/.go-index"
python3 "$here/scripts/notices.py" "$out/licenses.txt" "$rust_json" "$go_index"
rm -f "$out/.rust.json" "$out/.go-index"

# LGPL-3.0 is a set of permissions added to the GPL-3.0, whose text it requires beside it
# (section 4(b)); the libraries ship only the LGPL's.
if grep -A1 'GNU LESSER GENERAL PUBLIC LICENSE' "$out/licenses.txt" | grep -q 'Version 3,'; then
    curl -fsSL https://www.gnu.org/licenses/gpl-3.0.txt -o "$out/GPL-3.0.txt"
    head -5 "$out/GPL-3.0.txt" | grep -q 'GNU GENERAL PUBLIC LICENSE' &&
        head -5 "$out/GPL-3.0.txt" | grep -q 'Version 3, 29 June 2007' ||
        { echo "GPL-3.0.txt: not the GPL-3.0's text" >&2; exit 1; }
    printf '%s\nGNU General Public License, version 3: the LGPL-3.0 above builds on it\n%s\n\n' \
        "$rule" "$thin" >> "$out/licenses.txt"
    cat "$out/GPL-3.0.txt" >> "$out/licenses.txt"
fi
echo "license texts in $out"
