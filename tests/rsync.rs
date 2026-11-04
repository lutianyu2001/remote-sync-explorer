//! Transfers with the system rsync through the master connection `SystemSsh` keeps: both
//! ways, stopped and picked up again, and moves.
//! The server is a private `sshd` started as the current user on a free localhost port,
//! with its own host key and `authorized_keys` in a temporary directory, as in
//! `tests/sftp_ssh.rs`. Skipped when sshd, ssh, ssh-keygen or rsync is missing.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use rse::config::EngineChoice;
use rse::engine::Engines;
use rse::event::Event;
use rse::exec::Exec;
use rse::jobs::{Engine, OpKind, Transfer};
use rse::location::Location;
use rse::rclone::api::Stats;
use rse::rclone::system_ssh::{SshAnswer, SystemSsh};
use rse::rclone::{Backend, Embedded, api};
use rse::rsync::{Op, PARTIAL_DIR, RsyncRemote, Running, Side, args};

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn keygen(dir: &Path) -> PathBuf {
    let key = dir.join("id_test");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    key
}

const SFTP_SERVERS: [&str; 3] = [
    "/usr/lib/openssh/sftp-server",
    "/usr/libexec/openssh/sftp-server",
    "/usr/libexec/sftp-server",
];

/// Kills the private sshd when the test ends, pass or fail.
struct Sshd(std::process::Child);

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private sshd; with sftp when there is an sftp-server.
fn start_sshd(dir: &Path, client_key: &Path) -> Option<(Sshd, u16)> {
    let sshd = Path::new("/usr/sbin/sshd");
    if !sshd.exists() {
        return None;
    }
    let sftp = SFTP_SERVERS.iter().map(Path::new).find(|p| p.exists());
    let host_key = dir.join("host_key");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&host_key)
        .output()
        .ok()?;
    assert!(out.status.success());
    fs::copy(format!("{}.pub", client_key.display()), dir.join("authorized_keys")).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let config = dir.join("sshd_config");
    let subsystem = sftp.map(|s| format!("Subsystem sftp {}\n", s.display())).unwrap_or_default();
    fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n\
             StrictModes no\nPidFile {}\n{subsystem}",
            host_key.display(),
            dir.join("authorized_keys").display(),
            dir.join("sshd.pid").display(),
        ),
    )
    .unwrap();
    let child = Command::new(sshd)
        .args(["-D", "-f"])
        .arg(&config)
        .arg("-E")
        .arg(dir.join("sshd.log"))
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let guard = Sshd(child);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "sshd did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Some((guard, port))
}

/// Closes the master connections at the end of a test, also when an assertion fails.
struct CloseMasters(Arc<SystemSsh>);

impl Drop for CloseMasters {
    fn drop(&mut self) {
        self.0.close_masters();
    }
}

fn logins(dir: &Path) -> usize {
    fs::read_to_string(dir.join("sshd.log"))
        .unwrap_or_default()
        .matches("Accepted publickey")
        .count()
}

/// Every file under `dir` and its content, without rsync's partial files.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut todo = vec![dir.to_owned()];
    while let Some(d) = todo.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == PARTIAL_DIR) {
                continue;
            }
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                files.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Runs rsync to its end: the last progress, the result, and the bytes it sent and received.
fn run(args: Vec<OsString>) -> (Stats, Result<(), String>, Option<(u64, u64)>) {
    let mut running = Running::spawn(args).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let (stats, Some(result)) = running.poll() {
            return (stats, result, running.wire());
        }
        assert!(Instant::now() < deadline, "rsync did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `args` going at `kbps` KB/s.
fn slowly(mut args: Vec<OsString>, kbps: u32) -> Vec<OsString> {
    let at = args.iter().position(|a| a == "--").unwrap();
    args.insert(at, format!("--bwlimit={kbps}").into());
    args
}

/// Starts rsync on `args`, stops it once it has done `bytes`, and checks it says so.
fn stop_after(args: Vec<OsString>, bytes: i64) {
    let mut running = Running::spawn(args).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (stats, result) = running.poll();
        assert!(result.is_none(), "finished before it was stopped: {result:?}");
        if stats.bytes >= bytes {
            break;
        }
        assert!(Instant::now() < deadline, "no progress: {stats:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    running.stop();
    let (_, result) = running.poll();
    assert!(result.is_some_and(|r| r.is_err()), "a stopped run fails");
}

/// Waits for `path` to hold at least `len` bytes.
fn wait_for(path: &Path, len: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fs::metadata(path).is_ok_and(|m| m.len() >= len) {
        assert!(Instant::now() < deadline, "no partial file at {}", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `to` written relative to directory `from`, both absolute.
fn relative(from: &Path, to: &Path) -> String {
    let up = from.components().count() - 1;
    format!("{}{}", "../".repeat(up), to.strip_prefix("/").unwrap().display())
}

/// The login directory of the user running the tests, as the passwd database says.
fn login_dir() -> Option<PathBuf> {
    let uid = rse::rclone::system_ssh::current_uid().to_string();
    let out = Command::new("getent").args(["passwd", &uid]).output().ok()?;
    let line = String::from_utf8(out.stdout).ok()?;
    line.trim().split(':').nth(5).map(PathBuf::from).filter(|p| p.is_absolute())
}

#[test]
fn rsync_through_the_master_connection() {
    if !have("ssh") || !have("ssh-keygen") || !rse::rsync::usable() {
        eprintln!("skipped: ssh, ssh-keygen or rsync 3.1 or newer not installed");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let key = keygen(work.path());
    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    let askpass = PathBuf::from(env!("CARGO_BIN_EXE_rse"));
    let ssh = Arc::new(SystemSsh::new(embedded, &work.path().join("sockets"), Some(askpass)));
    let _close = CloseMasters(ssh.clone());
    // The server's host key is new: trusted.
    ssh.set_prompter(Box::new(|question| {
        assert!(question.confirm, "only the host key is asked about: {question:?}");
        Some(SshAnswer { text: "yes".to_owned(), remember: false })
    }));
    let fs_name = format!(
        ":sftp,host=127.0.0.1,port={port},user={user},key_file='{}',known_hosts_file='{}':",
        key.display(),
        work.path().join("known_hosts").display()
    );

    // rsync is found through the master connection, which this opens.
    assert_eq!(ssh.rsync_remote("/").unwrap(), None);
    let remote = ssh.rsync_remote(&fs_name).unwrap().expect("the server has rsync");
    assert_eq!(remote.host, format!("{user}@127.0.0.1"));
    assert_eq!(remote.root, "");
    assert!(remote.rsh.contains("ControlPath=") && remote.rsh.contains(&format!("-p {port}")));
    assert_eq!(ssh.rsync_remote(&fs_name).unwrap(), Some(remote.clone()), "asked once");
    assert_eq!(logins(work.path()), 1);

    let server = work.path().join("server");
    let local = work.path().join("local");
    fs::create_dir_all(server.join("tree/sub")).unwrap();
    fs::create_dir_all(local.join("tree/sub")).unwrap();
    fs::create_dir_all(local.join("down")).unwrap();
    fs::create_dir_all(server.join("up")).unwrap();
    fs::write(local.join("one file.txt"), "local file").unwrap();
    fs::write(local.join("tree/a"), vec![1u8; 300_000]).unwrap();
    fs::write(local.join("tree/sub/b's"), "b").unwrap();
    fs::write(server.join("notes.md"), "server notes").unwrap();
    fs::write(server.join("tree/c"), vec![2u8; 200_000]).unwrap();
    fs::write(server.join("tree/sub/d"), "d").unwrap();

    copies_both_ways(&remote, &local, &server);
    stopped_transfers_pick_up_again(&remote, &local, &server);
    moves(&remote, &local, &server);
    relative_paths(&ssh, &fs_name, &remote, &local, &server);
    the_job_worker_chooses_rsync(&ssh, &fs_name, &local, &server);
    assert_eq!(logins(work.path()), 1, "every transfer through the one login");
}

/// `len` bytes that do not repeat, so that rsync finds no part of a file in another.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn here(dir: &Path) -> Side {
    Side::local(&Location::local(dir))
}

fn there(remote: &RsyncRemote, dir: &Path) -> Side {
    remote.side(dir.to_str().unwrap())
}

/// Local to the server and back, a file and a directory each.
fn copies_both_ways(remote: &RsyncRemote, local: &Path, server: &Path) {
    for name in ["one file.txt", "tree"] {
        let (stats, result, _) =
            run(args(Op::Copy, &here(local), &there(remote, &server.join("up")), name, false));
        assert_eq!(result, Ok(()), "{name}");
        assert!(stats.bytes > 0);
    }
    assert_eq!(fs::read_to_string(server.join("up/one file.txt")).unwrap(), "local file");
    assert_eq!(tree(&server.join("up/tree")), tree(&local.join("tree")));
    for name in ["notes.md", "tree"] {
        let (_, result, _) =
            run(args(Op::Copy, &there(remote, server), &here(&local.join("down")), name, true));
        assert_eq!(result, Ok(()), "{name}");
    }
    assert_eq!(fs::read_to_string(local.join("down/notes.md")).unwrap(), "server notes");
    assert_eq!(tree(&local.join("down/tree")), tree(&server.join("tree")));
}

/// A stopped upload leaves its partial file on the server, and the next run sends only the
/// rest; a stopped download, here.
fn stopped_transfers_pick_up_again(remote: &RsyncRemote, local: &Path, server: &Path) {
    let size = 8_000_000;
    let data = noise(usize::try_from(size).unwrap());
    fs::write(local.join("big"), &data).unwrap();
    let up = args(Op::Copy, &here(local), &there(remote, &server.join("up")), "big", false);
    stop_after(slowly(up.clone(), 2000), 6_000_000);
    wait_for(&server.join("up").join(PARTIAL_DIR).join("big"), 5_000_000);
    assert!(!server.join("up/big").exists());
    let (_, result, wire) = run(up);
    assert_eq!(result, Ok(()));
    let (sent, _) = wire.unwrap();
    eprintln!("the upload picked up again sent {sent} bytes of {size}");
    assert!(sent < size / 2, "sent {sent} of {size}");
    assert_eq!(fs::read(server.join("up/big")).unwrap(), data);
    assert!(!server.join("up").join(PARTIAL_DIR).exists());

    let down = args(
        Op::Copy,
        &there(remote, &server.join("up")),
        &here(&local.join("down")),
        "big",
        false,
    );
    stop_after(slowly(down.clone(), 2000), 6_000_000);
    wait_for(&local.join("down").join(PARTIAL_DIR).join("big"), 5_000_000);
    let (_, result, wire) = run(down);
    assert_eq!(result, Ok(()));
    let (_, received) = wire.unwrap();
    eprintln!("the download picked up again received {received} bytes of {size}");
    assert!(received < size / 2, "received {received} of {size}");
    assert_eq!(fs::read(local.join("down/big")).unwrap(), data);
}

/// A move removes the source files and leaves their directories.
fn moves(remote: &RsyncRemote, local: &Path, server: &Path) {
    let want = tree(&local.join("tree"));
    let (_, result, _) =
        run(args(Op::Move, &here(local), &there(remote, &server.join("moved")), "tree", false));
    assert_eq!(result, Ok(()));
    assert_eq!(tree(&server.join("moved/tree")), want);
    assert!(tree(&local.join("tree")).is_empty(), "files moved");
    assert!(local.join("tree/sub").is_dir(), "directories left");
    let (_, result, _) =
        run(args(Op::Move, &there(remote, server), &here(local), "notes.md", false));
    assert_eq!(result, Ok(()));
    assert!(!server.join("notes.md").exists());
    assert_eq!(fs::read_to_string(local.join("notes.md")).unwrap(), "server notes");
}

/// A relative path is in the login directory, for rsync as for rclone.
fn relative_paths(
    ssh: &SystemSsh,
    fs_name: &str,
    remote: &RsyncRemote,
    local: &Path,
    server: &Path,
) {
    if let Some(home) = login_dir()
        && fs::canonicalize(&home).ok() == Some(home.clone())
    {
        let rel = relative(&home, &fs::canonicalize(server).unwrap());
        let (_, result, _) = run(args(Op::Copy, &remote.side(&rel), &here(local), "tree", false));
        assert_eq!(result, Ok(()));
        assert_eq!(tree(&local.join("tree")), tree(&server.join("tree")));
        if SFTP_SERVERS.iter().any(|s| Path::new(s).exists()) {
            let listed = api::list(ssh, &format!("{fs_name}{rel}/tree"), "").unwrap();
            let mut names: Vec<_> = listed.into_iter().map(|i| i.name).collect();
            names.sort();
            assert_eq!(names, ["c", "sub"]);
        }
    }
}

/// The job worker copies to and from the server with rsync, and between two places on the
/// local disk with rclone, as `Engines` decides; `Engines::between` tells the same.
fn the_job_worker_chooses_rsync(ssh: &Arc<SystemSsh>, fs_name: &str, local: &Path, server: &Path) {
    let engines = Engines::new(Some(ssh.clone()), EngineChoice::Auto, false);
    let here = Location::local(local);
    let there = Location::new(fs_name, server.join("by-job").to_str().unwrap());
    fs::create_dir_all(server.join("by-job")).unwrap();
    assert_eq!(engines.between(&here, &there), Engine::Rsync);
    assert_eq!(engines.between(&there, &here), Engine::Rsync);
    assert_eq!(engines.between(&here, &Location::local(server)), Engine::Rclone);

    let (tx, rx) = unbounded();
    let exec = Exec::spawn_with(ssh.clone(), tx, 1, Duration::from_millis(50), engines);
    let transfer = Transfer {
        kind: OpKind::Copy,
        src_dir: here,
        name: "one file.txt".to_owned(),
        is_dir: false,
        dst_dir: Some(there),
        size: None,
        overwrite: true,
    };
    let (method, params) = transfer.rc_call().unwrap();
    exec.submit(1, transfer, method, params);
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut engine, mut finished) = (None, None);
    while finished.is_none() && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Event::JobStarted { engine: e, resumable, .. }) => {
                assert!(resumable);
                engine = Some(e);
            }
            Ok(Event::Jobs(snaps)) => finished.clone_from(&snaps[0].finished),
            _ => {}
        }
    }
    exec.shutdown();
    assert_eq!((engine, finished), (Some(Engine::Rsync), Some(Ok(()))));
    assert_eq!(fs::read_to_string(server.join("by-job/one file.txt")).unwrap(), "local file");
}
