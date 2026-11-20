//! Copies and moves with rsync: [`crate::librse::rsync`], with rse's own places, progress and
//! words.

use std::path::PathBuf;

pub use crate::librse::rsync::*;

use crate::hostops::Failure;
use crate::location::Location;
use crate::rclone::api;

/// A local [`Location`] as one side of a transfer.
pub fn local(loc: &Location) -> Side {
    Side::Local(PathBuf::from(loc.fs_string()))
}

/// rsync's progress as a job reports its own.
pub fn stats(stats: Stats) -> api::Stats {
    let transferring = stats.transferring.into_iter().map(|t| api::Transferring {
        name: t.name,
        bytes: t.bytes,
        size: t.size,
    });
    api::Stats {
        bytes: stats.bytes,
        total_bytes: stats.total_bytes,
        speed: stats.speed,
        eta: stats.eta,
        transfers: stats.transfers,
        errors: stats.errors,
        last_error: stats.last_error,
        transferring: transferring.collect(),
        ..api::Stats::default()
    }
}

/// Why rsync did not succeed, as rse says it.
pub fn said(failure: Failure) -> String {
    match failure {
        Failure::Stopped => tr!("rsync-stopped"),
        Failure::Said(said) => said,
        Failure::Exit(code) => tr!("rsync-exit", code = code.to_string()),
        Failure::Signal(signal) => tr!("rsync-signal", signal = signal.to_string()),
        Failure::Internal => tr!("internal-lock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server's path is joined to the fs root as rclone joins a place's.
    #[test]
    fn the_fs_root_is_joined_as_rclone_joins_it() {
        for (root, path) in [("", "docs"), ("/srv", "a/b"), ("rel/", "x")] {
            let ssh = ["ssh".to_owned(), "me@box".to_owned()];
            let remote = RsyncRemote::new(&ssh, root).unwrap();
            let loc = Location::new(format!("box:{root}"), path);
            let server = loc.fs_string().strip_prefix("box:").unwrap().to_owned();
            assert_eq!(remote.path(path), server);
        }
    }
}
