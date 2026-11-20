//! Files on a machine reached through the system ssh, handled by the machine itself: its
//! own `rm`, `mv`, `cp` and rsync, started so that any name passes whatever the login
//! shell, and so that stopping here stops the work there.
//!
//! A library within rse, kept so that it could leave: nothing here knows of rclone, of
//! rse's places or of its messages, and nothing here uses another module of rse.
//!
//! - [`agent`]: a private ssh-agent, for an inline key the system ssh is to log in with.
//! - [`shell`]: a machine as a shell reaches it, and its directories.
//! - [`command`]: the command lines, and one at work.
//! - [`rsync`]: rsync's command line, and rsync at work with its progress read.
//! - [`sftp`]: removing a tree over sftp alone, many requests under way at once, for a
//!   server that runs no command.

use std::fmt;

pub mod agent;
pub mod command;
pub mod rsync;
pub mod sftp;
pub mod shell;

/// How a command that did not succeed ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Stopped from here before it was done.
    Stopped,
    /// What it wrote to its error output.
    Said(String),
    /// Its exit code, when it said nothing.
    Exit(i32),
    /// The signal that ended it, when it said nothing.
    Signal(i32),
    /// Its output could not be read here.
    Internal,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("stopped before it was done"),
            Self::Said(said) => f.write_str(said),
            Self::Exit(code) => write!(f, "failed with exit code {code}"),
            Self::Signal(signal) => write!(f, "ended by signal {signal}"),
            Self::Internal => f.write_str("its output could not be read"),
        }
    }
}

impl std::error::Error for Failure {}
