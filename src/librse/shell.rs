//! A machine as a shell reaches it: the commands that open a terminal there and run a
//! command line there, and its directories.

/// How to reach a shell on a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTarget {
    /// The command that opens a terminal on the server; the remote command goes last.
    pub open: Vec<String>,
    /// A command that runs its last argument on the server, never asking anything.
    pub run: Vec<String>,
    /// The fs root on the server, as rclone's sftp backend reads it: absolute, or relative
    /// to the login directory.
    pub root: String,
    /// Environment for `open` beyond rse's own.
    pub env: Vec<(String, String)>,
}

/// A directory on the server, as the shell reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellDir {
    Absolute(String),
    /// Relative to the login directory (empty for that directory itself).
    Home(String),
}

impl ShellTarget {
    /// The local disk, from `root` (`/`).
    pub fn local(root: &str) -> Self {
        let sh = vec!["/bin/sh".to_owned(), "-c".to_owned()];
        Self { open: sh.clone(), run: sh, root: root.to_owned(), env: Vec::new() }
    }

    /// The pane's path of the directory `cwd` on the server, whose login directory is
    /// `home`: what [`Self::dir`] leads back from. `None` for a directory outside the root.
    pub fn path_of(&self, cwd: &str, home: &str) -> Option<String> {
        let under = |base: &str| {
            let base = base.trim_end_matches('/');
            let rest = cwd.strip_prefix(base)?;
            if rest.is_empty() { Some("") } else { rest.strip_prefix('/') }
        };
        if !cwd.starts_with('/') || !home.starts_with('/') {
            return None;
        }
        let path = if self.root.is_empty() {
            // The login directory: what is outside it is written from the root.
            under(home).unwrap_or(cwd)
        } else if self.root.starts_with('/') {
            under(&self.root)?
        } else {
            under(&join(&format!("{}/", home.trim_end_matches('/')), &self.root))?
        };
        Some(path.trim_end_matches('/').to_owned())
    }

    /// The directory of a path in the fs.
    pub fn dir(&self, path: &str) -> ShellDir {
        let joined = join(&self.root, path);
        if joined.starts_with('/') { ShellDir::Absolute(joined) } else { ShellDir::Home(joined) }
    }
}

impl ShellDir {
    /// The path, without what says where it starts.
    pub fn path(&self) -> &str {
        match self {
            Self::Absolute(p) | Self::Home(p) => p,
        }
    }

    /// How the window's title shows it: `/srv/www`, `~/photos`, `~`.
    pub fn shown(&self) -> String {
        match self {
            Self::Absolute(p) => p.clone(),
            Self::Home(p) if p.is_empty() => "~".to_owned(),
            Self::Home(p) => format!("~/{p}"),
        }
    }
}

/// `dir/name` as rclone joins an fs root and a path.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') {
        format!("{dir}{name}")
    } else if name.is_empty() {
        dir.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}
