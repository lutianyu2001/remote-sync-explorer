//! Servers opened by address instead of a remote in rclone.conf: `sftp://me@box/srv`,
//! scp's `me@box:/srv`, `ftp://…`, `smb://…` and `webdav(s)://…`. Each becomes an
//! rclone on-the-fly backend (`:ftp,host=box,…:`).

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Sftp,
    Ftp,
    Ftps,
    Smb,
    Webdav,
    Webdavs,
}

/// Accepted spellings, canonical ones first.
const SCHEMES: [(&str, Scheme); 8] = [
    ("sftp", Scheme::Sftp),
    ("ftp", Scheme::Ftp),
    ("ftps", Scheme::Ftps),
    ("smb", Scheme::Smb),
    ("webdav", Scheme::Webdav),
    ("webdavs", Scheme::Webdavs),
    ("dav", Scheme::Webdav),
    ("davs", Scheme::Webdavs),
];

impl Scheme {
    pub fn from_name(name: &str) -> Option<Self> {
        SCHEMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|&(_, s)| s)
    }

    pub fn name(self) -> &'static str {
        SCHEMES.iter().find(|&&(_, s)| s == self).map_or("", |(n, _)| n)
    }

    /// Whether logging in takes a password; sftp goes through the system ssh instead.
    pub fn uses_password(self) -> bool {
        self != Self::Sftp
    }

    /// `ftp://rest` → (`Ftp`, `rest`) for a known scheme.
    pub fn split_prefix(input: &str) -> Option<(Self, &str)> {
        let (name, rest) = input.split_once("://")?;
        Some((Self::from_name(name)?, rest))
    }
}

/// A server and the account on it. Two endpoints that differ only in `pass` are the same
/// login; [`Endpoint::login`] drops the password for comparing them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub scheme: Scheme,
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// Obscured the way rclone.conf stores passwords (`rclone obscure`), never clear text.
    pub pass: Option<String>,
}

/// An address as typed: where it leads, and a clear-text password if it held one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub endpoint: Endpoint,
    pub path: String,
    pub password: Option<String>,
}

impl Address {
    /// Parses what follows `scheme://`: `[user[:password]@]host[:port][/path]`.
    ///
    /// sftp paths follow the usual URL convention: `/srv` is absolute, `/~/docs` and an
    /// empty path are in the login's home directory. Other paths are relative to where
    /// the server puts a login (for smb the first name is the share).
    pub fn parse(scheme: Scheme, rest: &str) -> Result<Self, String> {
        let rest = rest.trim();
        let (authority, url_path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, mut password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
                None => (Some(percent_decode(u)), None),
            },
            None => (None, None),
        };
        let (host, port) = split_port(hostport)?;
        if host.is_empty() {
            return Err(tr!("needs-host", scheme = scheme.name()));
        }
        let mut user = user.filter(|u| !u.is_empty());
        if matches!(scheme, Scheme::Ftp | Scheme::Ftps) && user.is_none() {
            user = Some("anonymous".to_owned());
            password.get_or_insert_with(|| "anonymous".to_owned());
        }
        let path = match scheme {
            Scheme::Sftp => match url_path {
                "" | "/~" => String::new(),
                p => p.strip_prefix("/~/").map_or_else(|| p.to_owned(), str::to_owned),
            },
            _ => url_path.trim_start_matches('/').to_owned(),
        };
        let endpoint = Endpoint { scheme, user, host: host.to_owned(), port, pass: None };
        Ok(Self { endpoint, path, password })
    }
}

impl Endpoint {
    /// scp's `user@host` (or a bare ssh host) as an sftp endpoint.
    pub fn ssh(dest: &str) -> Self {
        let (user, host) = match dest.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_owned()), h),
            None => (None, dest),
        };
        Self { scheme: Scheme::Sftp, user, host: host.to_owned(), port: None, pass: None }
    }

    /// The same account without its password.
    pub fn login(&self) -> Self {
        Self { pass: None, ..self.clone() }
    }

    /// The rclone on-the-fly fs, e.g. `:ftp,host=box,user=me,pass=…:`.
    pub fn fs(&self) -> String {
        let mut params = Vec::new();
        match self.scheme {
            // `rclone::system_ssh` turns this into a system ssh command, so that
            // ~/.ssh/config applies as it does for scp.
            Scheme::Sftp => {
                params.push(("host", self.host.clone()));
                if let Some(p) = self.port {
                    params.push(("port", p.to_string()));
                }
            }
            Scheme::Ftp | Scheme::Ftps | Scheme::Smb => {
                params.push(("host", self.host.clone()));
                let port = match (self.port, self.scheme) {
                    (Some(p), _) => Some(p),
                    // Implicit TLS has its own port; rclone defaults to 21 regardless.
                    (None, Scheme::Ftps) => Some(990),
                    (None, _) => None,
                };
                if let Some(p) = port {
                    params.push(("port", p.to_string()));
                }
                if self.scheme == Scheme::Ftps {
                    params.push(("tls", "true".to_owned()));
                }
            }
            Scheme::Webdav | Scheme::Webdavs => {
                let http = if self.scheme == Scheme::Webdav { "http" } else { "https" };
                let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
                params.push(("url", format!("{http}://{}{port}", self.bracketed_host())));
            }
        }
        let backend = match self.scheme {
            Scheme::Sftp => "sftp",
            Scheme::Ftp | Scheme::Ftps => "ftp",
            Scheme::Smb => "smb",
            Scheme::Webdav | Scheme::Webdavs => "webdav",
        };
        if let Some(u) = &self.user {
            params.push(("user", u.clone()));
        }
        if let Some(p) = &self.pass {
            params.push(("pass", p.clone()));
        }
        let mut fs = format!(":{backend}");
        for (key, value) in params {
            let _ = write!(fs, ",{key}={}", quote(&value));
        }
        fs.push(':');
        fs
    }

    /// `me@box:2222`, without the password.
    pub fn authority(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            let _ = write!(s, "{}@", percent_encode_user(u));
        }
        s.push_str(&self.bracketed_host());
        if let Some(p) = self.port {
            let _ = write!(s, ":{p}");
        }
        s
    }

    /// The start of the address text for `path` up to the first path name:
    /// `me@box/~` (home) or `me@box/` (absolute) for sftp, `box` otherwise.
    pub fn root_label(&self, path: &str) -> String {
        match (self.scheme, path.starts_with('/')) {
            (Scheme::Sftp, true) => format!("{}/", self.authority()),
            (Scheme::Sftp, false) => format!("{}/~", self.authority()),
            _ => self.authority(),
        }
    }

    /// The address text after `scheme://`, which [`Address::parse`] reads back.
    pub fn address(&self, path: &str) -> String {
        let root = self.root_label(path);
        let rest = path.trim_start_matches('/');
        if rest.is_empty() || root.ends_with('/') {
            format!("{root}{rest}")
        } else {
            format!("{root}/{rest}")
        }
    }

    fn bracketed_host(&self) -> String {
        if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() }
    }
}

/// `host`, `host:21` or `[::1]:21`.
fn split_port(hostport: &str) -> Result<(&str, Option<u16>), String> {
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (host, after) = inner.split_once(']').ok_or_else(|| tr!("ipv6-bracket"))?;
        (host, after.strip_prefix(':'))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    match port {
        None | Some("") => Ok((host, None)),
        Some(p) => p.parse().map(|p| (host, Some(p))).map_err(|_| tr!("bad-port", port = p)),
    }
}

/// A connection-string value, quoted when it holds `,` `:` or a quote.
fn quote(value: &str) -> String {
    if value.contains([',', ':', '\'', '"']) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_owned()
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escapes what would end the user name in an address.
fn percent_encode_user(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '%' | '@' | ':' | '/' => format!("%{:02X}", u32::from(c)),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Address {
        let (scheme, rest) = Scheme::split_prefix(s).unwrap();
        Address::parse(scheme, rest).unwrap()
    }

    #[test]
    fn schemes_and_aliases() {
        assert_eq!(Scheme::split_prefix("DAVS://h"), Some((Scheme::Webdavs, "h")));
        assert_eq!(Scheme::split_prefix("nfs://h"), None);
        assert_eq!(Scheme::Webdavs.name(), "webdavs");
    }

    #[test]
    fn sftp_paths_follow_url_conventions() {
        let paths: Vec<_> = [
            "sftp://me@box",
            "sftp://me@box/~",
            "sftp://me@box/~/docs",
            "sftp://me@box/srv/",
            "sftp://me@box/",
        ]
        .iter()
        .map(|s| addr(s).path)
        .collect();
        assert_eq!(paths, ["", "", "docs", "/srv/", "/"]);
        let e = addr("sftp://me@box:2222/srv").endpoint;
        assert_eq!(e.fs(), ":sftp,host=box,port=2222,user=me:");
        assert_eq!(Endpoint::ssh("box").fs(), ":sftp,host=box:");
    }

    #[test]
    fn logins_and_ports() {
        let a = addr("ftp://me%40corp:p%3Ass@[::1]:2121/pub");
        assert_eq!(a.endpoint.user.as_deref(), Some("me@corp"));
        assert_eq!(a.password.as_deref(), Some("p:ss"));
        assert_eq!((a.endpoint.host.as_str(), a.endpoint.port), ("::1", Some(2121)));
        assert_eq!(a.path, "pub");
        assert_eq!(a.endpoint.authority(), "me%40corp@[::1]:2121");
        let anon = addr("ftp://mirror.example");
        assert_eq!(anon.endpoint.user.as_deref(), Some("anonymous"));
        assert_eq!(anon.password.as_deref(), Some("anonymous"));
        assert!(Address::parse(Scheme::Ftp, "h:99999").is_err());
        assert!(Address::parse(Scheme::Smb, "me@/share").is_err());
    }

    #[test]
    fn fs_strings_per_backend() {
        let mut e = addr("smb://me@nas/share/dir").endpoint;
        e.pass = Some("OBSC".to_owned());
        assert_eq!(e.fs(), ":smb,host=nas,user=me,pass=OBSC:");
        assert_eq!(addr("ftps://me@h").endpoint.fs(), ":ftp,host=h,port=990,tls=true,user=me:");
        assert_eq!(
            addr("webdavs://me@dav.example:8443/remote.php").endpoint.fs(),
            ":webdav,url='https://dav.example:8443',user=me:"
        );
        assert_eq!(addr("webdav://[::1]").endpoint.fs(), ":webdav,url='http://[::1]':");
        assert_eq!(addr("sftp://o'k@box").endpoint.fs(), ":sftp,host=box,user='o''k':");
    }

    #[test]
    fn address_text_reads_back() {
        for s in [
            "sftp://me@box/~",
            "sftp://me@box/~/docs",
            "sftp://me@box/srv/x",
            "sftp://me@box/",
            "smb://me@nas/share/d",
            "ftp://anonymous@h:2121",
        ] {
            let a = addr(s);
            let text = format!("{}://{}", a.endpoint.scheme.name(), a.endpoint.address(&a.path));
            assert_eq!(text, s);
            assert_eq!(addr(&text).path, a.path);
        }
    }
}
