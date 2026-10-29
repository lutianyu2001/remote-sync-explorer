//! ssh passwords rext remembers, in `~/.ssh/rext-passwords`.
//!
//! One login per line: `user@host` (`user@host:port` off port 22), a space, and the
//! password obscured the way rclone.conf keeps passwords (`rclone reveal` shows it).
//! Other lines, such as `#` comments, are kept as they are. Obscuring only keeps a
//! password from being read at a glance, so, as ssh does with private keys, rext uses
//! the file only while nobody else can read it. Deleting a line forgets that password,
//! as does `rext --forget-password user@host`.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::obscure::{obscure, reveal};

pub struct PasswordFile {
    path: PathBuf,
}

impl PasswordFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> Option<PathBuf> {
        crate::config::home_dir().map(|h| h.join(".ssh").join("rext-passwords"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The clear-text password remembered for `login`.
    pub fn get(&self, login: &str) -> io::Result<Option<String>> {
        let lines = self.read()?;
        let Some(obscured) = lines.iter().find_map(|l| entry(l).filter(|e| e.0 == login)) else {
            return Ok(None);
        };
        reveal(obscured.1).map(Some).map_err(|e| {
            io::Error::other(tr!(
                "passwords-bad-entry",
                path = self.path.display().to_string(),
                login = login,
                error = e
            ))
        })
    }

    /// Remembers `clear` for `login`, in place of any password remembered before.
    pub fn set(&self, login: &str, clear: &str) -> io::Result<()> {
        let mut lines = self.read()?;
        let line = format!("{login} {}", obscure(clear)?);
        // The first line for `login` gets the new password; any others go.
        let mut replaced = false;
        lines.retain_mut(|l| {
            if entry(l).is_none_or(|e| e.0 != login) {
                return true;
            }
            if replaced {
                return false;
            }
            l.clone_from(&line);
            replaced = true;
            true
        });
        if !replaced {
            lines.push(line);
        }
        self.write(&lines)
    }

    /// Forgets the password for `login`; `false` when none was remembered.
    pub fn remove(&self, login: &str) -> io::Result<bool> {
        let mut lines = self.read()?;
        let before = lines.len();
        lines.retain(|l| entry(l).is_none_or(|e| e.0 != login));
        if lines.len() == before {
            return Ok(false);
        }
        self.write(&lines)?;
        Ok(true)
    }

    /// The file's lines; none when it does not exist.
    fn read(&self) -> io::Result<Vec<String>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if fs::metadata(&self.path)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other(tr!(
                "passwords-not-private",
                path = self.path.display().to_string()
            )));
        }
        Ok(text.lines().map(str::to_owned).collect())
    }

    /// Replaces the file with `lines`, readable by this user only, through a temporary
    /// file so that it is never left half written.
    fn write(&self, lines: &[String]) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        let name = self.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let written = (|| {
            let mut file =
                fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            for line in lines {
                writeln!(file, "{line}")?;
            }
            file.sync_all()?;
            fs::rename(&tmp, &self.path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }
}

/// `(login, obscured)` of a line holding a password.
pub(crate) fn entry(line: &str) -> Option<(&str, &str)> {
    if line.trim_start().starts_with('#') {
        return None;
    }
    let mut words = line.split_whitespace();
    let login = words.next()?;
    let obscured = words.next()?;
    words.next().is_none().then_some((login, obscured))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &tempfile::TempDir) -> PasswordFile {
        PasswordFile::new(dir.path().join(".ssh").join("rext-passwords"))
    }

    #[test]
    fn remembers_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir);
        assert_eq!(f.get("me@box").unwrap(), None);
        f.set("me@box", "hunter2").unwrap();
        f.set("root@nas.lan:2222", "two words").unwrap();
        assert_eq!(f.get("me@box").unwrap().as_deref(), Some("hunter2"));
        assert_eq!(f.get("root@nas.lan:2222").unwrap().as_deref(), Some("two words"));
        assert_eq!(f.get("root@nas.lan").unwrap(), None);

        let text = fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains("hunter2") && text.starts_with("me@box "), "{text}");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(f.path()), 0o600);
        assert_eq!(mode(f.path().parent().unwrap()), 0o700);

        f.set("me@box", "changed").unwrap();
        assert_eq!(f.get("me@box").unwrap().as_deref(), Some("changed"));
        assert!(f.remove("me@box").unwrap());
        assert!(!f.remove("me@box").unwrap());
        assert_eq!(f.get("me@box").unwrap(), None);
        assert_eq!(fs::read_dir(f.path().parent().unwrap()).unwrap().count(), 1, "no temp files");
    }

    #[test]
    fn keeps_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir);
        f.set("me@box", "pw").unwrap();
        let mut text = fs::read_to_string(f.path()).unwrap();
        text = format!("# servers at home\n\n{text}not an entry\n");
        fs::write(f.path(), &text).unwrap();
        f.set("other@box", "pw2").unwrap();
        f.remove("me@box").unwrap();
        let after = fs::read_to_string(f.path()).unwrap();
        let lines: Vec<_> = after.lines().collect();
        assert_eq!(lines[..2], ["# servers at home", ""]);
        assert_eq!(lines[2], "not an entry");
        assert!(lines[3].starts_with("other@box "), "{after}");
    }

    #[test]
    fn refuses_a_file_others_can_read() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir);
        f.set("me@box", "pw").unwrap();
        fs::set_permissions(f.path(), fs::Permissions::from_mode(0o644)).unwrap();
        let err = f.get("me@box").unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
        assert!(f.set("me@box", "pw").is_err());
    }

    #[test]
    fn reports_a_damaged_entry() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&dir);
        f.set("me@box", "pw").unwrap();
        fs::write(f.path(), "me@box not-obscured!\n").unwrap();
        assert!(f.get("me@box").unwrap_err().to_string().contains("me@box"));
    }
}
