//! Whatever is typed into the address bar: parsing never panics, an on-the-fly fs carries
//! exactly the typed account, and a location's parent holds it.
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use rse::endpoint::{Address, Scheme};
use rse::fuzz::{remote_colon, split_params};
use rse::location::{Destination, Location};

const SCHEMES: [Scheme; 6] =
    [Scheme::Sftp, Scheme::Ftp, Scheme::Ftps, Scheme::Smb, Scheme::Webdav, Scheme::Webdavs];

fuzz_target!(|input: &str| {
    let (home, cwd) = (Path::new("/home/u"), Path::new("/work"));
    if let Some(i) = remote_colon(input) {
        assert_eq!(input.as_bytes()[i], b':');
    }
    if let Ok(l) = Location::parse(input, home, cwd) {
        exercise(&l);
    }
    for scheme in std::iter::once(None).chain(SCHEMES.map(Some)) {
        if let Ok(Destination::Ready(l)) = Location::parse_address(input, scheme, home, cwd) {
            exercise(&l);
        }
    }
    for scheme in SCHEMES {
        let Ok(address) = Address::parse(scheme, input) else { continue };
        let mut ep = address.endpoint.clone();
        ep.pass = address.password.clone();
        // The fs string must not let a typed host, user or password add other options.
        let fs = ep.fs();
        let body = fs.strip_suffix(':').expect("fs ends with a colon");
        let parts = split_params(body);
        assert!(parts[0].0.starts_with(':'), "{fs}");
        let get = |k: &str| parts.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        let keys: Vec<&str> = parts[1..].iter().map(|(k, _)| k.as_str()).collect();
        for k in &keys {
            assert!(["host", "port", "tls", "url", "user", "pass"].contains(k), "{k} in {fs}");
        }
        let mut sorted = keys.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "a key twice in {fs}");
        if !matches!(scheme, Scheme::Webdav | Scheme::Webdavs) {
            assert_eq!(get("host"), Some(ep.host.as_str()), "{fs}");
        }
        assert_eq!(get("user"), ep.user.as_deref(), "{fs}");
        assert_eq!(get("pass"), ep.pass.as_deref(), "{fs}");
        let _ = (ep.authority(), ep.root_label(&address.path), ep.login());
    }
});

fn exercise(l: &Location) {
    let _ = (l.address(), l.fs_string(), l.rc_pair(), l.segments(), l.to_string());
    let _ = (l.remote_name(), l.is_root(), l.contains(l), l.join("x"), l.child_remote("x"));
    if let Some((parent, name)) = l.parent() {
        assert!(parent.contains(l), "{parent} holds {l}");
        let _ = name;
    }
}
