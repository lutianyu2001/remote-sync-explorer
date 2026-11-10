# Review of rse's unsafe code

- file: src/i18n.rs sha256:5cc60b8a91bead370f4a5b6122db7f451301e679f7f9a6031ced794e04b02daf
- file: src/main.rs sha256:d8472fffd9c78c026796f53be6bb34cc9e6240b469a74b8c445938d05a306476
- file: src/rclone/ffi.rs sha256:b701e4ddee91f173439cc79cadd9ef79c0f775401d52551c86f2059838bc6d9a
- file: src/rclone/system_ssh.rs sha256:999cf0696f65954c1238cf547e7f6e776494097db634f3c946ccc66313cda10d
- file: src/signals.rs sha256:e5614d52a244e3a5d8afc7111ba5268da002566d414211731bfac0c041bb8809
- file: src/terminal.rs sha256:d7f7d8ae7cb1df69142e73758ae5fb5dd5896074c29cd3a3a2bb83bf32dbb658
- reviewer: ten independent reviewers, each given the code alone, without the findings of the others: three of the code as it was, then one of it changed and one of it as committed; later two of the `SIGWINCH` handler installed before Go's runtime, as written and as committed, and one of the check of the Rust declarations in `build.rs`; last one of the compiler's check of those declarations, and one of the pseudo-terminal opened close-on-exec on macOS and of the `SIGWINCH` handler in every mode
- date: 2026-11-09
- commit: d54b9c0

rse's `unsafe` code is calls with no safe form: into librclone (`src/rclone/ffi.rs`), `setlocale` and glibc's
`bind_textdomain_codeset` (`src/i18n.rs`), `sigaction` and signal-hook-registry's `register` (`src/signals.rs`), and
two `CommandExt::pre_exec` closures (`src/rclone/system_ssh.rs`, `src/terminal.rs`); `src/main.rs` calls the one
`unsafe fn`, and places a constructor with `#[unsafe(link_section)]`. Each reviewer checked every block and
`// SAFETY:` comment against the sources: librse/main.go and rclone v1.75.2's librclone, the Go 1.27.3 runtime (cgo
calls, package initialisation, signal handling), glibc 2.39's headers and manual pages, POSIX, std's process code
and the vendored rustix 1.1.5, crossterm 0.29.0, signal-hook-registry 1.4.8, rpassword 7.5.4 and ureq 3.5.0, and
for macOS Apple's Libc-1752.120.2 and xnu-12377.121.6; and the build's checks of the hand-written declarations.
They also traced the program under strace and gdb, and the integration tests, which all call into librclone, ran
under AddressSanitizer without an error.

## Verdict

Sound: no undefined behaviour reachable in the code as committed, and no false safety claim, within the known
limits below.

## Findings, and what was done

- **A safe public function set the C locale** (all three first reviews): `i18n::set` called `setlocale`, which glibc
  marks MT-Unsafe, with the rule "before starting threads" only in its documentation. The locale is now set by
  `i18n::set_system_messages`, an `unsafe fn` whose `# Safety` says so, called from one helper in `main.rs` before rse
  starts a thread.
- **Go's threads were said never to read the locale** (all three first reviews, seen under strace): in a c-archive,
  Go initialises its packages on a thread of its own beside `main`, and rclone's sftp, ftp and smb backends look up
  the current user then. `set_system_messages` now first calls a Go export that does nothing else
  (`RcloneFreeString(NULL)`), which waits for that initialisation to end (runtime/cgocall.go, `cgocallbackg1`).
- **Shutting rclone down rested on a promise of the callers** (all three first reviews): `ffi::finalize` was safe
  and said "the caller guarantees no further RPCs", which nothing enforced. Calls now hold a lock that shutting down
  takes for writing, without waiting, and are refused after it. In rclone v1.75.2 `RcloneFinalize` only runs Go's
  garbage collector, so nothing was unsound; the comments say so now.
- **crossterm's SIGWINCH handler ran on the thread's own stack** (one first review): Go requires every handler
  installed after it starts to use `SA_ONSTACK`, or one that lands on a Go thread runs on its small goroutine stack.
  `signals::on_alternate_stack` adds the flag to that handler once crossterm has installed it, and rse refuses to
  start if it finds none (fourth review); a test checks that the flags and mask are kept and the handler still runs.
- **ssh's `setsid` error was dropped** (all three first reviews): harmless today, as a fresh child is never a process
  group leader; it is checked now, and the closure is in a function of its own, so the rest of the caller no longer
  allows `unsafe`.
- **The build checked the C declarations, not the Rust ones** (first and fourth reviews): `build.rs` also checks
  that `ffi.rs` and `i18n.rs` still declare what it keeps beside the C declarations, `#[repr(C)]` included;
  `improper_ctypes` is denied, and so is an `unsafe fn` without a `# Safety` section (fifth review). It first
  matched them as text with `//` comments left out, which a copy in a `/* */` comment, a string or disabled code
  satisfied; it now reads the files as tokens, and each must have exactly the `extern` blocks, of any ABI, and the
  structs it lists, with the attributes before them. The seventh review tried 29 changes against it: those that
  alter a declaration failed the build (another ABI, a block moved out of reach or disabled, a field's type,
  `repr(packed)`), but a `use … as` or a module that renames a type, and a macro that writes or rewrites a
  declaration. Each file now also assigns its functions to function pointer types written by absolute paths
  (`::core::ffi::c_char`) and asserts the struct's layout, in a `const _` item `build.rs` requires as well; a second
  declaration of one of the functions with another signature is forbidden (`clashing_extern_declarations`, which no
  `allow` lifts); and `build.rs` fails on a C declaration in any other file of the package, a `#[path]` outside a
  test module, and an `extern crate` at a crate root. The ninth review found the lint only denied, so an `allow`
  let a macro's declaration through, the scan limited to two files, so an integration test could declare
  `RcloneRPC` anew, and that a `#[path]` could swap the file built for the one checked; each fails the build now.
- **crossterm's handler lacked `SA_ONSTACK` for a few microseconds at start-up** (left by the first reviews): a
  constructor ordered before Go's (`.init_array.00200` on Linux; `__mod_init_func` in object order on macOS) now has
  signal-hook-registry install the `SIGWINCH` handler with the flag, before any thread exists; crossterm's later
  registration reuses it, as the registry installs a signal's handler once. Seen in the binary's `.init_array` and under
  strace; a test checks the order on both systems in CI, by `SIGFPE` still having no handler when the constructor runs,
  which Go installs in its own (sixth and eighth reviews; the eighth found the first such test proved nothing, as Rust's
  std installs one for `SIGSEGV`).
- **On macOS the pseudo-terminal was open a moment without close-on-exec** (left by the first reviews):
  an ssh or rsync another thread started then inherited it. macOS's `posix_openpt` is `open("/dev/ptmx", flags)`
  (Libc-1752.120.2, stdlib/grantpt.c), and rustix leaves `O_CLOEXEC` out there; rse makes that call itself, with
  the flag, which xnu sets before the descriptor is visible to other threads (tenth review). The slave side and
  its copies were already close-on-exec (std's `open`, `F_DUPFD_CLOEXEC`).
- **No source for `ioctl(TIOCSCTTY)` after fork on macOS** (left by the first reviews): the comment now cites xnu's
  `ioctl` wrapper, which makes the system call and nothing else, and Apple's `login_tty`, which `forkpty` calls in the
  child.
- **The `SIGWINCH` handler in every mode** (left by the first reviews): the askpass helper cannot get the signal, as ssh
  runs in a session of its own with no controlling terminal; a prompt reads the terminal, which `SA_RESTART` restarts,
  and std and Go retry the calls it does not restart. The tenth review found one exception: on macOS a blocking
  `connect` interrupted by a signal fails, and std's retry then left an unconnected socket, so a window change while rse
  first reached a remote `--rcd` could stop it; it connects with a timeout now, through `poll`, which goes on. It also
  found the `on_alternate_stack` comment wrong in saying Rust's threads have an alternate stack: std sets them up only
  with its own `SIGSEGV` handler, which Go's comes before; corrected.
- **The constructor's order on macOS** (left by the first reviews): CI's macOS run of the test passed (commit a319ca2).
- **SAFETY comments that gave wrong or no reasons**: the owner of rclone's output (the caller, freed with `C.free`),
  why the `pre_exec` closures are async-signal-safe (raw system calls through rustix on Linux; nothing allocated, the
  errors included), and that rclone's first config access calls C's `setenv`, which is why it comes before rse starts
  threads.
- **A musl cross build defaulted to a glibc compiler for cgo**: it uses `<arch>-linux-musl-gcc` now.

## Known limits

What the reviews pointed out and is left as it is, and why; none of it is work still to do.

- `build.rs` checks the files the package's Rust code is in, not what Cargo.toml makes of them: a `[lib] path`
  elsewhere, or a dependency renamed `core`, would get past it. Left so: both take a deliberate change to
  Cargo.toml, which a review of that change would see.
- On systems other than Linux, Android, FreeBSD, NetBSD and macOS the pseudo-terminal would be opened without
  close-on-exec and set so right after. Left so: rse is built for Linux and macOS only.
- An embedded build for musl is untested: Go's library start-up reads argc and argv from the constructor's
  registers, which musl may not pass. Left so: rse is not built for musl, by decision.
- `LANGUAGE`, when set, decides glibc's message language over the one rse sets. Left so: that is how gettext
  works, for every program.
