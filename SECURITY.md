# Security

How rse's own code is checked, and the decisions that keep secrets and files safe. How
the dependencies are checked is in [SUPPLY_CHAIN.md](SUPPLY_CHAIN.md).

## Audit

The tools and the nightly toolchain these need are pinned in `xtask/src/pins.rs`;
`cargo xtask setup` installs them, and each `cargo xtask <task>` checks them first and
runs heavy work one at a time under a memory limit (see Running the Audit below).
`cargo xtask outdated` says when a pinned version is due: a release a month newer, or
a known vulnerability in it; nothing is updated without the user's choice.

| Check | What it covers | Command |
|---|---|---|
| Lints | `unsafe` only in functions that allow it by name, one unsafe operation per block, each with a `// SAFETY:` comment; no `unwrap`/`expect` | `cargo clippy --all-targets` |
| Unit and integration tests | Exhaustive checks of the parsers on short inputs; ssh logins against a private `sshd` | `cargo test` |
| Go vet and tests | rse's own rc calls in `librse/` (resumed transfers), against the local backend and an sftp server in the test binary | `cd librse && go vet ./... && go test ./...` |
| Fuzzing | The parsers of untrusted text (below), against panics, hangs and broken invariants; `fuzz/` is a workspace of its own | `cargo xtask fuzz` |
| Kani | Proofs for every input: mouse hit-testing never overflows (1 s, 95 MiB); key names are never cut inside a character, up to 7 bytes (31 s, 1009 MiB) | `cargo xtask kani` |
| Miri | Undefined behaviour in the tests of the pure modules, dependencies included | `cargo xtask miri` |
| Branch coverage | Which branches the tests reach, to find decisions whose conditions are not each tested ([cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), nightly) | `cargo xtask coverage` |
| Release build | `overflow-checks = true`: an integer overflow stops rse instead of wrapping | `cargo build --release` |

### Fuzzing

The targets in `fuzz/fuzz_targets/` need a nightly toolchain and
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). They reach private parsers through
`src/fuzz.rs`, which is only built under `cargo fuzz`.

| Target | Input | Invariants |
|---|---|---|
| `address` | The address bar | An on-the-fly fs carries exactly the typed host, user and password, and no other option |
| `ssh_command` | sftp options from rclone.conf or a connection string | No value becomes an ssh option or another argument; rclone reads back exactly the intended command and options |
| `text` | Names and messages drawn in cells | Cutting and wrapping keep to the width (a line is wider only when one character is), lose no text and end; control characters, bidirectional marks and zero-width spaces are marked, and letters, digits and punctuation never are |
| `config` | `config.toml` | What is read writes back to the same settings |
| `secrets` | Obscured passwords, the password file, ssh's version | base64 decoding is the exact inverse of encoding; the password file and ssh's version are read without panicking |
| `rsync` | rsync's command line, from an sftp remote's ssh options and a transfer's paths; rsync's output | No path becomes an rsync option, or is read as another server's or an rsync daemon's; rsync reads back exactly ssh's command and destination; the output is read the same however it comes in pieces, and the total is narrowed down to the true one |

```bash
cargo xtask fuzz 300 --no-record   # every target for five minutes, not recorded
cargo +<nightly> fuzz fmt ssh_command fuzz/artifacts/ssh_command/<crash>   # show a failing input
```

`<nightly>` is the toolchain pinned in `xtask/src/pins.rs`.

### Miri

```bash
cargo xtask miri
```

It runs the tests of the modules listed in `xtask/src/tasks/miri.rs` (`MODULES`), with
`MIRIFLAGS=-Zmiri-disable-isolation`, skipping `base64_round_trips_every_short_input`.

Isolation is off so that tests can read and write their temporary files and
`/dev/urandom`. The skipped test is too slow under Miri (it tries every input up to 2
bytes); the module names end in `::` so that no interface test whose name merely
contains one is picked up.

Tests that run processes, use sockets, wait on worker threads with timeouts (the
interface tests) or call rclone cannot run under Miri, nor can the FFI to librclone;
`build.rs` checks the declarations in `src/rclone/ffi.rs` against `librclone.h`, and the
one in `src/i18n.rs` against glibc's `libintl.h`.

## Running the Audit

These are kept apart from building and testing: none of them runs in `cargo build` or
`cargo test`. In CI the dependency checks run on every push, and the rest in the Verify
workflow after each push to main (see below). README's Security Audit section sums up
what they cover.

They run through `cargo xtask`, a small program in `xtask/` that uses no crate rse
does not already depend on. It pins the tools and the nightly toolchain that fuzzing,
Miri and coverage need (Kani brings its own), so every machine and CI use the same
versions, and runs each heavy check in a systemd scope with a
memory limit (`RUST_XTASK_MEMORY`, 4G unless set; each Kani proof in a scope of its own),
one at a time: cbmc, Kani's solver, can take tens of gigabytes. The fuzz targets run
together in one scope, at most `RUST_XTASK_JOBS` at once (all unless set), limited to
`RUST_XTASK_FUZZ_MEMORY` (unless set, 1280M for each target running at once). Where
systemd gives no scope with a working memory limit, the heavy checks refuse to run;
`RUST_XTASK_NO_SCOPE=1` runs them without one, as CI does on machines of its own. With
`RUST_XTASK_PARALLEL=1` a task may start beside another when their limits together
leave 4G of the machine's memory, except Kani beside fuzzing.

The results are committed with the code, in `audit/<task>/`: the latest run of each task,
what it printed, when and on which commit it ran, how it ended (`run.txt`), the files it
read with a hash of each (`inputs.txt`), and its reports; the paths of the machine it ran
on are taken out. When the latest run failed, the last that succeeded is in
`audit/<task>/last_succeeded/`. A run needs a working tree without uncommitted changes,
since its result stands for a commit; `--no-record` runs on any tree, into
`target/audit/<task>/`, recording nothing; it is also needed to run some of the proofs
(`kani <proof>...`) or to fuzz for other than an hour, since such a run cannot stand for
the task. Each task ends with a line `xtask: <task>: succeeded`, `failed: ...`,
`skipped: ...` or `refused: ...` (a check before the run did not pass, and nothing ran or
was recorded). A result stands while the task succeeded with
the same tools and arguments on the same files it reads (documentation changes none, nor
do comments and layout: Rust files are compared as their tokens, doc comments included,
and TOML files as their values), and for `deny`, whose advisories are published daily, for
a day. `cargo xtask status` shows
which results stand, and `--if-needed` skips a task whose result does. After each push to
main the Verify workflow runs the tasks whose results do not stand, a newer push cancelling
it, and commits their results to `audit/` (failures too, beside the last success).

```bash
cargo xtask setup        # install the pinned tools and toolchain (once)
cargo xtask deny         # sources, licenses and advisories of the dependencies
cargo xtask vet          # every dependency is audited
cargo xtask geiger       # unsafe code in the dependencies
cargo xtask miri         # undefined behaviour in the tests of the pure modules
cargo xtask coverage     # which branches the tests reach
cargo xtask kani         # the proofs (Kani brings its own toolchain)
cargo xtask fuzz         # every fuzz target, an hour each
cargo xtask all          # the seven above in this order, each whatever the others did
cargo xtask status       # how each task last ran, and whether its result still stands
cargo xtask needed kani  # why kani needs a run; exits 1 when it does not
cargo xtask outdated     # pinned versions that are due for an update; asks which to take
```

The pinned versions change only on purpose. A tool is due for an update once a release
(not a pre-release, not yanked) is at least a month newer than the pinned one, so that
tools released every day do not ask every day; at once if the pinned version has a
known vulnerability (from OSV, which carries the RustSec advisories). The version
suggested is the newest that is a week old, not yanked, plain `x.y.z` and without a known
vulnerability; while there is none, a newer release is only mentioned, but a vulnerable
pinned version stays due (and `outdated` exits 2). The nightly
toolchain is due once a month old, and the one suggested is a week old, since a
nightly's regressions are mostly fixed within days. `cargo xtask outdated` asks about
each; the ones you take are written into `xtask/src/pins.rs` and installed (a nightly
lacking a component the tasks need gives way to the latest of the week before it that
has them all). Other tasks look by
themselves and say so in one line: for vulnerabilities daily, for releases weekly
(`RUST_XTASK_VULNS_EVERY` and `RUST_XTASK_RELEASES_EVERY`, in days; 0 looks every
run). In CI, the Advisories workflow fails daily on a vulnerability in a pinned
tool, and weekly while any update is due.

Each Kani proof runs alone in its own scope; its time and peak memory (with the crate
already built) are in `audit/kani/harnesses.txt`, and README gives those of the run it
names.

## Decisions

- **`unsafe`.** The crate denies `unsafe_code`; the 8 uses left are calls with no safe
  form, into librclone, `setlocale` and `pre_exec`, each in a function that allows it
  by name, with the reason it is sound beside it. The `extern` blocks declaring the C
  functions allow it by name too, and `build.rs` checks them against the C headers. File
  descriptors, `getuid` and `setsid` go through rustix's safe functions instead.
- **ssh questions.** A password or passphrase from rclone.conf, or a remembered one, goes
  only to ssh's own question for the server's account (`me@box's password:` or
  `(me@box) …`, `me@box` as `ssh -G` resolves it) or for the remote's `key_file`. A
  question for a jump host, or a server asking for a passphrase, is put to the user, and
  what they type is not remembered for the server.
- **Falling back to rclone's built-in ssh** after the system ssh fails, or for a remote
  with options only the built-in ssh honours, keeps checking the host key against ssh's
  `known_hosts` file. The built-in ssh checks none by itself, so otherwise blocking ssh
  would get an attacker past the check. A `UserKnownHostsFile none` in the ssh config
  becomes ssh's global file, or an empty one, since rclone reads `none` as no checking.
  A remote that pins its host keys is left to rclone, which ignores the pins once it
  runs ssh. With `StrictHostKeyChecking no` or `accept-new` the fallback lets in a new
  server, as OpenSSH does, and checks one listed in any file ssh reads under any name it
  knows the server by (`host`, `HostName`, `HostKeyAlias`, with the port, hashed or
  not). It decides when it falls back, since `accept-new` adds servers meanwhile, and
  checks whenever it cannot tell. A changed key is refused, stricter than OpenSSH with
  `no`. An sftp remote under a `crypt`, `alias` or `union` remote is opened by
  rclone itself, as rclone always does, checking host keys only if that remote says so.
- **What a server can ask for.** Connections for transfers forward no agent, X11 or
  ports and run no local or remote command, whatever `~/.ssh/config` says, as OpenSSH's
  own sftp does; remotes with different settings for one account never share one. A
  cancelled question ends the login, so a server cannot keep asking, and so do two
  minutes without progress, not counting the user's answers, so a server that stalls
  cannot hold a remote and a worker. Ending a login ends what ssh started with it, such
  as a `ProxyCommand`.
- **New host keys.** rse never tells ssh to accept a new host key by itself: ssh's own
  question, with the key's fingerprint, is shown in a dialog whose default is No.
- **ssh arguments.** A `host` or `user` starting with `-`, or any ssh setting holding a
  control character, is never passed to the system ssh.
- **`--rcd`** refuses plain `http://` to anything but this machine, and requests to this
  machine never go through a proxy from `HTTP_PROXY` and the like, which would carry them
  on in clear; no redirect is followed. The password can come from `RCLONE_RC_PASS`,
  which other users cannot read in `ps`. Plain http to this machine still trusts every
  local user: one who takes the port before rclone does gets the password, so use
  https, or a port only rclone can have, on a shared machine.
- **Obscured passwords** are read only from text exactly as `rclone obscure` writes it.
  rclone itself also reads text whose last base64 bits are not zero, or with line breaks in
  it; rse refuses such a value, which rclone never writes. From rclone.conf, it asks for
  the password instead and tells the user why.
- **Files.** The rclone log is created readable only by its owner (`0600`), and obscured
  passwords are left out of what rse writes to it. The ssh socket directory is `0700`
  and must belong to the user; the password file is `0600` and refused otherwise.
- **Socket directory.** It is `$XDG_RUNTIME_DIR/rse` where the system has one, since no
  other user can create anything there; `/tmp/rse-<uid>` could be created first by
  anyone. If it cannot be used, sftp falls back to rclone's built-in ssh still checking
  host keys, as when the system ssh fails, and rse shows why on startup.
- **Names from servers.** An entry listed as `.`, `..`, or with `/` or a NUL in its name
  is left out, with a message, since joined to the directory it would lead elsewhere.
  Control characters and the formatting characters with no glyph (bidirectional marks,
  zero-width spaces) are shown as `�` in names, paths, messages, ssh's questions and
  what rse prints after the interface closes, so a name cannot pass for another one
  that way, nor reach the terminal. The joiners U+200C and U+200D stay, for emoji and
  the scripts written with them, and so do look-alike letters from other scripts. The
  Go to field shows the address as it is typed.
- **The rclone log** is created in a directory of the user's own, never through a link,
  and only if it is a file of theirs. Without a log, rclone's messages go to /dev/null
  rather than onto the screen, where text from a server would arrive unmarked.
- **CI** runs with a read-only token; only the release job may write, and it downloads
  the dependencies itself instead of taking them from a cache other jobs fill. Actions
  are still named by tag, not pinned by commit. The security audit runs in its own
  Verify workflow after each push to main, only the tasks whose committed results no longer
  stand; its last job, which runs none of the code, is the other that may write: it
  commits the results to `audit/` and nothing else.
- **Resumed transfers** (`rse/copy`). A file is written under its name plus
  `.rse-partial` and renamed over the target only once complete, of the source's size,
  and with the same hash as the source wherever both sides have one in common (on sftp,
  `md5sum` or `sha1sum` run by rclone); a mismatch copies it again from the start once,
  then fails. A job that leaves a partial file behind gives it the source's
  modification time; it is resumed only if the source still has that time (to the
  target's precision), it is no larger than the source, and its last 64 KiB (before the
  point resumed from) are the source's. So a source changed since, even in the middle
  at the same size, or a partial file left by a crash, with the time of its last write,
  is copied again from the start. Without a hash in common, a resumed file cannot be
  checked as a whole, and rclone's log says so. On sftp a partial file is resumed from
  2 MiB before its end, where concurrent writes cut short may have left a hole. A
  partial file that is not a regular file, such as a link to some other file, is never
  written through: the copy fails (locally it is opened with `O_NOFOLLOW`; on sftp it
  is checked with `lstat` first). The sftp sessions for this run the remote's own `ssh`
  command with the sftp subsystem, as rclone does, so they are bound by the same ssh
  settings as rclone's.
- **Conflicts.** Before a copy or move the target is listed again, even when a pane shows
  it, so a file that appeared there since the pane listed it is not overwritten without
  asking. The check is made when the operation is queued and again as each job starts
  or is retried, so a file that arrives while a job waits fails the job, as an error,
  and the user is asked; only what the user chose to overwrite starts unchecked. A
  listing cannot see a transfer in flight, so a job also waits while another that
  writes, moves or deletes an entry of its name in the same directory is under way or
  queued before it: two jobs never write one file at once, and the later one is checked
  after the earlier has ended. A rename lists its directory first, so its new name does
  not replace a file the pane did not show. What is left is a file that something else
  writes between the last listing and rclone's call, jobs whose entries nest (a
  directory, and a file copied into it), and the files inside a directory merged into
  one of its name. Overwrite mode, which the user turns on (it is off at every start)
  and a red row across the top of the screen shows, leaves the checks out but not the
  waiting.
- **Marks**, the cursor, drags and renames know an entry by its name and whether it is a
  directory, so choosing a file never acts on a directory of the same name next to it,
  as S3 allows.