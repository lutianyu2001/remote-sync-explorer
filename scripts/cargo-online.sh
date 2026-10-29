#!/usr/bin/env bash
# Runs one cargo command with crates.io reachable, for the commands the offline setup in
# .cargo/config.toml blocks. A command that changes dependencies refreshes vendor/.
#
# Usage: scripts/cargo-online.sh outdated --root-deps-only
#        scripts/cargo-online.sh update -p some-crate
#        scripts/cargo-online.sh upgrade --incompatible -p some-crate
#        scripts/cargo-online.sh add serde
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

cfg=.cargo/config.toml
bak=.cargo/config.toml.offline-bak

mv "$cfg" "$bak"
trap 'mv "$bak" "$cfg"' EXIT

cargo "$@"

case "${1:-}" in
  update|upgrade|add|remove|rm)
    # fuzz/Cargo.lock seeded again from Cargo.lock, and fuzz/'s dependencies vendored too,
    # or cargo vendor would delete them (see SUPPLY_CHAIN.md).
    cp Cargo.lock fuzz/
    cargo vendor --versioned-dirs --sync fuzz/Cargo.toml vendor > /dev/null
    echo "vendor/ and fuzz/Cargo.lock refreshed. Review git diff Cargo.toml Cargo.lock, then cargo vet diff for the code."
    ;;
esac
