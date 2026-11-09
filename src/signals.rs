//! Signal handlers that live with the embedded rclone's Go runtime.
//!
//! Go requires every handler other code installs to run on the alternate signal stack
//! (`SA_ONSTACK`): a signal that reaches one of Go's threads otherwise runs the handler on
//! that thread's small goroutine stack, which it may overrun ("Go programs that use cgo or
//! SWIG" in the documentation of Go's os/signal). Go adds the flag itself only to the
//! handlers that exist when it starts; crossterm installs its own for `SIGWINCH` later,
//! without it. Between that and [`on_alternate_stack`] a few microseconds pass, in which a
//! window change reaching a Go thread would still find the handler on its own stack; only
//! signal-hook-registry setting the flag itself would close that. Go's own `signal.Notify`
//! for `SIGWINCH` (rclone's interactive prompts, which rse never shows) would replace the
//! handler.

use std::io;
use std::mem::MaybeUninit;

/// Has the handler installed for `signal` run on the alternate signal stack. False when
/// there is none to change (the default action, or ignored).
///
/// # Errors
///
/// What sigaction says when it cannot read or set the handler, such as for an invalid
/// signal number.
#[allow(unsafe_code)]
pub fn on_alternate_stack(signal: libc::c_int) -> io::Result<bool> {
    let mut current = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: with a null new action, sigaction only writes the current one to `current`,
    // which is valid for writes and as large as a `struct sigaction` (POSIX sigaction()).
    if unsafe { libc::sigaction(signal, std::ptr::null(), current.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigaction succeeded, so it filled in `current`, which was also zeroed before:
    // every field is initialised.
    let mut action = unsafe { current.assume_init() };
    if action.sa_sigaction == libc::SIG_DFL || action.sa_sigaction == libc::SIG_IGN {
        return Ok(false);
    }
    if action.sa_flags & libc::SA_ONSTACK != 0 {
        return Ok(true);
    }
    action.sa_flags |= libc::SA_ONSTACK;
    // SAFETY: the handler, mask and flags the signal has now, with SA_ONSTACK besides, so
    // the handler is called as before but on the alternate stack; a null old action is
    // allowed (POSIX sigaction()).
    if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::on_alternate_stack;
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
}
