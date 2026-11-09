# Review of rse's unsafe code

- file: src/i18n.rs sha256:347b4284c8b5a3170b3fb3b5645d147926c12ed88db1aee02e68dba5c07b5afe
- file: src/main.rs sha256:2fda601dff3152539654a32af79b271f3e55da4f79bfea8e09bd69f6ea55ef51
- file: src/rclone/ffi.rs sha256:58cf82b0a0efdef67f836fd0b4d7a78e16fe7249603b1e0a09544a1bd568275e
- file: src/rclone/system_ssh.rs sha256:999cf0696f65954c1238cf547e7f6e776494097db634f3c946ccc66313cda10d
- file: src/signals.rs sha256:9bff584e80f7c7d9b8888f3cf5aec991572f87605380cb465b1f9b04d2fcb301
- file: src/terminal.rs sha256:c233d331d9dd99d5a0176148e3aef57d6ecb6ed288dded1caf92801bc80b0d0b
- reviewer: eight independent reviewers, each given the code alone, without the findings of the others: three of the code as it was, then one of it changed and one of it as committed; later two of the `SIGWINCH` handler installed before Go's runtime, as written and as committed, and one of the check of the Rust declarations in `build.rs`
- date: 2026-11-09
- commit: 4eac88a

rse's `unsafe` code is calls with no safe form: into librclone (`src/rclone/ffi.rs`), `setlocale` and glibc's
`bind_textdomain_codeset` (`src/i18n.rs`), `sigaction` and signal-hook-registry's `register` (`src/signals.rs`), and
two `CommandExt::pre_exec` closures (`src/rclone/system_ssh.rs`, `src/terminal.rs`); `src/main.rs` calls the one
`unsafe fn`, and places a constructor with `#[unsafe(link_section)]`. Each reviewer checked every block and
`// SAFETY:` comment against the sources: librse/main.go and rclone v1.75.2's librclone, the Go 1.27.3 runtime (cgo
calls, package initialisation, signal handling), glibc 2.39's headers and manual pages, POSIX, std's process code
and the vendored rustix 1.1.5, crossterm 0.29.0 and signal-hook-registry 1.4.8; and the build's checks of the
hand-written declarations. They also traced the program under strace and gdb, and the integration tests, which all
call into librclone, ran under AddressSanitizer without an error.

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
  that `ffi.rs` and `i18n.rs` still declare what it keeps beside the C declarations, `#[repr(C)]` included;
  `improper_ctypes` is denied, and so is an `unsafe fn` without a `# Safety` section (fifth review). It first
  matched them as text with `//` comments left out, which a copy in a `/* */` comment, a string or disabled code
  satisfied; it now reads the files as tokens, and each must have exactly the `extern` blocks, of any ABI, and the
  structs it lists, with the attributes before them. The seventh review tried 29 changes against it: those that
  alter a declaration fail the build now (another ABI, a block moved out of reach or disabled, a field's type,
  `repr(packed)`), but for the two kinds in Open.
- **crossterm's handler lacked `SA_ONSTACK` for a few microseconds at start-up** (an open item of the first
  reviews): a constructor ordered before Go's (`.init_array.00200` on Linux; `__mod_init_func` in object order on
  macOS) now has signal-hook-registry install the `SIGWINCH` handler with the flag, before any thread exists;
  crossterm's later registration reuses it, as the registry installs a signal's handler once. Seen in the binary's
  `.init_array` and under strace; a test checks the order on both systems in CI, by `SIGFPE` still having no
  handler when the constructor runs, which Go installs in its own (sixth and eighth reviews; the eighth found the
  first such test proved nothing, as Rust's std installs one for `SIGSEGV`).
- **SAFETY comments that gave wrong or no reasons**: the owner of rclone's output (the caller, freed with `C.free`),
  why the `pre_exec` closures are async-signal-safe (raw system calls through rustix on Linux; nothing allocated, the
  errors included), and that rclone's first config access calls C's `setenv`, which is why it comes before rse starts
  threads.
- **A musl cross build defaulted to a glibc compiler for cgo**: it uses `<arch>-linux-musl-gcc` now.

## Open

- `build.rs` reads the Rust declarations as tokens, not as the compiler resolves them: a `use … as` or a module
  that renames a type (`c_long as c_int`), or a macro that writes or rewrites a declaration, gets past it.
- On macOS a shell pane's pseudo-terminal is opened without close-on-exec, which is set right after; an ssh or rsync
  started by another thread in between inherits it. Linux sets the flag when it opens.
- The constructor's order on macOS is untested here; CI's test on macOS checks it.
- The `SIGWINCH` handler is now there in every mode, not only the interface: a window change can interrupt, with
  `EINTR`, a call `SA_RESTART` does not restart (poll, sleep, socket timeouts) in askpass mode or a prompt; the
  askpass path was checked and is unaffected.
- An embedded build for musl is untested: Go's library start-up reads argc and argv from the constructor's
  registers, which musl may not pass. rse is not built for musl, by decision.
- `terminal.rs` gives no source for `ioctl(TIOCSCTTY)` being safe after fork on macOS, where it goes through libc;
  BSD's `login_tty` does the same.
- `LANGUAGE`, when set, still decides glibc's message language over the one rse sets, as for any program.
