//! A place a pane can show: an rclone fs plus a path inside it.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};

use crate::endpoint::{Address, Endpoint, Scheme};

/// `fs` is `/` for the local disk or `name:` for a remote; `path` is relative to it.
///
/// Local paths are kept relative to `/` (`home/sky`), so they can be passed as an rc
/// `remote` parameter unchanged. Remote paths are kept as written after the colon, so
/// `sftp:/abs` and `sftp:rel` stay different.
///
/// `endpoint` is set for a server opened by address (`sftp://me@box/srv`); `fs` is then
/// its on-the-fly backend. It only changes how the location is shown, so equality and
/// hashing look at `fs` and `path` alone.
#[derive(Debug, Clone)]
pub struct Location {
    pub fs: String,
    pub path: String,
    pub endpoint: Option<Box<Endpoint>>,
}

impl PartialEq for Location {
    fn eq(&self, other: &Self) -> bool {
        self.fs == other.fs && self.path == other.path
    }
}

impl Eq for Location {}

impl Hash for Location {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fs.hash(state);
        self.path.hash(state);
    }
}

/// What tells one endpoint (the local disk, a remote, a server opened by address) from
/// another: the fs string, without the password of a server opened by address, so that
/// the same login is the same endpoint whatever password was typed.
pub fn endpoint_key(loc: &Location) -> String {
    loc.endpoint.as_ref().map_or_else(|| loc.fs.clone(), |e| e.login().fs())
}

/// Where a typed address leads: a location, or a server that needs its password settled
/// (asked for, or obscured for rclone) first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Ready(Location),
    Login(Address),
}

impl Location {
    pub fn new(fs: impl Into<String>, path: impl Into<String>) -> Self {
        let path: String = path.into();
        let path = trim_trailing_slashes(&path);
        Self { fs: fs.into(), path, endpoint: None }
    }

    /// `path` on a server opened by address.
    pub fn on(endpoint: Endpoint, path: &str) -> Self {
        let fs = endpoint.fs();
        Self { fs, path: trim_trailing_slashes(path), endpoint: Some(Box::new(endpoint)) }
    }

    fn with_path(&self, path: String) -> Self {
        Self { fs: self.fs.clone(), path, endpoint: self.endpoint.clone() }
    }

    /// A local absolute path.
    pub fn local(path: &Path) -> Self {
        let clean = lexical_clean(path);
        let s = clean.to_string_lossy();
        Self::new("/", s.trim_start_matches('/'))
    }

    /// The root of a remote, `name` without the colon.
    pub fn remote_root(name: &str) -> Self {
        Self::new(format!("{name}:"), "")
    }

    /// Parses what a user types: `~/x`, `/abs`, `rel` (against `cwd`), `nas:dir`,
    /// `:sftp,host=h:dir`, and scp's `user@host:dir`, which goes through the system ssh.
    pub fn parse(input: &str, home: &Path, cwd: &Path) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err(tr!("empty-path"));
        }
        if input == "~" {
            return Ok(Self::local(home));
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return Ok(Self::local(&home.join(rest)));
        }
        if input.starts_with('/') {
            return Ok(Self::local(Path::new(input)));
        }
        if let Some(colon) = remote_colon(input) {
            let (fs, path) = input.split_at(colon + 1);
            let name = &fs[..colon];
            if is_ssh_dest(name) {
                return Ok(Self::on(Endpoint::ssh(name), path));
            }
            return Ok(Self::new(fs, path));
        }
        Ok(Self::local(&cwd.join(input)))
    }

    /// Parses an address: a URL such as `ftp://me@box/pub`, or anything [`Self::parse`]
    /// reads. `scheme` is the protocol tag shown in front of the text, which then holds
    /// only what follows `scheme://`.
    pub fn parse_address(
        input: &str,
        scheme: Option<Scheme>,
        home: &Path,
        cwd: &Path,
    ) -> Result<Destination, String> {
        let input = input.trim();
        let Some((scheme, rest)) =
            scheme.map(|s| (s, input)).or_else(|| Scheme::split_prefix(input))
        else {
            if let Some((name, _)) = input.split_once("://")
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err(tr!("unknown-protocol", name = name));
            }
            return Self::parse(input, home, cwd).map(Destination::Ready);
        };
        let address = Address::parse(scheme, rest)?;
        let ep = &address.endpoint;
        if address.password.is_some() || (scheme.uses_password() && ep.user.is_some()) {
            Ok(Destination::Login(address))
        } else {
            Ok(Destination::Ready(Self::on(address.endpoint, &address.path)))
        }
    }

    /// The text to edit in the address field, and the protocol tag shown in front of it.
    pub fn address(&self) -> (Option<Scheme>, String) {
        match &self.endpoint {
            Some(ep) => (Some(ep.scheme), ep.address(&self.path)),
            None => (None, self.to_string()),
        }
    }

    pub fn is_local(&self) -> bool {
        self.fs == "/"
    }

    pub fn is_root(&self) -> bool {
        self.path.is_empty() || self.path == "/"
    }

    /// The remote name for remotes (`nas`), `None` for local.
    pub fn remote_name(&self) -> Option<&str> {
        (!self.is_local()).then(|| self.fs.trim_end_matches(':'))
    }

    pub fn join(&self, name: &str) -> Self {
        self.with_path(join_path(&self.path, name))
    }

    /// The path of `name` inside this location, as an rc `remote` parameter.
    pub fn child_remote(&self, name: &str) -> String {
        join_path(&self.path, name)
    }

    /// The parent location and the name of this directory inside it.
    pub fn parent(&self) -> Option<(Self, String)> {
        if self.is_root() {
            return None;
        }
        let (parent, name) = match self.path.rfind('/') {
            Some(0) => ("/", &self.path[1..]),
            Some(i) => (&self.path[..i], &self.path[i + 1..]),
            None => ("", self.path.as_str()),
        };
        Some((self.with_path(parent.to_owned()), name.to_owned()))
    }

    /// The full rclone fs string, e.g. `nas:backup/2026` or `/home/sky`, as used by
    /// `sync/*` and on-the-fly fs parameters.
    pub fn fs_string(&self) -> String {
        if self.fs.ends_with(':') || self.fs.ends_with('/') {
            format!("{}{}", self.fs, self.path)
        } else {
            format!("{}/{}", self.fs, self.path)
        }
    }

    /// This location as an rc (`fs`, `remote`) pair the way rclone's command line splits
    /// paths: the parent directory as the fs and the last name as the remote. rclone strips
    /// a leading `/` from `remote`, so absolute remote paths must live in the fs string.
    pub fn rc_pair(&self) -> (String, String) {
        match self.parent() {
            Some((parent, name)) => (parent.fs_string(), name),
            None => (self.fs_string(), String::new()),
        }
    }

    /// True when `other` is this location or inside it.
    pub fn contains(&self, other: &Self) -> bool {
        if self.fs != other.fs {
            return false;
        }
        if self.path == other.path {
            return true;
        }
        match self.path.as_str() {
            "" => !other.path.starts_with('/'),
            "/" => other.path.starts_with('/'),
            p => other.path.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')),
        }
    }

    /// Breadcrumb segments from the root down: a label and the location it leads to.
    pub fn segments(&self) -> Vec<(String, Self)> {
        let root_path = if self.path.starts_with('/') { "/" } else { "" };
        let root_label = match &self.endpoint {
            _ if self.is_local() => "/".to_owned(),
            Some(ep) => ep.root_label(&self.path),
            None => format!("{}{root_path}", self.fs),
        };
        let root = self.with_path(root_path.to_owned());
        let mut out = vec![(root_label, root.clone())];
        let mut cur = root;
        for part in self.path.split('/').filter(|p| !p.is_empty()) {
            cur = cur.join(part);
            out.push((part.to_owned(), cur.clone()));
        }
        out
    }
}

/// The location as the user writes it; [`Location::parse_address`] reads it back.
impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.endpoint {
            Some(ep) => write!(f, "{}://{}", ep.scheme.name(), ep.address(&self.path)),
            None => f.write_str(&self.fs_string()),
        }
    }
}

/// Whether a name listed in a directory names one entry inside it, and so can be joined
/// to the directory's path. A server could list `..` or `a/../../b`, which would lead out.
pub fn safe_name(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']))
}

fn join_path(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else if path.ends_with('/') {
        format!("{path}{name}")
    } else {
        format!("{path}/{name}")
    }
}

fn trim_trailing_slashes(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') { "/".to_owned() } else { trimmed.to_owned() }
}

/// The index of the colon ending a remote name, following rclone's rules closely enough:
/// a leading `:` starts an on-the-fly backend, a remote name contains no `/`, and the
/// `,param=value` list after it may quote values with `'` or `"` to hide `:` and `,`.
pub(crate) fn remote_colon(input: &str) -> Option<usize> {
    let start = usize::from(input.starts_with(':'));
    let name_end = input[start..].find([',', ':'])? + start;
    if input[..name_end].contains('/') {
        return None;
    }
    // A doubled quote inside a quoted value closes and reopens it, so needs no case here.
    let mut quote = None;
    for (i, c) in input.char_indices().skip_while(|&(i, _)| i < name_end) {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, ':') => return Some(i),
            _ => {}
        }
    }
    None
}

/// True for the part before the colon of scp's `user@host:dir`.
fn is_ssh_dest(name: &str) -> bool {
    name.contains('@') && !name.starts_with(':') && !name.contains([',', ':', '/'])
}

/// Removes `.` and `..` without touching the file system.
fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_names_of_one_entry_are_safe() {
        assert!(safe_name("photos") && safe_name("..."), "dots are names too");
        assert!(safe_name(".hidden") && safe_name("a b\\c"));
        for bad in ["", ".", "..", "a/b", "../etc", "/", "a\0b"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }

    fn parse(s: &str) -> Location {
        Location::parse(s, Path::new("/home/sky"), Path::new("/work")).unwrap()
    }

    #[test]
    fn parses_local_and_remote_forms() {
        assert_eq!(parse("~"), Location::new("/", "home/sky"));
        assert_eq!(parse("~/a/b/"), Location::new("/", "home/sky/a/b"));
        assert_eq!(parse("/"), Location::new("/", ""));
        assert_eq!(parse("/x/../y/./z"), Location::new("/", "y/z"));
        assert_eq!(parse("rel"), Location::new("/", "work/rel"));
        assert_eq!(parse("nas:backup/2026"), Location::new("nas:", "backup/2026"));
        assert_eq!(parse("nas:"), Location::new("nas:", ""));
        assert_eq!(parse("sftp:/abs/dir"), Location::new("sftp:", "/abs/dir"));
        assert_eq!(parse(":sftp,host=h:dir"), Location::new(":sftp,host=h:", "dir"));
        assert_eq!(parse("./a:b"), Location::new("/", "work/a:b"));
        assert_eq!(parse("a/b,c:d"), Location::new("/", "work/a/b,c:d"));
        assert!(Location::parse("  ", Path::new("/"), Path::new("/")).is_err());
    }

    #[test]
    fn connection_string_values_may_hold_slashes_and_quoted_colons() {
        assert_eq!(
            parse(":sftp,host=h,key_file=/k/id:/srv"),
            Location::new(":sftp,host=h,key_file=/k/id:", "/srv")
        );
        assert_eq!(
            parse(":sftp,ssh='ssh -o A=b:c h',user=\"x,y\":dir"),
            Location::new(":sftp,ssh='ssh -o A=b:c h',user=\"x,y\":", "dir")
        );
        assert_eq!(parse("nas,p='it''s:x':d"), Location::new("nas,p='it''s:x':", "d"));
        assert_eq!(
            parse(":sftp,ssh='unclosed:x"),
            Location::new("/", "work/:sftp,ssh='unclosed:x")
        );
    }

    #[test]
    fn scp_style_destinations_are_sftp() {
        assert_eq!(parse("me@box:"), Location::new(":sftp,host=box,user=me:", ""));
        assert_eq!(parse("me@box:/srv/x"), Location::new(":sftp,host=box,user=me:", "/srv/x"));
        assert_eq!(parse("me@box:rel"), Location::new(":sftp,host=box,user=me:", "rel"));
        assert_eq!(parse("o'k@box:"), Location::new(":sftp,host=box,user='o''k':", ""));
        assert_eq!(parse(":sftp,user=me@x:d"), Location::new(":sftp,user=me@x:", "d"));
    }

    fn address(s: &str) -> Destination {
        Location::parse_address(s, None, Path::new("/home/sky"), Path::new("/work")).unwrap()
    }

    fn ready(s: &str) -> Location {
        match address(s) {
            Destination::Ready(loc) => loc,
            Destination::Login(a) => panic!("{s} wants a login: {a:?}"),
        }
    }

    #[test]
    fn servers_by_address_are_shown_as_urls() {
        for (typed, shown) in [
            ("me@box:", "sftp://me@box/~"),
            ("me@box:rel", "sftp://me@box/~/rel"),
            ("me@box:/srv/x", "sftp://me@box/srv/x"),
            ("sftp://box:2222/srv", "sftp://box:2222/srv"),
            ("webdavs://dav.example/files", "webdavs://dav.example/files"),
            ("o'k@box:d", "sftp://o'k@box/~/d"),
        ] {
            let loc = ready(typed);
            assert_eq!(loc.to_string(), shown);
            assert_eq!(ready(shown), loc);
        }
        let raw = ":sftp,ssh='ssh -p 2 me@box':/x";
        assert_eq!(ready(raw).to_string(), raw);
        assert_eq!(ready("me@box:/srv").fs_string(), ":sftp,host=box,user=me:/srv");
        let labels: Vec<_> =
            ready("me@box:/srv/x").segments().into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels, ["me@box/", "srv", "x"]);
        let labels: Vec<_> = ready("me@box:docs").segments().into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels, ["me@box/~", "docs"]);
        assert_eq!(ready("me@box:/srv").parent().unwrap().0.to_string(), "sftp://me@box/");
    }

    #[test]
    fn addresses_with_a_tag_or_a_password() {
        let home = Path::new("/");
        let tagged = Location::parse_address("me@box/srv", Some(Scheme::Sftp), home, home);
        assert_eq!(tagged, Ok(Destination::Ready(ready("sftp://me@box/srv"))));
        assert_eq!(
            ready("sftp://me@box/srv").address(),
            (Some(Scheme::Sftp), "me@box/srv".to_owned())
        );
        assert_eq!(ready("nas:x").address(), (None, "nas:x".to_owned()));
        // A user on a protocol with passwords, or a password in the URL: log in first.
        assert!(matches!(address("smb://me@nas/share"), Destination::Login(_)));
        assert!(matches!(address("webdav://me:pw@h"), Destination::Login(_)));
        assert!(matches!(address("ftp://mirror.example/pub"), Destination::Login(_)));
        assert!(matches!(address("webdav://h/x"), Destination::Ready(_)));
        let err = Location::parse_address("nfs://h/x", None, home, home).unwrap_err();
        assert!(err.contains("unknown protocol nfs://"), "{err}");
        // A connection string holding a URL is not a protocol prefix.
        assert!(matches!(address(":webdav,url='http://h':x"), Destination::Ready(_)));
    }

    #[test]
    fn fs_string_and_display() {
        assert_eq!(Location::new("/", "home/sky").fs_string(), "/home/sky");
        assert_eq!(Location::new("/", "").fs_string(), "/");
        assert_eq!(Location::new("nas:", "a/b").to_string(), "nas:a/b");
        assert_eq!(Location::new("sftp:", "/abs").to_string(), "sftp:/abs");
    }

    #[test]
    fn parent_and_join_round_trip() {
        let l = Location::new("nas:", "a/b");
        let (p, name) = l.parent().unwrap();
        assert_eq!((p.clone(), name.as_str()), (Location::new("nas:", "a"), "b"));
        assert_eq!(p.join("b"), l);
        assert_eq!(Location::new("nas:", "a").parent().unwrap().0, Location::new("nas:", ""));
        assert_eq!(Location::new("sftp:", "/a").parent().unwrap().0, Location::new("sftp:", "/"));
        assert!(Location::new("sftp:", "/").parent().is_none());
        assert!(Location::new("/", "").parent().is_none());
        assert_eq!(Location::new("sftp:", "/").join("a"), Location::new("sftp:", "/a"));
    }

    #[test]
    fn rc_pair_keeps_absolute_paths_in_the_fs() {
        assert_eq!(
            Location::new("sftp:", "/srv/data").rc_pair(),
            ("sftp:/srv".to_owned(), "data".to_owned())
        );
        assert_eq!(
            Location::new("sftp:", "/srv").rc_pair(),
            ("sftp:/".to_owned(), "srv".to_owned())
        );
        assert_eq!(
            Location::new("/", "home/sky").rc_pair(),
            ("/home".to_owned(), "sky".to_owned())
        );
        assert_eq!(Location::new("nas:", "a").rc_pair(), ("nas:".to_owned(), "a".to_owned()));
        assert_eq!(Location::new("nas:", "").rc_pair(), ("nas:".to_owned(), String::new()));
    }

    #[test]
    fn contains_is_component_wise() {
        let a = Location::new("/", "home/sky");
        assert!(a.contains(&Location::new("/", "home/sky")));
        assert!(a.contains(&Location::new("/", "home/sky/x")));
        assert!(!a.contains(&Location::new("/", "home/skyline")));
        assert!(!a.contains(&Location::new("nas:", "home/sky/x")));
        assert!(Location::new("/", "").contains(&a));
        assert!(Location::new("nas:", "").contains(&Location::new("nas:", "x")));
    }

    #[test]
    fn segments_lead_to_each_level() {
        let segs = Location::new("nas:", "backup/2026").segments();
        let labels: Vec<_> = segs.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["nas:", "backup", "2026"]);
        assert_eq!(segs[1].1, Location::new("nas:", "backup"));
        let local: Vec<_> =
            Location::new("/", "home").segments().into_iter().map(|(l, _)| l).collect();
        assert_eq!(local, ["/", "home"]);
        assert_eq!(Location::new("sftp:", "/srv").segments()[1].1, Location::new("sftp:", "/srv"));
    }
}
