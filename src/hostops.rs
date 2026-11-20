//! Deletes, moves and copies inside one machine by its own commands: [`crate::librse::command`],
//! with what a command that failed says in rse's words.

pub use crate::librse::Failure;
pub use crate::librse::command::*;

/// Why a command of the machine's own did not succeed, as rse says it.
pub fn said(failure: Failure) -> String {
    match failure {
        Failure::Stopped => tr!("host-stopped"),
        Failure::Said(said) => said,
        Failure::Exit(code) => tr!("host-exit", code = code.to_string()),
        Failure::Signal(signal) => tr!("host-signal", signal = signal.to_string()),
        Failure::Internal => tr!("internal-lock"),
    }
}
