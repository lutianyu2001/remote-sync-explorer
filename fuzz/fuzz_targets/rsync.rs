//! rsync's command line and output: no path or ssh option becomes an rsync option, the
//! remote shell and the server read back as ssh's own, a path is never read as another
//! server's or a daemon's; reading the output never panics, does not depend on how it
//! comes, and narrows the total down to the true one.
#![no_main]

use std::path::PathBuf;

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use rse::fuzz::RsyncProgress;
use rse::rsync::{Op, RsyncRemote, Side, args};

#[derive(Arbitrary, Debug)]
enum Input {
    Args(Transfer),
    /// Any output, in pieces cut at `cuts`.
    Output {
        data: Vec<u8>,
        cuts: Vec<u16>,
    },
    /// Output as rsync writes it, for a job of `total` bytes.
    Job {
        total: u64,
        steps: Vec<(u32, Option<Vec<u8>>)>,
    },
}

/// A server reached with `ssh` (its destination last), and a transfer between a directory
/// on it and a local one.
#[derive(Arbitrary, Debug)]
struct Transfer {
    ssh: Vec<String>,
    root: String,
    remote: String,
    local: String,
    name: String,
    upload: bool,
    moving: bool,
    checksum: bool,
}

fuzz_target!(|input: Input| match input {
    Input::Args(transfer) => command_line(&transfer),
    Input::Output { data, cuts } => output(&data, &cuts),
    Input::Job { total, steps } => job(total, &steps),
});

fn command_line(t: &Transfer) {
    let Transfer { ssh, root, remote, local, name, upload, moving, checksum } = t;
    let (name, upload, moving, checksum) = (name.as_str(), *upload, *moving, *checksum);
    let Some(r) = RsyncRemote::new(ssh, root) else { return };
    let (dest, command) = ssh.split_last().expect("a destination");
    assert_eq!(split_rsh(&r.rsh).as_deref(), Ok(command), "{}", r.rsh);
    // rsync passes ssh the account with -l and the server after its options: as ssh
    // reads the destination.
    let probe = format!("{}:x", r.host);
    let Some(Spec::Remote { user, machine, path }) = hostspec(probe.as_bytes()) else {
        panic!("{probe} is not read as a server");
    };
    assert_eq!(path, b"x");
    let (ssh_user, ssh_host) = match dest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, dest.as_str()),
    };
    assert_eq!(user.as_deref(), ssh_user.map(str::as_bytes), "{dest} as {}", r.host);
    assert_eq!(machine, ssh_host.as_bytes(), "{dest} as {}", r.host);
    assert!(!machine.starts_with(b"-"), "{dest} would be an ssh option");

    let here = Side::Local(PathBuf::from(local));
    let there = r.side(remote);
    let op = if moving { Op::Move } else { Op::Copy };
    let (src, dst) = if upload { (&here, &there) } else { (&there, &here) };
    let a = args(op, src, dst, name, checksum);
    let a: Vec<&[u8]> =
        a.iter().map(|a| std::os::unix::ffi::OsStrExt::as_bytes(a.as_os_str())).collect();
    let end = a.iter().position(|a| *a == b"--").expect("--");
    assert_eq!(a.len(), end + 3, "two paths after --: {a:?}");
    assert!(a[..end].iter().all(|a| a.starts_with(b"-")), "{a:?}");
    assert!(a.contains(&&b"-s"[..]), "paths reach the server as they are");
    assert_eq!(a.contains(&&b"--remove-source-files"[..]), moving);
    assert_eq!(a.contains(&&b"--checksum"[..]), checksum);
    assert!(a.contains(&format!("--rsh={}", r.rsh).as_bytes()));
    let (src_arg, dst_arg) = (a[end + 1], a[end + 2]);
    assert!(dst_arg.ends_with(b"/"), "into the directory");
    let component = !name.is_empty() && !name.contains('/') && name != "." && name != "..";
    for (arg, local_side, path) in [
        (src_arg, !upload, if component { join(&r.path(remote), name) } else { String::new() }),
        (dst_arg, upload, r.path(remote)),
    ] {
        match hostspec(arg) {
            Some(Spec::Local) => {
                assert!(!local_side, "{arg:?} on the server read as local");
                assert!(!arg.starts_with(b"-"));
            }
            Some(Spec::Remote { machine, path: got, .. }) => {
                assert!(local_side, "local {arg:?} read as on {machine:?}");
                if !path.is_empty() {
                    let got = String::from_utf8(got).expect("UTF-8");
                    assert!(same_place(&got, &path), "{got:?} for {path:?}");
                }
            }
            None => panic!("{arg:?} is read as a daemon's"),
        }
    }
    if component {
        assert!(
            src_arg.ends_with(format!("/{name}").as_bytes())
                || src_arg.ends_with(format!(":{name}").as_bytes()),
            "{src_arg:?}"
        );
    }
}

/// `dir/name` as `Location::fs_string` joins them.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') {
        format!("{dir}{name}")
    } else if name.is_empty() {
        dir.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether rsync's `got` names `want` on the server, trailing `/`s aside.
fn same_place(got: &str, want: &str) -> bool {
    let trim = |p: &str| {
        let t = p.trim_end_matches('/');
        if t.is_empty() && !p.is_empty() { "/".to_owned() } else { t.to_owned() }
    };
    let (got, want) = (trim(got), trim(want));
    got == want
        || (want.is_empty() && got == ".")
        || got == format!("./{want}")
        || (want.starts_with("//") && got == trim(&format!("/{}", want.trim_start_matches('/'))))
}

/// How rsync 3.2.7 reads a path argument (`check_for_hostspec` in main.c).
#[derive(Debug, PartialEq)]
enum Spec {
    Local,
    Remote { user: Option<Vec<u8>>, machine: Vec<u8>, path: Vec<u8> },
}

/// `None` for an rsync daemon's (`rsync://`, `host::module`).
fn hostspec(s: &[u8]) -> Option<Spec> {
    if s.len() >= 8 && s[..8].eq_ignore_ascii_case(b"rsync://") {
        return None;
    }
    let mut host_start = 0;
    let mut user_end = None;
    let mut bracketed = None;
    let mut i = 0;
    loop {
        match s.get(i) {
            None | Some(b'/') => return Some(Spec::Local),
            Some(b':') => break,
            Some(b'@') => {
                user_end = Some(i);
                host_start = i + 1;
            }
            Some(b'[') => {
                if i != host_start {
                    return Some(Spec::Local);
                }
                let close = s[i..].iter().position(|&c| c == b']' || c == b'/').map(|p| p + i);
                let Some(close) = close.filter(|&c| s[c] == b']' && c > i + 1) else {
                    return Some(Spec::Local);
                };
                if !matches!(s.get(close + 1), None | Some(b'/' | b':')) {
                    return Some(Spec::Local);
                }
                bracketed = Some((i + 1, close));
                i = close;
            }
            Some(_) => {}
        }
        i += 1;
    }
    if s.get(i + 1) == Some(&b':') {
        return None;
    }
    let machine = match bracketed {
        Some((a, b)) => s[a..b].to_vec(),
        None => s[host_start..i].to_vec(),
    };
    Some(Spec::Remote {
        user: user_end.map(|u| s[..u].to_vec()),
        machine,
        path: s[i + 1..].to_vec(),
    })
}

/// rsync 3.2.7's split of its remote shell command (`do_cmd` in main.c).
fn split_rsh(cmd: &str) -> Result<Vec<String>, String> {
    let b = cmd.as_bytes();
    let mut args = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' ' {
            i += 1;
            continue;
        }
        let mut arg = Vec::new();
        let mut quote = None;
        while i < b.len() && (b[i] != b' ' || quote.is_some()) {
            let c = b[i];
            if c == b'\'' || c == b'"' {
                match quote {
                    None => {
                        quote = Some(c);
                        i += 1;
                        continue;
                    }
                    Some(q) if q == c => {
                        i += 1;
                        if b.get(i) != Some(&q) {
                            quote = None;
                            continue;
                        }
                    }
                    Some(_) => {}
                }
            }
            arg.push(b[i]);
            i += 1;
        }
        if let Some(q) = quote {
            return Err(format!("Missing trailing-{}", q as char));
        }
        args.push(String::from_utf8(arg).map_err(|e| e.to_string())?);
    }
    Ok(args)
}

fn output(data: &[u8], cuts: &[u16]) {
    let mut whole = RsyncProgress::default();
    whole.feed(data);
    let mut pieces = RsyncProgress::default();
    let mut at = 0;
    for &cut in cuts {
        let to = (at + usize::from(cut)).min(data.len());
        pieces.feed(&data[at..to]);
        sane(&pieces);
        at = to;
    }
    pieces.feed(&data[at..]);
    assert_eq!(pieces.stats(), whole.stats());
    assert_eq!(pieces.total_bounds(), whole.total_bounds());
    assert_eq!(pieces.wire(), whole.wire());
    sane(&whole);
}

fn sane(p: &RsyncProgress) {
    let s = p.stats();
    assert!(s.bytes >= 0 && (s.total_bytes == 0 || s.total_bytes >= s.bytes), "{s:?}");
    assert!(s.speed.is_finite() && s.speed >= 0.0, "{s:?}");
    assert!(s.eta.is_none_or(|e| e >= 0.0), "{s:?}");
    assert!(s.transferring.len() <= 1);
    assert!(s.transferring.iter().all(|t| 0 <= t.bytes && t.bytes <= t.size), "{s:?}");
    let (low, high) = p.total_bounds();
    assert!(high.is_none_or(|h| low <= h), "{low} {high:?}");
}

/// As rsync prints a name in the C locale: `\#ooo` for each byte it would not print, and
/// for a `\` before a `#`.
fn escape(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, &c) in name.iter().enumerate() {
        let printable =
            (0x20..0x7f).contains(&c) && !(c == b'\\' && name.get(i + 1) == Some(&b'#'));
        if printable || c == b'\t' {
            out.push(c);
        } else {
            out.extend(format!("\\#{c:03o}").bytes());
        }
    }
    out
}

fn job(total: u64, steps: &[(u32, Option<Vec<u8>>)]) {
    // Up to 64 TiB, where rsync's floating-point percentage is still the exact one.
    let total = total % (1 << 46) + 1;
    let mut p = RsyncProgress::default();
    let mut bytes = 0u64;
    for (step, name) in steps {
        if let Some(name) = name {
            // rsync ends the progress line it is on before anything else.
            let mut line = b"\nrse-file:12 ".to_vec();
            line.extend(escape(name));
            line.push(b'\n');
            p.feed(&line);
            let s = p.stats();
            let shown = String::from_utf8_lossy(name);
            if name.ends_with(b"/") {
                assert!(s.transferring.is_empty());
            } else {
                assert_eq!(s.transferring.first().map(|t| t.name.as_str()), Some(&*shown));
            }
        }
        bytes = (bytes + u64::from(*step)).min(total);
        let percent = if bytes == total { 100 } else { bytes * 100 / total };
        p.feed(format!("\r{bytes:>15} {percent:>3}%    1.00MB/s    0:00:01  ").as_bytes());
        let (low, high) = p.total_bounds();
        assert!(low <= total && high.is_none_or(|h| total <= h), "{total}: {low} {high:?}");
        assert_eq!(p.stats().bytes, i64::try_from(bytes).unwrap());
        sane(&p);
    }
    p.feed(format!("\r{total:>15} 100%    1.00MB/s    0:00:01 (xfr#1, to-chk=0/1)\n").as_bytes());
    assert_eq!(p.stats().total_bytes, i64::try_from(total).unwrap());
    p.feed(b"\nsent 12 bytes  received 3 bytes  15.00 bytes/sec\n");
    assert_eq!(p.wire(), Some((12, 3)));
}
