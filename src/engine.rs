//! Which program carries out a copy or move: rsync between the local disk and an sftp
//! server reached through the system ssh that has an `rsync` of its own, when the local one
//! is rsync 3.1 or newer ([`rsync::usable`]); the machine's own `rm` and `mv` for a delete or
//! a move inside the local disk or one such server ([`hostops`]); for a copy inside such a
//! server, and a move there onto what may be replaced, the server's own rsync, 3.1 or newer
//! too, or its `cp` for a copy to a name not taken when it has none; rclone for everything
//! else (other remotes, two servers, a server rclone's built-in ssh reaches).

use std::ffi::OsString;
use std::sync::Arc;

use crate::config::EngineChoice;
use crate::hostops::{self, Hosts, Op as HostOp};
use crate::jobs::{Engine, OpKind, Transfer};
use crate::location::Location;
use crate::rclone::RpcError;
use crate::rclone::system_ssh::SystemSsh;
use crate::rsync::{self, Op, Side};
use crate::terminal::{ShellDir, ShellTarget};

/// What the choice of engine depends on.
pub struct Engines {
    /// The system ssh, which knows the servers rsync reaches; `None` leaves rclone alone.
    ssh: Option<Arc<SystemSsh>>,
    choice: EngineChoice,
    /// Tell files apart by checksum, with either engine.
    pub checksum: bool,
    /// Whether terminals may run on servers; off, [`Self::shell`] reaches none.
    pub terminal: bool,
    /// The machines known to have sh, for [`Self::host_command`].
    hosts: Hosts,
    /// Tests: terminals run sh here instead of ssh.
    #[cfg(test)]
    pub local_shell: Option<ShellTarget>,
}

impl Default for Engines {
    /// rclone alone.
    fn default() -> Self {
        Self {
            ssh: None,
            choice: EngineChoice::Rclone,
            checksum: false,
            terminal: true,
            hosts: Hosts::default(),
            #[cfg(test)]
            local_shell: None,
        }
    }
}

impl Engines {
    pub fn new(ssh: Option<Arc<SystemSsh>>, choice: EngineChoice, checksum: bool) -> Self {
        Self {
            ssh,
            choice,
            checksum,
            terminal: true,
            hosts: Hosts::default(),
            #[cfg(test)]
            local_shell: None,
        }
    }

    /// How a terminal reaches `loc`: sh on the local disk, ssh on a server reached through
    /// the system ssh; `None` elsewhere, and with terminals turned off. May
    /// open the master connection, asking the user to log in.
    ///
    /// # Errors
    /// Fails as an rc call on `loc` would when the connection cannot be opened.
    pub fn shell(&self, loc: &Location) -> Result<Option<ShellTarget>, RpcError> {
        if !self.terminal {
            return Ok(None);
        }
        #[cfg(test)]
        if let Some(t) = &self.local_shell {
            return Ok(Some(t.clone()));
        }
        if loc.is_local() {
            return Ok(Some(ShellTarget::local(&loc.fs)));
        }
        match &self.ssh {
            Some(ssh) => ssh.shell(&loc.fs),
            None => Ok(None),
        }
    }

    /// The command that carries `t` out on the machine it happens on, and the command that
    /// runs it there: a delete, or a move to a name not taken, on the local disk or on a
    /// server reached through the system ssh that has sh, and on such a server a copy to
    /// a name not taken too, for one without an rsync to do it ([`Self::server_rsync`],
    /// asked first). `None` leaves `t` to the other engines: with rclone chosen for
    /// everything, between two machines, for a copy inside the local disk, or for a move
    /// or copy onto what may be there ([`Transfer::overwrite`]), which is merged.
    /// May open the master connection to the server.
    pub fn host_command(&self, t: &Transfer) -> Option<(Vec<String>, String)> {
        if self.choice != EngineChoice::Auto || t.name.is_empty() {
            return None;
        }
        let target = if t.src_dir.is_local() {
            ShellTarget::local(&t.src_dir.fs)
        } else {
            self.ssh.as_ref()?.shell(&t.src_dir.fs).ok().flatten()?
        };
        let entry = |dir: &Location| target.dir(&dir.join(&t.name).path);
        let op = match (t.kind, &t.dst_dir) {
            (OpKind::Delete, _) => HostOp::Delete(entry(&t.src_dir)),
            (OpKind::Move, Some(dst)) if dst.fs == t.src_dir.fs && !t.overwrite => {
                HostOp::Move { from: entry(&t.src_dir), to: entry(dst) }
            }
            // Not on the local disk, where rclone copies without a detour, shows how far
            // it is, and clones the file where the disk can.
            (OpKind::Copy, Some(dst))
                if dst.fs == t.src_dir.fs && !t.overwrite && !t.src_dir.is_local() =>
            {
                HostOp::Copy { from: entry(&t.src_dir), to: entry(dst) }
            }
            _ => return None,
        };
        let command = hostops::command(&op)?;
        self.hosts.has_sh(&target.run).then_some((target.run, command))
    }

    /// The server's own rsync for `t`, a transfer inside one server reached through the
    /// system ssh: its command line, and the command that runs it there. It carries out a
    /// copy, and a move onto what may be there ([`Transfer::overwrite`]), merging into
    /// what the target has as rclone would, without the files leaving the server. `None`
    /// for a move to a name not taken, which is `mv`'s ([`Self::host_command`]), and when
    /// the server has no sh or no rsync that takes rse's options. May open the master
    /// connection to the server.
    pub fn server_rsync(&self, t: &Transfer) -> Option<(Vec<String>, String)> {
        let op = match t.kind {
            OpKind::Copy => Op::Copy,
            OpKind::Move if t.overwrite => Op::Move,
            OpKind::Move | OpKind::Delete => return None,
        };
        let dst = t.dst_dir.as_ref()?;
        if t.name.is_empty() {
            return None;
        }
        let target = self.server_with_rsync(&t.src_dir, dst)?;
        let side = |dir: &Location| {
            Side::Local(match target.dir(&dir.path) {
                // One in the login directory as it is: rsync starts there, as every
                // command does.
                ShellDir::Absolute(path) | ShellDir::Home(path) => path.into(),
            })
        };
        let args = rsync::args(op, &side(&t.src_dir), &side(dst), &t.name, self.checksum);
        Some((target.run, hostops::rsync_command(&args)))
    }

    /// The server both `a` and `b` are on, when it is reached through the system ssh and
    /// has an rsync of its own that rse can use.
    fn server_with_rsync(&self, a: &Location, b: &Location) -> Option<ShellTarget> {
        if self.choice != EngineChoice::Auto || a.is_local() || a.fs != b.fs {
            return None;
        }
        let target = self.ssh.as_ref()?.shell(&a.fs).ok().flatten()?;
        self.hosts.has_rsync(&target.run).then_some(target)
    }

    /// The engine of copies and moves between `a` and `b`, either way. May open the
    /// master connection to a server, and ask whether it has rsync.
    pub fn between(&self, a: &Location, b: &Location) -> Engine {
        if self.rsync_sides(a, b).is_some() || self.server_with_rsync(a, b).is_some() {
            Engine::Rsync
        } else {
            Engine::Rclone
        }
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
        // Before the server is asked: without a local rsync that takes rse's options,
        // whatever the server has is of no use.
        if src.is_local() == dst.is_local() || !rsync::usable() {
            return None;
        }
        let remote = |loc: &Location| ssh.rsync_remote(&loc.fs).ok().flatten();
        match (src.is_local(), dst.is_local()) {
            (true, false) => Some((rsync::local(src), remote(dst)?.side(&dst.path))),
            (false, true) => Some((remote(src)?.side(&src.path), rsync::local(dst))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The local disk has a terminal without the system ssh; remotes have none.
    #[test]
    fn terminals_on_the_local_disk_need_no_ssh() {
        let e = Engines::new(None, EngineChoice::Auto, false);
        let target = e.shell(&Location::new("/", "home")).unwrap().unwrap();
        assert_eq!(target.dir("home"), crate::terminal::ShellDir::Absolute("/home".into()));
        assert_eq!(e.shell(&Location::new("nas:", "x")).unwrap(), None);
    }

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

    fn transfer(kind: OpKind, src: &str, dst: Option<Location>, overwrite: bool) -> Transfer {
        Transfer {
            kind,
            src_dir: Location::new("/", src),
            name: "a b".into(),
            is_dir: true,
            dst_dir: dst,
            size: None,
            overwrite,
        }
    }

    /// A delete, and a move to a free name, inside the local disk are the disk's own to do.
    #[test]
    fn deletes_and_moves_inside_the_local_disk_run_there() {
        let e = Engines::new(None, EngineChoice::Auto, false);
        let (run, command) =
            e.host_command(&transfer(OpKind::Delete, "tmp/x", None, false)).unwrap();
        assert_eq!(run, ["/bin/sh", "-c"]);
        assert!(command.ends_with(r#"exec rm -rf -- "$s"'"#), "{command}");
        let there = Some(Location::new("/", "tmp/y"));
        let (_, command) =
            e.host_command(&transfer(OpKind::Move, "tmp/x", there.clone(), false)).unwrap();
        assert!(command.ends_with(r#"exec mv -- "$s" "$d"'"#), "{command}");
        // Not a copy, which is the machine's own only on a server, a move onto what is to
        // be replaced, or one to another machine.
        assert_eq!(e.host_command(&transfer(OpKind::Copy, "tmp/x", there.clone(), false)), None);
        assert_eq!(e.host_command(&transfer(OpKind::Move, "tmp/x", there.clone(), true)), None);
        let nas = Some(Location::new("nas:", "x"));
        assert_eq!(e.host_command(&transfer(OpKind::Move, "tmp/x", nas, false)), None);
        // Nor on a remote the system ssh does not reach, nor with rclone chosen for all.
        let mut remote = transfer(OpKind::Delete, "x", None, false);
        remote.src_dir = Location::new("nas:", "x");
        assert_eq!(e.host_command(&remote), None);
        let rclone = Engines::new(None, EngineChoice::Rclone, false);
        assert_eq!(rclone.host_command(&transfer(OpKind::Delete, "tmp/x", None, false)), None);
        assert_eq!(
            Engines::default().host_command(&transfer(OpKind::Delete, "x", None, false)),
            None
        );
    }

    /// Turned off, terminals reach no server, whatever else could reach it.
    #[test]
    fn terminals_turned_off_reach_no_server() {
        let shell = ShellTarget {
            open: vec!["sh".into()],
            run: vec!["sh".into()],
            root: String::new(),
            env: vec![],
        };
        let mut e = Engines { local_shell: Some(shell), ..Engines::default() };
        let nas = Location::new("nas:", "x");
        let local = Location::new("/", "home");
        assert!(e.shell(&nas).unwrap().is_some());
        e.terminal = false;
        assert_eq!(e.shell(&nas).unwrap(), None);
        assert_eq!(e.shell(&local).unwrap(), None);
    }
}
