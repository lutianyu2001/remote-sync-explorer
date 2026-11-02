//! Which program carries out a copy or move: rsync between the local disk and an sftp
//! server reached through the system ssh that has an `rsync` of its own, rclone for
//! everything else (other remotes, two servers, a server rclone's built-in ssh reaches).

use std::ffi::OsString;
use std::sync::Arc;

use crate::config::EngineChoice;
use crate::jobs::{Engine, OpKind, Transfer};
use crate::location::Location;
use crate::rclone::system_ssh::SystemSsh;
use crate::rsync::{self, Op, Side};

/// What the choice of engine depends on.
pub struct Engines {
    /// The system ssh, which knows the servers rsync reaches; `None` leaves rclone alone.
    ssh: Option<Arc<SystemSsh>>,
    choice: EngineChoice,
    /// Tell files apart by checksum, with either engine.
    pub checksum: bool,
}

impl Default for Engines {
    /// rclone alone.
    fn default() -> Self {
        Self { ssh: None, choice: EngineChoice::Rclone, checksum: false }
    }
}

impl Engines {
    pub fn new(ssh: Option<Arc<SystemSsh>>, choice: EngineChoice, checksum: bool) -> Self {
        Self { ssh, choice, checksum }
    }

    /// The engine of copies and moves between `a` and `b`, either way. May open the
    /// master connection to a server, and ask whether it has rsync.
    pub fn between(&self, a: &Location, b: &Location) -> Engine {
        if self.rsync_sides(a, b).is_some() { Engine::Rsync } else { Engine::Rclone }
    }

    /// rsync's command line for `t`, when rsync carries it out.
    pub fn rsync_args(&self, t: &Transfer) -> Option<Vec<OsString>> {
        let op = match t.kind {
            OpKind::Copy => Op::Copy,
            OpKind::Move => Op::Move,
            OpKind::Delete => return None,
        };
        let (src, dst) = self.rsync_sides(&t.src_dir, t.dst_dir.as_ref()?)?;
        Some(rsync::args(op, &src, &dst, &t.name, self.checksum))
    }

    /// The sides rsync sees for a transfer from `src` to `dst`: one the local disk, the
    /// other a server that has rsync.
    fn rsync_sides(&self, src: &Location, dst: &Location) -> Option<(Side, Side)> {
        if self.choice != EngineChoice::Auto {
            return None;
        }
        let ssh = self.ssh.as_ref()?;
        let remote = |loc: &Location| ssh.rsync_remote(&loc.fs).ok().flatten();
        match (src.is_local(), dst.is_local()) {
            (true, false) => Some((Side::local(src), remote(dst)?.side(&dst.path))),
            (false, true) => Some((remote(src)?.side(&src.path), Side::local(dst))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_system_ssh_rclone_does_everything() {
        let e = Engines::new(None, EngineChoice::Auto, false);
        let (local, nas) = (Location::new("/", "home"), Location::new("nas:", "x"));
        assert_eq!(e.between(&local, &nas), Engine::Rclone);
        let t = Transfer {
            kind: OpKind::Copy,
            src_dir: local,
            name: "a".into(),
            is_dir: false,
            dst_dir: Some(nas),
            size: None,
            overwrite: false,
        };
        assert_eq!(e.rsync_args(&t), None);
    }
}
