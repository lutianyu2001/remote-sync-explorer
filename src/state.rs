//! The jobs left unfinished, saved in `$XDG_STATE_HOME/rse/queue.json` (else under
//! `~/.local/state`) so that they go on at the next start, once the panes show their
//! endpoints again.
//!
//! A server opened by address is saved without its password: the job gets it back from
//! the pane that opens the server again (see `Queue::set_shown`). The file is private all
//! the same, as other settings of a remote may be in its fs string.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::endpoint::{Endpoint, Scheme};
use crate::jobs::{OpKind, Transfer};
use crate::location::Location;

/// The format of the file, raised when it changes incompatibly.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    jobs: Vec<SavedTransfer>,
}

#[derive(Serialize, Deserialize)]
struct SavedTransfer {
    kind: OpKind,
    src: SavedLocation,
    name: String,
    is_dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dst: Option<SavedLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<i64>,
    overwrite: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedLocation {
    fs: String,
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<SavedServer>,
}

#[derive(Serialize, Deserialize)]
struct SavedServer {
    scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

impl From<&Location> for SavedLocation {
    fn from(loc: &Location) -> Self {
        let server = loc.endpoint.as_deref();
        Self {
            fs: server.map_or_else(|| loc.fs.clone(), |e| e.login().fs()),
            path: loc.path.clone(),
            server: server.map(|e| SavedServer {
                scheme: e.scheme.name().to_owned(),
                user: e.user.clone(),
                host: e.host.clone(),
                port: e.port,
            }),
        }
    }
}

impl SavedLocation {
    fn location(self) -> Option<Location> {
        let endpoint = match self.server {
            Some(s) => Some(Box::new(Endpoint {
                scheme: Scheme::from_name(&s.scheme)?,
                user: s.user,
                host: s.host,
                port: s.port,
                pass: None,
            })),
            None => None,
        };
        Some(Location { fs: self.fs, path: self.path, endpoint })
    }
}

impl From<&Transfer> for SavedTransfer {
    fn from(t: &Transfer) -> Self {
        Self {
            kind: t.kind,
            src: (&t.src_dir).into(),
            name: t.name.clone(),
            is_dir: t.is_dir,
            dst: t.dst_dir.as_ref().map(Into::into),
            size: t.size,
            overwrite: t.overwrite,
        }
    }
}

impl SavedTransfer {
    fn transfer(self) -> Option<Transfer> {
        let dst_dir = match self.dst {
            Some(d) => Some(d.location()?),
            None => None,
        };
        Some(Transfer {
            kind: self.kind,
            src_dir: self.src.location()?,
            name: self.name,
            is_dir: self.is_dir,
            dst_dir,
            size: self.size,
            overwrite: self.overwrite,
        })
    }
}

/// `$XDG_STATE_HOME/rse/queue.json`, else under `~/.local/state`.
pub fn queue_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| crate::config::home_dir().map(|h| h.join(".local/state")))?;
    Some(base.join("rse").join("queue.json"))
}

/// The jobs saved at `path`; none when there is no file.
///
/// # Errors
/// Fails when the file cannot be read or is not one rse wrote.
pub fn load_queue(path: &Path) -> io::Result<Vec<Transfer>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let file: File = serde_json::from_str(&text).map_err(io::Error::other)?;
    if file.version != VERSION {
        return Err(io::Error::other(format!("unknown version {}", file.version)));
    }
    Ok(file.jobs.into_iter().filter_map(SavedTransfer::transfer).collect())
}

/// Saves `jobs` at `path`, private to this user, replacing the file in one step; no jobs
/// remove it.
///
/// # Errors
/// Fails when the directory or the file cannot be written.
pub fn save_queue(path: &Path, jobs: &[Transfer]) -> io::Result<()> {
    if jobs.is_empty() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let dir = path.parent().ok_or_else(|| io::Error::other("no directory"))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let file = File { version: VERSION, jobs: jobs.iter().map(Into::into).collect() };
    let text = serde_json::to_string_pretty(&file).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    match written.and_then(|()| fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn copy_to(dst: Location) -> Transfer {
        Transfer {
            kind: OpKind::Copy,
            src_dir: Location::new("/", "home/sky"),
            name: "big.iso".to_owned(),
            is_dir: false,
            dst_dir: Some(dst),
            size: Some(5),
            overwrite: true,
        }
    }

    #[test]
    fn jobs_come_back_as_saved_and_no_jobs_remove_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rse/queue.json");
        assert_eq!(load_queue(&path).unwrap(), vec![]);
        let delete = Transfer {
            kind: OpKind::Delete,
            dst_dir: None,
            size: None,
            ..copy_to(Location::new("/", "x"))
        };
        let jobs = vec![copy_to(Location::new("nas:", "backup")), delete];
        save_queue(&path, &jobs).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_queue(&path).unwrap(), jobs);
        save_queue(&path, &[]).unwrap();
        assert!(!path.exists());
    }

    /// The password of a server opened by address is not written down.
    #[test]
    fn a_server_is_saved_without_its_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.json");
        let mut server = Endpoint::ssh("me@box");
        server.scheme = Scheme::Ftp;
        server.pass = Some("c2VjcmV0".to_owned());
        let dst = Location::on(server.clone(), "srv");
        save_queue(&path, &[copy_to(dst.clone())]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("c2VjcmV0"), "{text}");
        let back = load_queue(&path).unwrap();
        let back_dst = back[0].dst_dir.as_ref().unwrap();
        assert_eq!(back_dst.fs, server.login().fs());
        assert_eq!(back_dst.endpoint.as_deref(), Some(&server.login()));
        assert_eq!(back_dst.path, dst.path);
    }

    #[test]
    fn a_file_of_another_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.json");
        fs::write(&path, r#"{"version": 99, "jobs": []}"#).unwrap();
        assert!(load_queue(&path).is_err());
        fs::write(&path, "not json").unwrap();
        assert!(load_queue(&path).is_err());
    }
}
