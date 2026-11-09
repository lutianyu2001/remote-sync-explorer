# Review of rse's unsafe code

- file: src/i18n.rs sha256:347b4284c8b5a3170b3fb3b5645d147926c12ed88db1aee02e68dba5c07b5afe
- file: src/main.rs sha256:18d5af61ed50ffefb7a6398ff0c45902e37bb273fd12d50617a76ed0ca0e4cff
- file: src/rclone/ffi.rs sha256:58cf82b0a0efdef67f836fd0b4d7a78e16fe7249603b1e0a09544a1bd568275e
- file: src/rclone/system_ssh.rs sha256:999cf0696f65954c1238cf547e7f6e776494097db634f3c946ccc66313cda10d
- file: src/signals.rs sha256:7975e2c81eeaeac24aa4651c62eac1fbdff4ec415773c46211cf2a3c28aabc92
- file: src/terminal.rs sha256:c233d331d9dd99d5a0176148e3aef57d6ecb6ed288dded1caf92801bc80b0d0b
- reviewer: five independent reviewers, each given the code alone, without the findings of the others: three of the code as it was, then one of it changed and one of it as committed
- date: 2026-11-09
- commit: 8a14836

rse's `unsafe` code is calls with no safe form: into librclone (`src/rclone/ffi.rs`), `setlocale` and glibc's
`bind_textdomain_codeset` (`src/i18n.rs`), `sigaction` (`src/signals.rs`), and two `CommandExt::pre_exec` closures
(`src/rclone/system_ssh.rs`, `src/terminal.rs`); `src/main.rs` calls the one `unsafe fn`. Each reviewer checked every
block and `// SAFETY:` comment against the sources: librse/main.go and rclone v1.75.2's librclone, the Go 1.27.3
runtime (cgo calls, package initialisation, signal handling), glibc 2.39's headers and manual pages, POSIX, std's
process code and the vendored rustix 1.1.5, crossterm 0.29.0 and signal-hook-registry 1.4.8; and the build's checks
of the hand-written declarations. They also traced the program under strace and gdb, and the integration tests,
which all call into librclone, ran under AddressSanitizer without an error.

## Verdict

Sound: no undefined behaviour reachable in the code as committed, and no false safety claim, but for the open items
below.

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
  that `ffi.rs` and `i18n.rs` still declare, outside comments, what it keeps beside the C declarations, `#[repr(C)]`
  included; `improper_ctypes` is denied, and so is an `unsafe fn` without a `# Safety` section (fifth review).
- **SAFETY comments that gave wrong or no reasons**: the owner of rclone's output (the caller, freed with `C.free`),
  why the `pre_exec` closures are async-signal-safe (raw system calls through rustix on Linux; nothing allocated, the
  errors included), and that rclone's first config access calls C's `setenv`, which is why it comes before rse starts
  threads.
- **A musl cross build defaulted to a glibc compiler for cgo**: it uses `<arch>-linux-musl-gcc` now.

## Open

- Between crossterm installing its SIGWINCH handler and `on_alternate_stack` a few microseconds pass at start-up, in
  which a window change reaching a Go thread would run the handler on the goroutine stack. Closing it needs
  signal-hook-registry to set `SA_ONSTACK` itself (a patch of the vendored crate, or upstream).
- `build.rs` matches the Rust declarations as text with `//` comments left out: a copy in a `/* */` comment, a string
  or disabled code would also match. Comparing tokens, or the items of the `extern` blocks, would not.
- On macOS a shell pane's pseudo-terminal is opened without close-on-exec, which is set right after; an ssh or rsync
  started by another thread in between inherits it. Linux sets the flag when it opens.
- An embedded build for musl is untested: Go's library start-up reads argc and argv from the constructor's
  registers, which musl may not pass. Nothing is released for musl.
- `terminal.rs` gives no source for `ioctl(TIOCSCTTY)` being safe after fork on macOS, where it goes through libc;
  BSD's `login_tty` does the same.
- `LANGUAGE`, when set, still decides glibc's message language over the one rse sets, as for any program.
