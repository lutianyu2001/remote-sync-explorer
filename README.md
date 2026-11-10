# remote-sync-explorer

<p align="right">
    <strong>English</strong> | <a href="./README/README_CN.md">中文</a>
</p>

[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Release](https://img.shields.io/github/v/release/lutianyu2001/remote-sync-explorer)](https://github.com/lutianyu2001/remote-sync-explorer/releases/latest)
[![Crates.io Version](https://img.shields.io/crates/v/rse)](https://crates.io/crates/rse)

[![CI](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/ci.yml/badge.svg)](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/ci.yml)
[![audit](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Faudit.json)](SECURITY.md#ci)
[![pins](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fpins.json)](xtask/src/pins.rs)

remote-sync-explorer (`rse`) is a dual-pane terminal file manager for the local disk,
remote servers and any [rclone](https://rclone.org) remote. Between the local disk and
an sftp server that has [rsync](https://rsync.samba.org), it transfers with rsync, so
changed files send only what changed; everything else goes through rclone. Interrupted
transfers go on from where they stopped wherever the target allows it.

## Table of Contents

- [Overview](#overview)
- [Features](#features)
- [Security Audit](#security-audit)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Languages](#languages)
- [SFTP Through OpenSSH](#sftp-through-openssh)
- [Testing](#testing)
- [Compatibility](#compatibility)
- [Disclaimer](#disclaimer)
- [License](#license)
- [Author](#author)

## Overview

Each pane shows a location; copy, move, delete, rename and create directories between
them, with a transfer queue that shows live progress. rclone is compiled in as a static
library, so a release is a single binary called `rse`; rsync is the server's and your
system's own, never shipped with rse.

```text
╭───────╮╭────────────[1] Address─╮╭───╮╭────────╮╭───────────[3] Address─╮╭───╮
│ Local ││ /home/sky/projects     ││ ↻ ││ smb:// ││ nas:backup/2026       ││ ↻ │
╰───────╯╰────────────────────────╯╰───╯╰────────╯╰───────────────────────╯╰───╯
╭────────────────────────────[2] Local─╮╭───────────────────────────[4] Remote─╮
│ Name ▲                           Size││ Name ▲                           Size│
│ ..                                   ││ ..                                   │
│ rse/                            <DIR>││ photos/                         <DIR>│
│*huge.bin                       1.5 GB││ notes.md                       4.1 KB│
│*report-final-v3.pdf            2.4 MB││ report-final-v2.pdf            2.3 MB│
│ very-long-file-n…ing.tar.gz     12 MB││                                      │
│                                      ││                                      │
│ very-long-file-nam…-shortening.tar.gz││ refreshed at 14:03:27 · 1 new        │
╰───────────────────────────[2 marked]─╯╰──────────────────────────────────────╯
╭────────────────────────────────────────────────────────────────[5] Transfers─╮
│ 1 running · 1 queued                                                         │
│   copy huge.bin → nas:backup/2026       ██████░░░░░░  48%    52 MB/s   0:16 ✕│
│   copy report-final-v3.pdf → nas:backup/2026                         queued ✕│
╰──────────────────────────────────────────────────────────────────────────────╯
 F2 Rename  F5 Copy  F6 Move  F8 Delete  F10 Remote  / Filter  ? Help    rclone
 copy 2 items queued
```

## Features

- **Remotes**: browse the local disk and every remote in your `rclone.conf`; nothing has
  to be added to the rclone config.
- **Servers by address**: `sftp://`, scp's `me@box:/path`, `ftp://`, `smb://` and
  `webdav://` open without a remote in `rclone.conf`.
- **File operations**: copy, move, delete, rename, new directory. Moves and deletes are
  always confirmed, including drag and drop. Overwrites are asked about unless overwrite
  mode is on: the target is checked when an operation is queued and again as each
  transfer starts, and two transfers never write one file at once.
- **Two engines**: rsync between the local disk and an sftp server reached through the
  system ssh that has `rsync` (delta transfer, through the same login), rclone for
  everything else. The bottom row names the engine of copies between the two panes.
- **Resumed transfers**: a stopped file goes on from where it stopped, with rsync, and
  with rclone when the target is the local disk or an sftp server reached through the
  system ssh (downloads to the local disk resume from any remote). Identical files are
  skipped.
- **Transfer queue**: jobs run concurrently with bytes, speed and time left, and can be
  cancelled or retried one by one. A job runs while the panes show both of its ends
  (the one it deletes in, for a delete); opening another place in a pane pauses it, and
  coming back goes on. Unfinished jobs are kept for the next start. Leaving or quitting
  while a transfer that cannot resume runs names the files that would start over first.
- **Mouse**: click, double click, wheel, Ctrl+click or right click to mark, drag to copy
  or move.
- **SFTP through OpenSSH**: `~/.ssh/config` (ProxyJump, certificates, hardware keys)
  applies, with one shared login per server.
- **Terminal**: Ctrl+O opens your shell under the panes, in the pane's directory: on the
  local disk, or on a server through the same ssh login; it follows the pane as you
  browse.
- **Languages**: English and Simplified Chinese, following the system locale.

## Security Audit

[![review](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Freview.json)](audit/reviews)
[![deny](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fdeny.json)](https://github.com/lutianyu2001/remote-sync-explorer/actions/workflows/audit.yml)
[![vet](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fvet.json)](audit/vet)
[![geiger](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fgeiger.json)](audit/geiger)
[![miri](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fmiri.json)](audit/miri)
[![asan](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fasan.json)](audit/asan)
[![coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fcoverage.json)](audit/coverage)
[![kani](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Fkani.json)](audit/kani)
[![fuzz](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Flutianyu2001%2Fremote-sync-explorer%2Fbadges%2Ffuzz.json)](audit/fuzz)

rse handles passwords, ssh logins and the files on remote servers, so its code has been
audited for security. The audit was done by the project's maintainer, not by an
independent third party: it raises confidence in the code but is no guarantee. The audit
is repeated rather than done once: its latest results are committed with the code in
[`audit/`](audit/), and the badges above (with `audit` under the title for them all)
show whether each still holds for the code on main. At each release CI reruns the parts
whose results no longer hold (fuzzing four hours a target, Kani and Miri a few minutes).
[SECURITY.md](SECURITY.md) describes each part and how to run it, and
[SUPPLY_CHAIN.md](SUPPLY_CHAIN.md) the dependencies. The figures below are from the runs
of 2026-11-02.

| Area | What was audited | Extent |
|---|---|---|
| Threat model | Every place where input from outside, or a secret, reaches rse, reviewed by hand, with each decision written down | 18 decisions in SECURITY.md: where passwords go, host keys (also after falling back to rclone's built-in ssh), what a server can make ssh do, options slipped into ssh, `--rcd` over plain http, file permissions, names and text from servers on screen, overwrites, rsync's command line, resumed files, the saved queue |
| `unsafe` code | rse forbids `unsafe` except in functions that allow it by name; each file with `unsafe` has a review, done again whenever the file changes | 15 blocks, each a call that has no safe form (into librclone, `setlocale`, `sigaction`, signal-hook-registry, `pre_exec`), each with the reason it is sound; reviewed by ten independent reviewers ([`audit/reviews/`](audit/reviews/)), no undefined behaviour found |
| AddressSanitizer | The integration tests, which call into librclone, built with AddressSanitizer, which detects invalid memory accesses and leaks at run time | 8 test programs, no error (2026-11-09) |
| Dependencies | Sources, licenses, known vulnerabilities, abandoned and yanked crates (cargo-deny); an audit of every crate (cargo-vet); builds offline from checksummed sources | 220 crates audited: audits imported from Mozilla, Google and the Bytecode Alliance, and 189 of our own; no exemptions. Vulnerabilities are looked up again daily |
| Tests | Unit, screen snapshot and integration tests, including ssh logins against a private `sshd` | 268 tests, reaching 93% of the lines and 80% of the branches |
| Miri | The tests of the modules that need no process, socket or network, run under Miri, which detects undefined behaviour | 74 tests in 13 modules |
| Fuzzing | Random input to every reader of untrusted text, checked against the invariants in SECURITY.md | 6 targets (addresses, ssh options, rsync's command line and output, text shown on screen, the config file, passwords), an hour each: from 1.6 to 65 million inputs per target |
| Kani proofs | Properties proved for every input within a bound | Mouse hit-testing never overflows, for all inputs (3 s, 105 MiB); key names are never cut inside a character, up to 7 bytes (75 s, 1008 MiB) |
| Release build | `overflow-checks = true` | An integer overflow stops rse instead of wrapping |

What this does not show:

- Rust rules out memory errors in rse's own code, not logic errors; the review and the
  tests are what look for those.
- 60 of the 128 crates in the default build use `unsafe` code. That code rests on its
  authors and on the audits above, not on rse's tests, Miri or Kani.
- The rclone compiled into rse is Go code, which none of these checks covers but for
  rse's own rc calls in `librse/`, which `go vet` and `go test` check; Go checks its
  modules against `librse/go.sum` when it downloads them.
- Kani proves a property only up to its bound, and fuzzing tries samples. Not proved: the
  decoding of obscured passwords, and the readers of addresses, ssh options, connection
  strings and the config file; tests of every short input and fuzzing check them.

## Prerequisites

- Linux, or macOS 13 or newer (Intel or Apple Silicon); Windows is not supported
- Rust ≥1.88
- Go ≥1.26 (required by rclone v1.75), on `PATH` or named by the `GO` environment
  variable
- A C toolchain (`cc`) for cgo; libclang is **not** needed
- Network access to the Go module proxy for the first build
- OpenSSH ≥8.4 for passwords and one-time codes asked through the system `ssh`
  (optional)
- rsync ≥3.1 for transfers with rsync (optional; without it rclone carries them out).
  macOS's own `rsync` is openrsync, which rse does not use: install rsync, for example
  with Homebrew

## Installation

### From crates.io (Recommended)

1. Install Rust, Go and a C toolchain (see [Prerequisites](#prerequisites)).
2. Install `rse`:

   ```bash
   cargo install --locked rse
   ```

The first build compiles rclone, with rse's own additions from `librse/`, which takes a
minute or more. The binary is around 90–115 MB because it contains all of rclone. The
build checks `librclone.h` against the hand-written bindings and fails if rclone ever
changes the four exported functions; on glibc, it checks the one libintl function rse
declares the same way.

### Building from Source

```bash
cargo --config net.offline=false vendor --locked --versioned-dirs --sync fuzz/Cargo.toml vendor
cargo build --release --locked
```

Build-time environment variables, for either way of building:

| Variable | Effect |
|----------|--------|
| `RSE_RCLONE_VERSION` | Build against another rclone version (default: the `v1.75.2` that `librse/go.mod` pins); Go downloads it |
| `RSE_LIBRCLONE_DIR` | Directory with a prebuilt `librclone.a` and `librclone.h`; skips the Go build entirely (offline builds, packaging) |
| `GO` | The Go binary to use |
| `GOTOOLCHAIN` | Passed to Go as is; when unset, rse sets it to `local` so an old Go fails clearly instead of downloading a toolchain |
| `MACOSX_DEPLOYMENT_TARGET` | Oldest macOS the build runs on; `.cargo/config.toml` sets `13.0`, the oldest the embedded Go runtime supports |
| `CC_<target>` / `TARGET_CC` | C compiler cgo uses for the target, e.g. `CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc` |

Note: release binaries include rclone (MIT) and its Go dependencies, and must ship with
their license texts. `scripts/third-party-licenses.sh` collects the license texts of the
Rust and Go dependencies (needs `cargo-about` and `go-licenses`); a build with
`THIRD_PARTY_LICENSES` pointing at its `licenses.txt` embeds them, for `rse --licenses` to
print, and other builds print a note instead. Among them are Go
modules under LGPL-3.0 and MPL-2.0, whose source ships with a release; see
[SUPPLY_CHAIN.md](SUPPLY_CHAIN.md#licenses-in-a-release).

### Cross-Compiling

`GOOS` and `GOARCH` follow cargo's target, so librclone is cross-compiled along with
rse:

```bash
rustup target add aarch64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
# → target/aarch64-apple-darwin/release/rse
```

Between macOS architectures nothing else is needed: cgo uses `clang -arch <arch>`. For
another Linux architecture, cgo uses `<arch>-linux-gnu-gcc` (`i686` for x86, and arm's ABI
after it, as in `arm-linux-gnueabihf-gcc`) unless `CC_<target>` says otherwise, and rustc
needs the same compiler as its linker (`CARGO_TARGET_<TRIPLE>_LINKER`). A musl target
takes the [`rcd` feature](#without-go-the-rcd-feature): with rclone embedded, rse cannot
start on musl, since Go's runtime, linked in as a C archive, reads its arguments from those
of its initialiser, which musl does not pass. Two macOS builds can be joined into one
universal binary:

```bash
lipo -create -output rse <x86_64 rse> <arm64 rse>
```

### Without Go: The `rcd` Feature

This build talks to an `rclone rcd` you run yourself instead of embedding rclone:

```bash
cargo build --release --locked --no-default-features --features rcd
rclone rcd --rc-user <user> --rc-pass <password> &
RCLONE_RC_PASS=<password> rse --rcd http://localhost:5572 --rc-user <user>
```

Plain `http://` is accepted only to this machine (`localhost`, `127.0.0.1`, `[::1]`); an
rcd elsewhere needs `https://` (`rclone rcd --rc-cert … --rc-key …`).

### Manual Page

`rse --man` prints the manual, `man rse`, in the interface language. To put the English
and the Chinese one where `man` finds them, beside the directory `rse` is in
(`~/.cargo/share/man` after `cargo install`):

```bash
man_dir="$(dirname "$(command -v rse)")/../share/man"
mkdir -p "$man_dir/man1" "$man_dir/zh_CN/man1"
rse -c ui.language=en --man > "$man_dir/man1/rse.1"
rse -c ui.language=zh-CN --man > "$man_dir/zh_CN/man1/rse.1"
```

`man rse` then shows the Chinese one in a `zh_CN` locale. A release archive has both in
`man/`.

## Usage

```text
rse [--config <PATH>] [-c <KEY=VALUE>]... [--rclone-config <PATH>] [--ask-password]
     [--forget-password <LOGIN>] [--rcd <URL> --rc-user <USER> --rc-pass <PASS>]
     [-T] [--version] [--licenses] [--man] [-h] [LEFT] [RIGHT]
```

- `LEFT`
    - Initial location of the left pane, e.g. `~/work`
- `RIGHT`
    - Initial location of the right pane, e.g. `nas:backup`. Given alone, a remote opens
      here instead of on the left
- `--config <PATH>` (default: `~/.config/rse/config.toml`)
    - remote-sync-explorer config file
- `-c`, `--set <KEY=VALUE>`
    - Set a setting for this run, e.g. `ui.show_hidden=true`; may be given more than once
      (see [Configuration](#configuration))
- `--rclone-config <PATH>`
    - rclone config file
- `--ask-password`
    - Ask for the rclone config password before starting
- `--forget-password <LOGIN>`
    - Forget the ssh password remembered for `LOGIN` (`user@host`) and exit
- `--rcd <URL>`
    - Use an external `rclone rcd` instead of the embedded rclone (needs the `rcd`
      feature)
- `--rc-user <USER>`, `--rc-pass <PASS>`
    - User and password for `--rcd`; `RCLONE_RC_USER` and `RCLONE_RC_PASS` are read when
      they are not given, and keep the password out of `ps`
- `-T, --no-terminal`
    - Turn the [terminal](#terminal) off: no shell can be opened, locally or on a server
- `--version`
    - Print the remote-sync-explorer and rclone versions
- `--licenses`
    - Print the licenses of remote-sync-explorer and of the third-party code built into it
- `--man`
    - Print the manual (`man rse`) in the interface language (see
      [Manual Page](#manual-page))
- `-h, --help`
    - Print help

An encrypted rclone config needs its password before rse starts: use
`--ask-password`, or export `RCLONE_CONFIG_PASS` in the shell that starts rse.

### Getting Started

1. Run `rse`. The left pane shows your home directory, the right one your first rclone
   remote (or your home directory again); `rse ~/work sftp://me@box/srv` starts in those
   two places instead.
2. Move with the arrow keys (or `j` and `k`), open a directory with Enter, go up with
   Backspace. Tab switches between the panes.
3. To open a server, press `:` and type its address, such as `sftp://me@box/srv` (see
   [Locations and Addresses](#locations-and-addresses)). To open one of your rclone
   remotes, press F10 (or `g`): a list of the local disk and the remotes in
   `rclone.conf` opens; type to narrow it, and Enter opens the one chosen. Remotes are
   added with `rclone config`; with none, the list says so.
4. Mark entries with Space and press F5 to copy them into the directory the other pane
   shows, or F6 to move them there. With nothing marked, the entry under the cursor is
   copied. The Transfers window shows the progress; Ctrl+T enters it, to cancel or retry
   a job.
5. `?` lists every key, and `man rse` has the whole manual (see
   [Manual Page](#manual-page)). `q` quits.

The screen, from the top: the address row over each pane; the panes, whose last row
shows the filter and the whole name of the entry under the cursor, and for a few
seconds after a refresh (Ctrl+R or ↻) when it ended and what changed; the
[terminal](#terminal) (Ctrl+O), while it is open; Transfers; the key hints, with the
engine a copy between the panes would use (`rsync` or `rclone`) in reverse video at the
right; and the message line, where messages appear for a few seconds (errors longer;
jobs just queued that end meanwhile say how they went) and an input method shows what it
is composing.

### Locations and Addresses

A location is a local path (`~/x`, `/abs`, `relative`) or an rclone path (`nas:`,
`nas:backup/2026`, `sftp:/absolute/path`). Remotes are managed with `rclone config` as
usual.

Names are shown as they are: a character that would not show, such as a control
character, a zero-width space or a mark that reorders text, appears as `�`. An entry a server lists under a
name that could lead out of the directory (`..`, or one with `/` in it) is left out,
and the message line says how many were.

With a single location, a remote or server address opens in the right pane and the left
pane shows the directory rse was started in: `rse sftp://me@box` puts the working
directory on the left and `box` on the right. A single local path opens on the left.

A server can also be opened by its address, without a remote in `rclone.conf`:

| Address | Opens |
|---------|-------|
| `sftp://me@box/srv`, or scp's `me@box:/srv` | sftp through the system `ssh` (see [SFTP Through OpenSSH](#sftp-through-openssh)) |
| `ftp://me@box/pub`, `ftps://…` | FTP; `ftps` is implicit TLS on port 990 |
| `smb://me@nas/share/dir` | SMB; the first name is the share |
| `webdav://host/path`, `webdavs://…` | WebDAV over http or https (`dav://`, `davs://` too) |

A port goes after the host (`sftp://me@box:2222/srv`). sftp paths are absolute;
`sftp://me@box/~/docs`, or no path at all, is in the home directory, and scp's
`me@box:docs` means the same. Without a user, FTP logs in as `anonymous`.

When a login needs a password and the address has none, rse asks for it (on the
terminal before starting, for an address given on the command line) and keeps it in
memory until it quits; a failed listing forgets it, so the next attempt asks again.
`ftp://me:secret@box` also works, but leaves the password in your shell history.

Any other rclone backend can be opened with a connection string, such as
`:s3,provider=AWS,env_auth=true:bucket/path`. A value holding `:` or `,` is quoted with
`'` or `"`, as in `:sftp,ssh='ssh -p 2222 me@box':`.

### Address Bar

Above each pane is a row of three boxes: the protocol (`sftp://`, `smb://`, `s3://`, …,
or `Local` on the local disk), the address bar, and one button: `↻` reloads the pane.
Only the address bar can be focused (its key is `1` or `3`); the boxes beside it light
up with it, and clicking the protocol box does nothing. A path too long for the bar
shows only the current directory after `«`, as Windows Explorer does (`« photos`);
editing shows it in full.

Enter in the focused address bar, `:` in a pane, or clicking the address bar edits the
address: a window opens under the address bar, with an arrow from the bar to it, and the
bar reads `[Revising]` meanwhile (in a terminal too short for that window the bar itself
is edited). An address too long for the field scrolls; a highlighted `<` or `>` at
either end says text is cut off there, as in readline and nano. The button turns into
`>` once the address differs, and goes there (as Enter does; Enter on an unchanged
address reloads). Esc or a click elsewhere cancels. While editing, the protocol box
follows what you type: typing `ftp://` moves it there, Backspace at the start of the
field takes it back, and Ctrl+U clears the field and the protocol.

Marked entries are counted in the pane's bottom right corner (`[2 marked]`). Esc, or a
click on blank space in the pane, clears the marks.

### Keys

All keys can be changed in the config file. F keys have letter aliases for keyboards
without them.

| Key | Action |
|-----|--------|
| Tab | Switch panes |
| 1 / 2 / 3 / 4 / 5 | Focus a window: left address bar / left pane / right address bar / right pane / Transfers (the key is shown in each window's top right corner; a pane is named Local or Remote after what it shows) |
| ↑ ↓ / j k, PgUp PgDn, Home End | Move the cursor |
| Enter / l | Open directory |
| Backspace / h | Parent directory (the cursor lands on the one you left) |
| Space / Insert | Mark and move down |
| Esc | Clear marks; in the address bar, go back to the entries |
| a | Mark all / unmark all |
| / | Filter by name, on the pane's last row (Enter keeps it, Esc clears it; the entries still scroll meanwhile) |
| s | Sort by name, size, time |
| S | Reverse the sort order |
| . | Show or hide dot files |
| : | Go to a path, remote or address, e.g. `nas:backup`, `ftp://me@box/pub` |
| F10 / g | Choose what the pane shows from a list of the local disk and the remotes in `rclone.conf` (type to narrow it; with no remotes, it says how to add one) |
| Ctrl+U | Swap panes |
| Ctrl+R | Reload the pane |
| F5 / c | Copy to the other pane |
| F6 / m | Move to the other pane |
| F2 / r | Rename the entry under the cursor |
| F7 / n | New directory |
| F8 / Delete | Delete |
| O | Overwrite mode on or off: copies, moves and renames replace existing files without asking (off at every start; a red row across the top shows it is on) |
| Ctrl+T | Enter or leave the transfer queue; there x cancels, r retries, c clears finished jobs |
| Ctrl+O | Terminal under the panes, in the pane's directory, locally or on a server reached through ssh; in the terminal, back to the panes (see [Terminal](#terminal)) |
| E | Error log (last 100) |
| ? | Help |
| q | Quit |

Copy and move act on the marked entries, or on the entry under the cursor when nothing
is marked. The target is the other pane's directory.

With a Chinese or Japanese input method on, the punctuation keys still work: `？`, `：`,
`／` (or `、`) and `。` act as `?`, `:`, `/` and `.`.

### Mouse

Mouse support is on by default (`[ui] mouse = true`). Every mouse action also has a key.

| Action | Effect |
|--------|--------|
| Click | Move the cursor there, clear marks, focus that pane |
| Ctrl+click or right click | Mark or unmark, keep the other marks |
| Double click | Open a directory; on `..` go up |
| Wheel | Scroll the pane under the pointer (focus and cursor do not move) |
| Click a column header (Name, Size, Modified) | Sort by it; again, the other way round |
| Click blank space in a pane | Clear marks, focus that pane |
| Click the address bar | Edit the address |
| Click the ↻ / > button right of the address | Reload that pane / go to the edited address |
| Drag | Copy or move (see below) |
| Click a queue row / its ✕ | Select the job / cancel it |
| Click a key hint at the bottom | Run it |
| Click the terminal / wheel over it | Give it the keys / scroll back through its output |

Dragging: press on an entry and move. If that entry is marked, all marked entries are
dragged, otherwise only it. Drop on a directory to put the entries inside it, anywhere
else in a pane to put them in that pane's directory. Dragging near the top or bottom
edge scrolls. Esc cancels. On release a menu offers *Copy here*, *Move here* and
*Cancel*; afterwards the same conflict checks and confirmations as F5/F6 apply. Set
`drop_action = "copy"` to copy without the menu.

Note: while mouse support is on, hold Shift to select text in most terminals. In tmux
use `set -g mouse on`. On macOS some terminals turn Ctrl+click into a right click (which
also marks) or open their own menu; `multi_select_modifier = "alt"` switches to
Alt+click.

### Terminal

Ctrl+O opens a terminal across the bottom of the panes: your login shell on the local
disk, or on a server reached through the system ssh (see
[SFTP Through OpenSSH](#sftp-through-openssh)) through the master connection rse already
holds, so there is no second login. Other remotes have no terminal. It starts in the
directory the pane shows, on a clean screen: what a server prints before the shell (a
banner, the message of the day) is wiped, and bash, zsh and fish get Ctrl+L as their
first input, which clears what their startup files print.

While the terminal has the keys (its border is green), every key goes to the shell,
Ctrl+C and Esc included. Ctrl+O gives the keys back to the panes, and from there to the
terminal again; the terminal stays open meanwhile. When the pane it was opened from goes
to another directory there (on the same disk or server), the terminal follows: rse types
a `cd` into the shell, after clearing a half-typed line, with a leading space that keeps
it out of the history of most shells. While a command runs in the shell (rse asks with
`ps`), the `cd` waits and is typed once the command ends, so it never lands in a program
such as `vim`. A directory whose name holds a control character is not followed. `exit`
or Ctrl+D closes the terminal; the wheel scrolls back through its output. If a program
you run needs Ctrl+O, bind the terminal to another key (`terminal = [...]` under
`[keys]`). `-T` (`--no-terminal`) turns the terminal off for the session, locally and on
servers, for when no shell should be opened from rse.

### Conflicts, Moves and Cancel

- When the target already has an entry with the same name, rse asks: overwrite, skip,
  overwrite all, skip all or cancel. Skip is the default, and names that differ only in
  case count as the same (many targets, such as macOS disks and cloud drives, do not
  tell them apart). The target is listed again for this check, even when a pane shows
  it, so a file that appeared there since is not overwritten without asking.
- Each job lists its target again as it starts and when it is retried, so a file that
  arrived while the job waited is not overwritten either. A listing cannot see a
  transfer in flight, so a job also waits while another that writes, moves or deletes
  an entry of its name (in any case) in the same directory is under way or queued
  before it: two jobs never write one file at once, and the later one sees what the
  earlier wrote. A job that finds its name taken does not start: it shows as failed in
  the queue and in the error log, and a dialog asks about it, with overwrite, skip, the
  same for all the jobs waiting on a conflict, and cancel, which leaves every job not
  yet decided failed (retrying one checks again). Items you chose to overwrite when the
  operation was queued are not asked about again. A file that something else puts in
  the target between the last check and the start of the transfer is still overwritten.
- A rename checks the new name against the server, not only against the pane, and is
  refused when another entry has it (in any case).
- Overwrite mode (`O`) leaves these checks out: copies, moves and renames replace what
  the target has without asking, though jobs still wait for one another as above. It is
  off at every start; while it is on, a red row across the top of the screen says so,
  and the copy and move confirmations repeat it.
- Overwriting goes on where it can: identical files are skipped (by size and
  modification time, or by checksum with `checksum = true`), a file a stopped transfer
  left half done is resumed, and different files are replaced. Copying a directory
  merges it into an existing one this way.
- A move across remotes is a copy followed by deleting the source. If it is cancelled
  midway, the files copied so far are already gone from the source. After a directory
  move rse removes the emptied source directory.
- Cancelling or pausing a transfer keeps what it has done of the file under way, so that
  a retry goes on from there: rclone keeps it as `<name>.rse-partial` beside the target,
  rsync in a `.rse-partial/` directory there. They are never copied by rse, nor are your
  own files and directories of those names; delete them if you give the transfer up. A
  target that cannot resume (S3 and other cloud storage, or sftp through rclone's
  built-in ssh) is left without a partial file.
- On macOS, rclone clones files that are copied within one APFS volume instead of
  copying their bytes, so such copies finish at once.

## Configuration

Each setting is taken from the highest of these layers that has it:

```text
the defaults  <  your config.toml  <  the environment (RSE_...)  <  --set on the command line
```

- **Your file**: `~/.config/rse/config.toml` (or `$XDG_CONFIG_HOME/rse/config.toml`, or
  `--config <PATH>`). It is optional and needs only the settings you change. Unknown
  settings and values of the wrong type are errors that give the file, line and column,
  so typos do not go unnoticed.
- **The environment**: `RSE_` and the key upper-cased, dots made underscores, e.g.
  `RSE_JOBS_MAX_CONCURRENT=4` or `RSE_UI_MOUSE=false`; a list is comma-separated:
  `RSE_KEYS_COPY=F5,c`.
- **The command line**: `--set KEY=VALUE` (or `-c`), as many as needed, for this run
  only: `rse -c ui.show_hidden=true -c jobs.engine=rclone`.

A value of the environment or the command line that does not fit its setting is an
error that names the variable or says it is the command line's.

The settings and their defaults, as [`src/defaults.toml`](src/defaults.toml) has them,
with `[keys]` (every action's defaults are on the help screen, `?`):

```toml
[rclone]
config = ""                     # rclone.conf path; empty = rclone's default
transfers = 4                   # rclone --transfers
checkers = 8                    # rclone --checkers
browse_threads = 2              # threads for listings and other short calls
system_ssh = true               # sftp through the system ssh, one login per server

[ui]
left = "~"                      # left pane at start
right = ""                      # right pane at start; empty = first remote's root
show_hidden = false
mouse = true
multi_select_modifier = "ctrl"  # or "alt"
drop_action = "ask"             # or "copy": copy on release without the menu
language = "auto"               # "en", "zh-CN", or "auto": Chinese for a zh* locale

[jobs]
max_concurrent = 2              # jobs running at once; the rest wait
poll_ms = 500                   # progress refresh
engine = "auto"                 # or "rclone": never rsync
checksum = false                # tell files apart by checksum, not size and time

[keys]
copy = ["F5", "c"]
move = ["F6", "m"]
# Any action from the help screen (?) by its name: switch_pane, up, down,
# page_up, page_down, home, end, enter, parent, mark, mark_all, filter, sort,
# sort_reverse, toggle_hidden, goto, remote, swap, refresh, copy, move, rename,
# mkdir, delete, overwrite_mode, queue, terminal, focus_left_address,
# focus_left, focus_right_address, focus_right, focus_queue, error_log, help,
# quit, queue_cancel, queue_retry, queue_clear.
# Keys are written like F5, c, E, Ctrl+U, Alt+x, Space, Enter, PgDn, Delete.
```

rclone's own log messages go to `~/.cache/rse/rclone.log` (`~/Library/Caches/rse/` on
macOS) while the interface is open. Unfinished jobs are saved in
`~/.local/state/rse/queue.json` (or `$XDG_STATE_HOME/rse/queue.json`), readable by you
alone; a server opened by address is saved without its password, which it gets back
when you open the server again.

rse was called rclone-explorer-tui (`rext`) before. At its first start it moves
`~/.config/rext/`, `~/.cache/rext/` and `~/.ssh/rext-passwords` to the new names, where
nothing of the new name is there yet.

## Languages

rse speaks English or Simplified Chinese: `language` in `[ui]` picks one, and `auto`
(the default) follows the system locale, as gettext does (`LANGUAGE`, `LC_ALL`,
`LC_MESSAGES`, `LANG`), on macOS too; with none of them set, the system's language
(on macOS, the one in System Settings). This covers the interface, `rse --help` and its errors, and the
errors and prompts at start-up; the setting already applies to `--help`.
System errors such as "No such file or directory" follow it too when glibc has the
translations (Linux). What rclone, ssh and the reader of the settings say stays in
English, and so does the log file.

The manual, `man rse`, is in both languages too (see [Manual Page](#manual-page)).

The texts live in [Fluent](https://projectfluent.org) files, `locales/en/rse.ftl` and
`locales/zh-CN/rse.ftl`, built into the binary.

## SFTP Through OpenSSH

rse reaches sftp servers through the system `ssh` by default, so everything in
`~/.ssh/config` applies (host aliases, ProxyJump, keys, …). Before the first use of a
server it logs in once and keeps that OpenSSH master connection open; all the
connections rclone makes go through it. A server therefore sees one login per session,
not one for each of rclone's parallel connections, which keeps tools such as fail2ban
from banning you. The connection is shared by every remote or address for the same
account on the same server, closes when rse quits, and otherwise ends 15 minutes after
its last use. Remotes reaching one account with different settings, such as their own
`known_hosts_file`, get connections of their own. Whatever `~/.ssh/config` says, these
connections forward no agent, X11 or ports and run no local or remote command, as
OpenSSH's own sftp does; the [terminal](#terminal) alone runs a command on the server,
your login shell, and forwards nothing either.

This covers `sftp://` and `me@box:` addresses, `:sftp,…` connection strings, and sftp
remotes in `rclone.conf`. An sftp remote reached through another remote (a `crypt`,
`alias` or `union` over it, for example) is opened by rclone itself, as rclone always
does, with its built-in ssh; to have it use OpenSSH there too, give that sftp remote an
`ssh = …` line of its own. A remote's `host`, `user`,
`port`, `key_file`, `pubkey_file` and `known_hosts_file` become ssh options. A remote
with its own `ssh = …` line is left as it is. A `host` or `user` that starts with `-`
uses rclone's built-in ssh, since the system `ssh` would read it as an option. So does
a remote with options only the built-in ssh honours, such as `key_pem`, `ciphers` or a
proxy, still checking host keys against `known_hosts` as ssh would, and one that pins
its host keys (`host_keys`, `pin_host_key`), which rclone then checks. A
`known_hosts_file` of the remote's own is the file ssh checks; a server not in it is
asked about as below, where rclone itself would refuse it.

Logging in can need a password, a key passphrase or a one-time code. A password
(`pass`) or key passphrase (`key_file_pass`) kept in `rclone.conf` is given to ssh
without asking; anything else is asked in a dialog, such as `me@box's password:` or a
verification code. A `pass` or `key_file_pass` not made by `rclone obscure` (written in
plain text, say) is not used: rse asks for it instead, and a window under the dialog
says why, as it does when the server refuses the `pass` or the `key_file_pass` does not
unlock the key. This needs OpenSSH 8.4 or newer (`ssh -V`); with an older one,
remotes that need a password use rclone's built-in ssh as before. The password and
passphrase from `rclone.conf`, and a remembered password, only answer ssh's own question
for that account or for the remote's `key_file`: a jump host's question, or a server
asking for a key passphrase, is put to you, and what you type there is not remembered.
A login that makes no progress for two minutes, not counting the time you take to
answer, is given up.

### Remembered Passwords

The password dialog has a **Remember password** box (Tab or a click toggles it). When it
is ticked and the login succeeds, rse keeps the password in `~/.ssh/rse-passwords` and
gives it to ssh the next time that account logs in, after any password in
`rclone.conf`. The file has one line per account:

```text
me@box.example.com 3T9PgUymd9w-jZk2CoHnCElf
root@10.0.0.5:2222 dlGK7HGgNL9oJ5cxgg8FeI5Ne_Yjjg
```

The account is written as ssh resolves it (`ssh -G`): the real user and host name
rather than a host alias, with `:port` when it is not 22. The password is obscured the
way `rclone.conf` keeps passwords: `rclone reveal` shows it, and a value made with
`rclone obscure` can be written in by hand. Obscuring only keeps a password from being
read at a glance, so, as ssh does with private keys, rse uses the file only while it is
readable by you alone (`chmod 600`). Other lines, such as `#` comments, are left alone.

When rse asks for a password although one was remembered, a window under the dialog
says why: ssh refused the remembered password, the file cannot be used (for example
because others can read it), or saving the password failed last time. When ssh asks
again for another reason, the window shows what ssh said, such as
`ssh: Permission denied, please try again.` A new password remembered then replaces a
refused one, and one not remembered removes it. To forget a password, delete its line,
or run:

```bash
rse --forget-password <user>@<host>
```

These problems also go to the rclone log.

### Fallback to rclone's Built-in SSH

rclone's built-in ssh is used instead (logging in once per connection, with rclone's own
settings) when:

- there is no `ssh` program, or `system_ssh = false` is set under `[rclone]` in rse's
  config;
- the remote needs something only the built-in ssh can do: an inline key (`key_pem`), a
  proxy or cipher setting;
- logging in through the system ssh fails, for example with a refused key. The built-in
  ssh then checks the host key against ssh's `known_hosts` file (unless the remote sets
  `known_hosts_file`), so a server ssh has never accepted is refused rather than
  trusted. With `StrictHostKeyChecking no` or `accept-new` in your ssh config, a new
  server is let in as OpenSSH would; one listed in any file ssh reads, under any name
  ssh knows it by, must show the key listed there. A changed key is refused, where
  OpenSSH with `no` would go on with password logins turned off. When rse cannot tell
  (a `%` token in the file's path, a `KnownHostsCommand`), it checks.

The reason is written to the rclone log. Two failures are errors instead of reasons to
fall back: a host key that differs from the known one (the built-in ssh does not check
host keys), and a cancelled question (the built-in ssh would only fail again). A server
not seen before is shown with its key's fingerprint and added to `known_hosts` only if
you answer Yes (No is the default); a `StrictHostKeyChecking` in your ssh config applies
as set. If your ssh config already sets up `ControlMaster` and
`ControlPath`, rse uses those; otherwise its sockets go in `$XDG_RUNTIME_DIR/rse`,
where no other user can create anything, or in `/tmp/rse-<uid>` on systems without
it, such as macOS. When that directory cannot be used, for example because another user
created it first, sftp uses rclone's built-in ssh as above, still checking host keys,
and rse shows why on startup. None of this applies with `--rcd`: that rclone runs on
its own machine.

## Testing

```bash
cargo test                       # unit, snapshot and integration tests
cargo clippy --all-targets       # pedantic, unwrap_used and expect_used are errors
cargo fmt --check
(cd librse && go vet ./... && go test ./...)   # rse's own rc calls, built into librclone
```

- Unit tests drive the interface logic with a fake backend; screen snapshots are kept
  with [insta](https://insta.rs) (`cargo insta review` after an intended change).
- Screen snapshots are taken in English; a few are repeated in Chinese to check that
  wide characters line up.
- Integration tests run the embedded rclone on the local and memory backends, cancel
  transfers midway, and browse an sftp remote through a private `sshd` (skipped when
  `sshd` is not installed).
- `tests/resume.rs` stops transfers midway and checks that the next one resumes, from
  a remote to the local disk and to an sftp server through the system ssh.
- `tests/rsync.rs` copies and moves with rsync both ways through a private `sshd`,
  stops transfers midway and checks that the next run sends only what was missing
  (skipped when `sshd` or rsync ≥3.1 is not installed).
- `tests/concurrency.rs` checks that rclone calls do not block each other.
- `cargo test` also checks that both language files define the same messages and that
  every message is used.

The window frames, the arrow between windows and the scrolling text field come from
[lazyui-ratatui](https://github.com/lutianyu2001/lazyui-ratatui).

## Compatibility

Tested on:

- Ubuntu 24.04.4 LTS (x86_64)
  - Rust 1.99.1
  - Go 1.27.3
  - [rclone](https://rclone.org) v1.75.2
  - [OpenSSH](https://www.openssh.com) 9.6p1
- macOS 15.7.9 (Sequoia) on Apple Silicon

## Disclaimer

This project is not affiliated with, endorsed by, or sponsored by rclone or its maintainers. "rclone" refers to the software developed by Nick Craig-Wood (https://github.com/rclone/rclone) and is referenced here solely for descriptive purposes.

This project is not affiliated with, endorsed by, or sponsored by rsync or its maintainers. "rsync" refers to the software developed by the rsync project (https://github.com/RsyncProject/rsync) and is referenced here solely for descriptive purposes.

## License

Copyright 2026 Tianyu (Sky) Lu

The entire repository is licensed under the Apache License, Version 2.0 
(the "License"); you may not use any file in this repository except in 
compliance with the License. You may obtain a copy of the License at

```plaintext
http://www.apache.org/licenses/LICENSE-2.0
```

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.

## Author

Tianyu (Sky) Lu
