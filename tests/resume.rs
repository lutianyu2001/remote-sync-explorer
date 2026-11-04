//! rse/copy against the real embedded rclone: files a stopped job left half done are
//! resumed, on the local disk and on an sftp server reached through the system OpenSSH.
//! Its own test binary because it sets rclone's global bandwidth limit, to keep
//! transfers slow enough to stop. The sftp test runs a private `sshd` as the current
//! user, as `tests/sftp_ssh.rs` does, and is skipped when sshd, sftp-server, ssh or
//! ssh-keygen is missing.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rse::rclone::system_ssh::{SshAnswer, SystemSsh};
use rse::rclone::{Backend, Embedded, api};
use serde_json::json;

const MIB: usize = 1 << 20;
/// How far back rse/copy resumes a partial file on sftp: the writes rclone's default
/// sftp settings keep in flight (64 of 32 KiB).
const SFTP_SLACK: i64 = 2 << 20;

/// The tests take turns: the bandwidth limit is global.
static TURN: Mutex<()> = Mutex::new(());

fn data(size: usize, seed: usize) -> Vec<u8> {
    (0..size).map(|i| u8::try_from((i * 31 + i / 4093 + seed) % 251).unwrap()).collect()
}

fn partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".rse-partial");
    name.into()
}

/// Sets the bandwidth limit, and lifts it when dropped.
struct Bwlimit<'a>(&'a dyn Backend);

impl<'a> Bwlimit<'a> {
    fn set(b: &'a dyn Backend, rate: &str) -> Self {
        b.rpc("core/bwlimit", json!({ "rate": rate })).unwrap();
        Self(b)
    }
}

impl Drop for Bwlimit<'_> {
    fn drop(&mut self) {
        let _ = self.0.rpc("core/bwlimit", json!({ "rate": "off" }));
    }
}

fn wait(b: &dyn Backend, id: i64) -> api::JobStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = api::job_status(b, id).unwrap();
        if status.finished {
            return status;
        }
        assert!(Instant::now() < deadline, "job {id} did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs one rse/copy job to the end; its status and stats.
fn run(b: &dyn Backend, params: serde_json::Value) -> (api::JobStatus, api::Stats) {
    let id = api::submit(b, api::COPY, params).unwrap();
    let job = wait(b, id);
    (job, api::stats(b, &api::job_group(id)).unwrap())
}

/// Starts an rse/copy job and stops it once `at` bytes have gone; its stats then.
fn stop_after(b: &dyn Backend, params: serde_json::Value, at: i64) -> api::Stats {
    let id = api::submit(b, api::COPY, params).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut stats;
    loop {
        stats = api::stats(b, &api::job_group(id)).unwrap();
        if stats.bytes >= at {
            break;
        }
        assert!(!api::job_status(b, id).unwrap().finished, "finished before it was stopped");
        assert!(Instant::now() < deadline, "no progress");
        std::thread::sleep(Duration::from_millis(20));
    }
    api::job_stop(b, id).unwrap();
    assert!(!wait(b, id).success, "stopped");
    stats
}

/// A directory for ssh's master sockets, whose paths may be no longer than about 100
/// bytes: under /tmp, not the temporary directory, which on macOS is a long path.
fn socket_work_dir() -> tempfile::TempDir {
    tempfile::tempdir_in("/tmp").unwrap()
}

#[test]
fn download_to_local_resumes_after_stop() {
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let b = Embedded::new();
    let work = tempfile::tempdir().unwrap();
    let content = data(16 * MIB, 1);
    fs::write(work.path().join("big"), &content).unwrap();
    let remote = ":memory:resume-download";
    let up = api::copy_params(work.path().to_str().unwrap(), "big", remote, "big", false, false);
    assert!(run(&b, up).0.success);

    let here = tempfile::tempdir().unwrap();
    let dst = here.path().to_str().unwrap();
    let target = here.path().join("big");
    let params = || api::copy_params(remote, "big", dst, "big", false, false);
    let limit = Bwlimit::set(&b, "4M");
    let stats = stop_after(&b, params(), 4 << 20);
    assert_eq!(stats.total_bytes, 16 << 20);
    assert_eq!(stats.transferring.len(), 1, "{stats:?}");
    drop(limit);
    let left = fs::read(partial(&target)).unwrap();
    assert!(left.len() >= 4 * MIB && left.len() < 16 * MIB, "{} bytes left", left.len());
    assert_eq!(left[..], content[..left.len()]);
    assert!(!target.exists());

    let (job, stats) = run(&b, params());
    assert!(job.success, "{}", job.error);
    assert_eq!(stats.bytes, i64::try_from(content.len() - left.len()).unwrap(), "the rest only");
    assert_eq!(fs::read(&target).unwrap(), content);
    assert!(!partial(&target).exists());
}

#[test]
fn mismatched_partial_restarts_and_identical_files_are_skipped() {
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let b = Embedded::new();
    let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let content = data(3 * MIB, 2);
    fs::write(src.path().join("f"), &content).unwrap();
    fs::write(partial(&dst.path().join("f")), data(MIB, 3)).unwrap();
    let params = || {
        api::copy_params(
            src.path().to_str().unwrap(),
            "f",
            dst.path().to_str().unwrap(),
            "f",
            false,
            false,
        )
    };

    let (job, stats) = run(&b, params());
    assert!(job.success, "{}", job.error);
    assert_eq!(stats.bytes, 3 << 20, "started over");
    assert_eq!(fs::read(dst.path().join("f")).unwrap(), content);

    let (job, stats) = run(&b, params());
    assert!(job.success, "{}", job.error);
    assert_eq!((stats.bytes, stats.transfers), (0, 0), "already there");
}

#[test]
fn resumable_answers_by_target() {
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let b = Embedded::new();
    for (dst, want) in [
        ("/tmp", true),
        (":sftp,ssh='ssh box':/srv", true),
        (":sftp,host=box:/srv", false),
        (":memory:bucket", false),
    ] {
        assert_eq!(api::resumable(&b, ":memory:bucket", dst), Ok(want), "{dst}");
    }
    assert!(api::resumable(&b, "/", "nosuchremote:").is_err());
}

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

fn start_sshd(dir: &Path, client_key: &Path) -> Option<(Sshd, u16)> {
    let sshd = Path::new("/usr/sbin/sshd");
    let sftp = SFTP_SERVERS.iter().map(Path::new).find(|p| p.exists())?;
    if !sshd.exists() {
        return None;
    }
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
    fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n\
             StrictModes no\nPidFile {}\nSubsystem sftp {}\n",
            host_key.display(),
            dir.join("authorized_keys").display(),
            dir.join("sshd.pid").display(),
            sftp.display()
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

fn logins(dir: &Path) -> usize {
    fs::read_to_string(dir.join("sshd.log"))
        .unwrap_or_default()
        .matches("Accepted publickey")
        .count()
}

/// Closes the master connections at the end of the test, also when an assertion fails.
struct CloseMasters(Arc<SystemSsh>);

impl Drop for CloseMasters {
    fn drop(&mut self) {
        self.0.close_masters();
    }
}

/// An sftp remote of rclone's built-in ssh, which rse routes through the system ssh and
/// its master connection: the upload resumes, over sessions of rse's own on that
/// connection.
#[test]
fn upload_to_sftp_over_system_ssh_resumes_after_stop() {
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("skipped: ssh or ssh-keygen not installed");
        return;
    }
    let work = socket_work_dir();
    let served = work.path().join("served");
    fs::create_dir_all(&served).unwrap();
    let key = keygen(work.path());
    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let conf = work.path().join("rclone.conf");
    fs::write(
        &conf,
        format!(
            "[box]\ntype = sftp\nhost = 127.0.0.1\nport = {port}\nuser = {user}\n\
             key_file = {}\nknown_hosts_file = {}\n",
            key.display(),
            work.path().join("known_hosts").display()
        ),
    )
    .unwrap();
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    api::set_config_path(embedded.as_ref(), conf.to_str().unwrap()).unwrap();
    // Without the system ssh, rclone's built-in one cannot be resumed.
    assert_eq!(api::resumable(embedded.as_ref(), "/", "box:"), Ok(false));
    let askpass: PathBuf = env!("CARGO_BIN_EXE_rse").into();
    let ssh = Arc::new(SystemSsh::new(embedded, &work.path().join("sockets"), Some(askpass)));
    let _close = CloseMasters(ssh.clone());
    ssh.set_prompter(Box::new(|question| {
        assert!(question.confirm, "only the new host key is asked about: {question:?}");
        Some(SshAnswer { text: "yes".to_owned(), remember: false })
    }));
    let b: &dyn Backend = ssh.as_ref();
    assert_eq!(api::resumable(b, "/", "box:"), Ok(true));

    let local = tempfile::tempdir().unwrap();
    let content = data(16 * MIB, 4);
    fs::write(local.path().join("big"), &content).unwrap();
    let dst = format!("box:{}", served.display());
    let params =
        || api::copy_params(local.path().to_str().unwrap(), "big", &dst, "up/big", false, false);
    let limit = Bwlimit::set(b, "4M");
    stop_after(b, params(), 6 << 20);
    drop(limit);
    let target = served.join("up/big");
    let left = fs::read(partial(&target)).unwrap();
    let left_len = i64::try_from(left.len()).unwrap();
    assert!(left_len > SFTP_SLACK && left.len() < 16 * MIB, "{} bytes left", left.len());
    assert_eq!(left[..], content[..left.len()]);
    assert!(!target.exists());

    let (job, stats) = run(b, params());
    assert!(job.success, "{}", job.error);
    let rest = i64::try_from(content.len()).unwrap() - left_len;
    assert!(
        stats.bytes >= rest && stats.bytes <= rest + SFTP_SLACK,
        "{} bytes sent for the {rest} left",
        stats.bytes
    );
    assert_eq!(fs::read(&target).unwrap(), content);
    assert!(!partial(&target).exists());
    assert_eq!(logins(work.path()), 1, "rse's sessions share rclone's login");
}
