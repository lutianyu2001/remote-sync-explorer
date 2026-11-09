//! Signal handlers that live with the embedded rclone's Go runtime.
//!
//! Go requires every handler other code installs to run on the alternate signal stack
//! (`SA_ONSTACK`): a signal that reaches one of Go's threads otherwise runs the handler on
//! that thread's small goroutine stack, which it may overrun ("Go programs that use cgo or
//! SWIG" in the documentation of Go's os/signal). crossterm installs its handler for
//! `SIGWINCH` through signal-hook-registry, without the flag. So rse has the registry
//! install that handler first, with the flag added, before Go's runtime starts and so
//! before any thread can take the signal ([`install_on_alternate_stack`], from a
//! constructor in `main.rs`): the registry installs a signal's handler at its first
//! registration only, and crossterm's later one reuses it. [`on_alternate_stack`] checks it
//! once crossterm has registered, and adds the flag where the constructor failed or does not
//! run (on a system other than Linux and macOS).
//! Go's own `signal.Notify` for `SIGWINCH` (rclone's interactive prompts, which rse never
//! shows) would replace the handler.

use std::io;
use std::mem::MaybeUninit;

/// The action `signal` has now.
#[allow(unsafe_code)]
fn current(signal: libc::c_int) -> io::Result<libc::sigaction> {
    let mut current = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: with a null new action, sigaction only writes the current one to `current`,
    // which is valid for writes and as large as a `struct sigaction` (POSIX sigaction()).
    if unsafe { libc::sigaction(signal, std::ptr::null(), current.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigaction succeeded, so it filled in `current`, which was also zeroed before:
    // every field is initialised.
    Ok(unsafe { current.assume_init() })
}

/// Whether `signal` has its default action, no handler.
///
/// # Errors
///
/// What sigaction says when it cannot read the action, such as for an invalid signal number.
pub fn is_default(signal: libc::c_int) -> io::Result<bool> {
    Ok(current(signal)?.sa_sigaction == libc::SIG_DFL)
}

/// Has the handler installed for `signal` run on the alternate signal stack. False when
/// there is none to change (the default action, or ignored).
///
/// # Errors
///
/// What sigaction says when it cannot read or set the handler, such as for an invalid
/// signal number.
#[allow(unsafe_code)]
pub fn on_alternate_stack(signal: libc::c_int) -> io::Result<bool> {
    let mut action = current(signal)?;
    if action.sa_sigaction == libc::SIG_DFL || action.sa_sigaction == libc::SIG_IGN {
        return Ok(false);
    }
    if action.sa_flags & libc::SA_ONSTACK != 0 {
        return Ok(true);
    }
    action.sa_flags |= libc::SA_ONSTACK;
    // SAFETY: the handler, mask and flags the signal has now, with SA_ONSTACK besides, so
    // the handler is called as before but on the thread's alternate stack, where it has
    // one: 32 KiB on Go's threads, a Rust thread's with a guard page. A handler needing more
    // would overrun it, as it would a goroutine's stack without the flag; rse's, the
    // registry's, takes far less (atomics and a write to a pipe). A null old action is
    // allowed (POSIX sigaction()). What another thread set in between would be replaced:
    // rse calls this before it starts one, or for a signal no other code sets.
    if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

/// Has signal-hook-registry install its handler for `signal` now, if it has none yet, and
/// run it on the alternate signal stack. The action registered for it does nothing, and
/// stays: the handler, and so the flag, stay for what registers for `signal` later.
///
/// Only for a signal whose default action is to ignore it, such as `SIGWINCH`: the
/// registry's handler never takes the default action, so any other signal would be
/// swallowed from now on.
///
/// # Errors
///
/// What sigaction says when it cannot read or set the handler.
///
/// # Panics
///
/// For a signal the registry refuses (`signal_hook_registry::FORBIDDEN`: `SIGKILL`,
/// `SIGSTOP`, `SIGILL`, `SIGFPE`, `SIGSEGV`).
#[allow(unsafe_code)]
pub fn install_on_alternate_stack(signal: libc::c_int) -> io::Result<()> {
    // SAFETY: an action that does nothing is async-signal-safe and cannot panic, as
    // signal_hook_registry::register's # Safety asks.
    unsafe { signal_hook_registry::register(signal, || {}) }?;
    if on_alternate_stack(signal)? {
        Ok(())
    } else {
        Err(io::Error::other("signal-hook-registry installed no handler"))
    }
}

#[cfg(test)]
mod tests {
    use super::{install_on_alternate_stack, on_alternate_stack};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Reads, or with `new` also sets, the action for `signal`.
    #[allow(unsafe_code)]
    fn action(signal: libc::c_int, new: Option<&libc::sigaction>) -> libc::sigaction {
        // SAFETY: an all-zero sigaction is a valid value: no handler, empty mask, no flags.
        let mut old = unsafe { std::mem::zeroed::<libc::sigaction>() };
        let new = new.map_or(std::ptr::null(), std::ptr::from_ref);
        // SAFETY: `new` is null or a valid action, and `old` is valid for writes.
        assert_eq!(unsafe { libc::sigaction(signal, new, &raw mut old) }, 0);
        old
    }

    static CALLED: AtomicBool = AtomicBool::new(false);

    /// The flag is added to an installed handler, as crossterm's is (`SA_SIGINFO`,
    /// `SA_RESTART`), which keeps its flags and mask and is still called. `SIGUSR1`, which
    /// neither rse, rclone nor Go's runtime uses, and which is put back afterwards.
    #[test]
    #[allow(unsafe_code)]
    fn installed_handlers_get_the_alternate_stack() {
        extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
            CALLED.store(true, Ordering::SeqCst);
        }
        let mut ours = action(libc::SIGUSR1, None);
        let before = ours;
        ours.sa_sigaction = handler as *const () as libc::sighandler_t;
        ours.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        // SAFETY: the mask is a valid sigset_t, from sigaction.
        assert_eq!(unsafe { libc::sigaddset(&raw mut ours.sa_mask, libc::SIGUSR2) }, 0);
        action(libc::SIGUSR1, Some(&ours));
        assert!(on_alternate_stack(libc::SIGUSR1).is_ok_and(|found| found));
        let now = action(libc::SIGUSR1, None);
        // SAFETY: raises a signal whose handler only stores to an atomic.
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
        action(libc::SIGUSR1, Some(&before));
        assert!(CALLED.load(Ordering::SeqCst));
        // All of the flags kept (glibc adds `SA_RESTORER` of its own).
        assert_eq!(now.sa_flags & ours.sa_flags, ours.sa_flags);
        assert_ne!(now.sa_flags & libc::SA_ONSTACK, 0);
        assert_eq!(now.sa_sigaction, ours.sa_sigaction);
        // SAFETY: the mask is a valid sigset_t, from sigaction.
        assert_eq!(unsafe { libc::sigismember(&raw const now.sa_mask, libc::SIGUSR2) }, 1);
        assert!(on_alternate_stack(-1).is_err());
    }

    static LATER: AtomicBool = AtomicBool::new(false);

    /// The handler installed early keeps the flag when something registers for the signal
    /// later, as crossterm does, and calls what it registered. `SIGUSR2`, which rse does not
    /// use, and rclone only once initialised, which no test of the library is; its action
    /// before is put back afterwards: the registry, which still has it as installed, would
    /// install no handler for it again in this process, which no other test asks of it.
    #[test]
    #[allow(unsafe_code)]
    fn a_later_registration_keeps_the_alternate_stack() {
        let before = action(libc::SIGUSR2, None);
        install_on_alternate_stack(libc::SIGUSR2).unwrap_or_else(|e| panic!("{e}"));
        // SAFETY: the action only stores to an atomic.
        let later = unsafe {
            signal_hook_registry::register(libc::SIGUSR2, || LATER.store(true, Ordering::SeqCst))
        };
        assert!(later.is_ok());
        let now = action(libc::SIGUSR2, None);
        // SAFETY: raises a signal whose handler only calls the actions above.
        assert_eq!(unsafe { libc::raise(libc::SIGUSR2) }, 0);
        action(libc::SIGUSR2, Some(&before));
        assert!(LATER.load(Ordering::SeqCst));
        assert_ne!(now.sa_flags & libc::SA_ONSTACK, 0);
    }
}
