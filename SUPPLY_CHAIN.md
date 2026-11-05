# Supply Chain

## Dependencies

- Builds are offline. `.cargo/config.toml` replaces crates.io with `vendor/` and sets
  `net.offline = true`.
- `vendor/` is not in git. It is downloaded per `Cargo.lock` and `fuzz/Cargo.lock` (see
  below), whose sha256 checksums make it byte for byte the code that was reviewed:

  ```bash
  cargo --config net.offline=false vendor --locked --versioned-dirs --sync fuzz/Cargo.toml vendor
  ```

- `vendor/` is not filtered by platform, so it also holds crates only Windows or
  WebAssembly would build (`windows-sys`, `winapi`, `wasm-bindgen`, …).
- `rust-toolchain.toml` pins the Rust toolchain.
- `fuzz/` is a workspace of its own, with its own `fuzz/Cargo.lock`, so the fuzzing
  tools (`libfuzzer-sys`, `arbitrary` and what they pull in) are not among rse's
  dependencies: not in `Cargo.lock`, not built for users, and not covered by
  `cargo deny` and `cargo vet` below; `cargo xtask deny` checks them for known
  vulnerabilities and foreign sources only (`cargo deny --locked --manifest-path
  fuzz/Cargo.toml check advisories sources`). Its lock file is seeded from
  `Cargo.lock`, so that the crates both share are the same versions; after updating
  `Cargo.lock`, seed it again, which also brings its new dependencies into `vendor/`:

  ```bash
  cp Cargo.lock fuzz/ && cargo --config net.offline=false vendor --versioned-dirs --sync fuzz/Cargo.toml vendor
  ```
- The Rust side ends there. The default build also compiles rclone with Go, from the
  module in `librse/`: librclone's four C functions, as rclone builds them, and rse's
  own rc calls (see SECURITY.md). Its `go.mod` pins rclone (build.rs checks that it is
  the version build.rs names) and its committed `go.sum` holds the checksums of every
  module, so Go downloads exactly those from the Go module proxy on the first build,
  and builds with `-mod=readonly`, never changing either file. `RSE_RCLONE_VERSION`
  builds another rclone version from a copy of `go.mod` in the build directory, whose
  new checksums Go takes from its checksum database. To build with no network at all,
  build librclone once with `scripts/build-librclone.sh <dir>` and point
  `RSE_LIBRCLONE_DIR` at it, or build the `rcd` feature, which needs no Go.

## Licenses in a Release

- `rse --licenses` prints the license texts of every Rust crate and Go module built into
  it, one block a component (name and version, license, source, then the texts), with
  the NOTICE files of the Go modules that have one, and the GPL-3.0's text, which the
  LGPL-3.0 below builds on: the release build embeds them, collected at the release by
  `scripts/third-party-licenses.sh` from the same lock files as the build, and checks the
  program prints them (`scripts/check-embedded-licenses.sh`). Other builds print a note.
- A release archive also holds them in `third-party/` (`licenses.txt`,
  `rust-licenses.html`, `GPL-3.0.txt`, `go/librse/` and the list `go-licenses.csv`).
- Go links everything statically, and a few of the modules rclone brings in ask more of a
  binary than their license texts:
  - `github.com/cloudsoda/sddl`, under LGPL-3.0, comes in through rclone's SMB backend.
    The LGPL lets users run rse with a modified version of it: its source is in
    `third-party/go/librse/github.com/cloudsoda/sddl/`, and rse's own source is public,
    so rse can be rebuilt with a modified copy, for example through a `replace`
    directive in `librse/go.mod`.
  - `github.com/hashicorp/errwrap`, `go-cleanhttp`, `go-multierror`, `go-retryablehttp`
    and `go-uuid`, under MPL-2.0: their source is in `third-party/go/librse/` as well.

## Audit

`cargo xtask deny` and `cargo xtask vet` run the two below with the versions pinned in
`xtask/src/pins.rs` (`cargo xtask setup` installs them). `xtask/` is a member of this
workspace that uses only crates rse already depends on (ureq and serde_json, to look
up newer releases), so it adds no package to `Cargo.lock`; `cargo build`
and `cargo test` never build it.

- `cargo deny --locked --workspace check` (`deny.toml`), every member included: crates
  come only from crates.io, licenses are on the allow list, no known vulnerabilities, no
  crate its authors gave up on, no yanked version, no wildcard versions.
- `cargo vet` (`supply-chain/`): every crate is audited, by the audits imported from
  Mozilla, Google and the Bytecode Alliance or by our own in `supply-chain/audits.toml`,
  recorded after reviewing the code. There are no exemptions.
- Releases are built with `cargo auditable build --release --locked`, which embeds the
  Rust dependency list in the binary; `cargo audit bin <binary>` checks a release for
  dependencies with known vulnerabilities.

## Upgrades

- A new version waits 7 days after its release before it is taken, unless it fixes a
  vulnerability.
- Commands that need crates.io (`outdated`, `update`, `upgrade`, `add`, `remove`) run
  through `scripts/cargo-online.sh`, which lifts the offline configuration for that one
  command and refreshes `vendor/` after a command that changes dependencies. While it
  runs, the `[env]` setting in `.cargo/config.toml` (`MACOSX_DEPLOYMENT_TARGET`) is off
  too.
- Review `git diff Cargo.toml Cargo.lock`, look at code changes with
  `cargo vet diff <crate> <old> <new>`, certify them with `cargo vet certify`, then run
  `cargo xtask deny --no-record` and the offline build and tests.
- In new or changed code, look for: new build scripts and procedural macros, `unsafe`,
  network and file system access, obfuscated or base64 strings, and new indirect
  dependencies.

## Unsafe Code in Dependencies

`cargo geiger` counts the `unsafe` code in each crate of the default build: functions,
expressions, impls, traits and methods, as used by the build / found in the crate. As
of this `Cargo.lock` (cargo-geiger 0.14.0, run with `cargo xtask geiger`, which writes the
full table to `audit/geiger/geiger.txt`):

- 123 crates: 56 use `unsafe`, 53 use none, and 14 also declare `forbid(unsafe_code)`.
- In all: 196 unsafe functions, 14,119 expressions, 217 impls, 28 traits and 486
  methods used by the build.
- rse itself has 8 `unsafe` blocks (17 expressions to geiger), each in a function that
  allows it by name: the calls into librclone (`src/rclone/ffi.rs`), `setlocale` and its
  text domain (`src/i18n.rs`), and `pre_exec` to start ssh in a session of its own
  (`src/rclone/system_ssh.rs`). File descriptors, `getuid` and `setsid` go through
  rustix's safe functions.

The crates with the most unsafe expressions used by the build:

| Crate | Functions | Expressions | Impls | Methods | What for |
|---|---|---|---|---|---|
| memchr 2.8.3 | 34/48 | 1992/2440 | 2/2 | 110/148 | vectorised byte search |
| hashbrown 0.17.1 | 1/1 | 1399/1612 | 20/23 | 86/99 | hash tables |
| rustix 1.1.5 | 63/436 | 1245/7521 | 9/22 | 28/62 | system calls |
| parking_lot_core 0.9.12 | 16/16 | 912/1343 | 0/0 | 8/58 | locks (crossterm, ratatui) |
| allocator-api2 0.2.21 | 2/3 | 819/903 | 12/13 | 28/32 | allocator API for hashbrown |
| aes 0.8.4 | 18/43 | 765/1113 | 0/0 | 0/0 | AES with CPU instructions (obscured passwords) |
| lock_api 0.4.14 | 0/0 | 669/669 | 32/32 | 24/24 | lock types |
| zmij 1.0.23 | 3/3 | 620/620 | 0/0 | 6/6 | float formatting (serde_json) |
| smallvec 1.16.3 | 1/1 | 611/613 | 7/7 | 14/14 | small vectors |
| lru 0.18.5 | 0/0 | 611/611 | 6/6 | 0/0 | caches (ratatui) |
| ryu 1.0.23 | 7/9 | 560/690 | 0/0 | 2/2 | float formatting |
| crossbeam-channel 0.5.17 | 2/2 | 494/503 | 6/7 | 12/14 | channels |
| compact_str 0.9.1 | 6/6 | 393/439 | 6/6 | 20/23 | strings (ratatui) |
| anyhow 1.0.104 | 12/15 | 354/360 | 2/2 | 12/12 | errors |
| linux-raw-sys 0.12.1 | 10/10 | 292/17551 | 0/0 | 20/2286 | system call numbers and types |

This code is not checked by rse's own tests, Miri or Kani; it rests on the audits
above and on its authors.

## Code Run at Build Time

Crates with a build script (`custom-build`) or procedural macros (`proc-macro`), as of
this `Cargo.lock`. The Windows and WebAssembly ones are not built on Linux or macOS.
Regenerate with:

```bash
cargo metadata --format-version 1 --locked | jq -r '
  .packages[]
  | select(.source != null)
  | select(any(.targets[]; .kind[] == "custom-build" or .kind[] == "proc-macro"))
  | "\(.name) \(.version) [\([.targets[].kind[]] | unique | map(select(. == "custom-build" or . == "proc-macro")) | join(","))]"' | sort
```

```text
anyhow 1.0.104 [custom-build]
clap_derive 4.6.7 [proc-macro]
crossbeam-utils 0.8.23 [custom-build]
darling_macro 0.24.1 [proc-macro]
derive_more-impl 2.1.1 [proc-macro]
displaydoc 0.2.7 [proc-macro]
document-features 0.2.12 [proc-macro]
fluent-template-macros 0.15.1 [proc-macro]
generic-array 0.14.7 [custom-build]
getrandom 0.4.3 [custom-build]
iana-time-zone-haiku 0.1.2 [custom-build]
indoc 2.0.8 [proc-macro]
instability 0.3.14 [proc-macro]
libc 0.2.191 [custom-build]
libm 0.2.16 [custom-build]
num-traits 0.2.19 [custom-build]
palette_derive 0.7.7 [proc-macro]
parking_lot_core 0.9.12 [custom-build]
portable-atomic 1.15.0 [custom-build]
proc-macro-hack 0.5.20+deprecated [custom-build,proc-macro]
proc-macro2 1.0.107 [custom-build]
quote 1.0.47 [custom-build]
rustix 1.1.5 [custom-build]
rustversion 1.0.23 [custom-build,proc-macro]
serde 1.0.229 [custom-build]
serde_core 1.0.229 [custom-build]
serde_derive 1.0.229 [proc-macro]
serde_json 1.0.154 [custom-build]
signal-hook 0.3.18 [custom-build]
strum_macros 0.28.0 [proc-macro]
thiserror 2.0.21 [custom-build]
thiserror-impl 2.0.21 [proc-macro]
unic-langid-macros-impl 0.9.6 [proc-macro]
wasm-bindgen 0.2.129 [custom-build]
wasm-bindgen-macro 0.2.129 [proc-macro]
wasm-bindgen-shared 0.2.129 [custom-build]
winapi 0.3.9 [custom-build]
winapi-i686-pc-windows-gnu 0.4.0 [custom-build]
winapi-x86_64-pc-windows-gnu 0.4.0 [custom-build]
windows-implement 0.60.2 [proc-macro]
windows-interface 0.59.3 [proc-macro]
zmij 1.0.23 [custom-build]
```
