//! sftp through the system OpenSSH: remotes with `ssh = ...` in rclone.conf, and remotes
//! that rse's `SystemSsh` moves from rclone's built-in ssh to the system one.
//! The server is a private `sshd` started as the current user on a free localhost port,
//! with its own host key and `authorized_keys` in a temporary directory, so no account
//! setup is needed. Skipped when sshd, sftp-server, ssh or ssh-keygen is missing.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;
use rse::app::with_hint;
use rse::event::Event;
use rse::exec::Exec;
use rse::jobs::{Effect, JobState, OpKind, Queue, Transfer};
use rse::location::Location;
use rse::rclone::ssh_passwords::PasswordFile;
use rse::rclone::system_ssh::{SshAnswer, SshQuestion, SystemSsh};
use rse::rclone::{Backend, Embedded, api};

/// The tests set rclone's global config path, so they take turns.
static RCLONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn keygen(dir: &Path) -> std::path::PathBuf {
    keygen_with(dir, "")
}

/// A key protected by `passphrase` (none when empty).
fn keygen_with(dir: &Path, passphrase: &str) -> std::path::PathBuf {
    let key = dir.join("id_test");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    key
}

fn ssh_cmd(key: &Path, port: u16, user: &str) -> String {
    format!(
        "ssh -F none -i {} -p {port} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=no \
         -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR {user}@127.0.0.1",
        key.display()
    )
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
    start_sshd_with(dir, client_key, "PasswordAuthentication no\n")
}

/// A private sshd with `auth` as its authentication settings.
fn start_sshd_with(dir: &Path, client_key: &Path, auth: &str) -> Option<(Sshd, u16)> {
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
             {auth}KbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n\
             PidFile {}\nSubsystem sftp {}\n",
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

fn sorted_names(backend: &dyn Backend, loc: &Location) -> Vec<String> {
    let mut names: Vec<String> =
        api::list(backend, &loc.fs_string(), "").unwrap().into_iter().map(|i| i.name).collect();
    names.sort();
    names
}

/// A directory for ssh's master sockets, whose paths may be no longer than about 100
/// bytes: under /tmp, not the temporary directory, which on macOS is a long path.
fn socket_work_dir() -> tempfile::TempDir {
    tempfile::tempdir_in("/tmp").unwrap()
}

#[test]
fn browse_and_copy_over_openssh() {
    let _turn = RCLONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("skipped: ssh or ssh-keygen not installed");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let served = work.path().join("served");
    fs::create_dir_all(served.join("photos")).unwrap();
    fs::write(served.join("notes.md"), "remote notes").unwrap();
    let key = keygen(work.path());
    let other_key = keygen(&{
        let d = work.path().join("other");
        fs::create_dir(&d).unwrap();
        d
    });

    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let backend: Arc<dyn Backend> = Arc::new(Embedded::new());

    let conf = work.path().join("rclone.conf");
    fs::write(
        &conf,
        format!(
            "[viassh]\ntype = sftp\nssh = {}\n\n[badkey]\ntype = sftp\nssh = {}\n",
            ssh_cmd(&key, port, &user),
            ssh_cmd(&other_key, port, &user)
        ),
    )
    .unwrap();
    api::set_config_path(backend.as_ref(), conf.to_str().unwrap()).unwrap();
    assert_eq!(api::listremotes(backend.as_ref()).unwrap(), ["badkey", "viassh"]);

    // Browse.
    let t = Instant::now();
    let root = served.to_str().unwrap().to_owned();
    let names = sorted_names(backend.as_ref(), &Location::new("viassh:", root.clone()));
    eprintln!("listed viassh: in {:?}", t.elapsed());
    assert_eq!(names, ["notes.md", "photos"]);

    // The same server typed as a connection string, with no rclone.conf entry.
    let typed = format!(":sftp,ssh='{}':{root}", ssh_cmd(&key, port, &user));
    let loc = Location::parse(&typed, Path::new("/"), Path::new("/")).unwrap();
    assert_eq!(loc.path, root);
    assert_eq!(sorted_names(backend.as_ref(), &loc), ["notes.md", "photos"]);

    // Copy a local file and a directory up, then a remote file down.
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("up.txt"), "uploaded").unwrap();
    fs::create_dir(local.path().join("dir")).unwrap();
    fs::write(local.path().join("dir/inner.txt"), "inner").unwrap();
    let remote = Location::new("viassh:", format!("{root}/photos"));
    let (tx, rx) = unbounded();
    let exec = Exec::spawn(backend.clone(), tx, 1, Duration::from_millis(50));
    let mut q = Queue::new(2);
    let mk = |kind, src: &Location, name: &str, is_dir, dst: &Location| Transfer {
        kind,
        src_dir: src.clone(),
        name: name.into(),
        is_dir,
        dst_dir: Some(dst.clone()),
        size: None,
        // As if the user chose to overwrite: no check of the target first.
        overwrite: true,
    };
    let here = Location::local(local.path());
    let effects = q.push(vec![
        mk(OpKind::Copy, &here, "up.txt", false, &remote),
        mk(OpKind::Copy, &here, "dir", true, &remote),
        mk(OpKind::Copy, &Location::new("viassh:", root.clone()), "notes.md", false, &here),
    ]);
    let run = |effects: Vec<Effect>| {
        for e in effects {
            if let Effect::Start { id, transfer, method, params } = e {
                exec.submit(id, *transfer, method, params);
            }
        }
    };
    run(effects);
    let deadline = Instant::now() + Duration::from_secs(60);
    while q.jobs.iter().any(|j| !j.state.is_finished()) {
        assert!(Instant::now() < deadline);
        if let Ok(Event::Jobs(s)) = rx.recv_timeout(Duration::from_millis(200)) {
            let effects = q.apply(s);
            run(effects);
        }
    }
    let states: Vec<_> = q.jobs.iter().map(|j| j.state.clone()).collect();
    assert_eq!(states, [JobState::Done, JobState::Done, JobState::Done]);
    assert_eq!(fs::read_to_string(served.join("photos/up.txt")).unwrap(), "uploaded");
    assert_eq!(fs::read_to_string(served.join("photos/dir/inner.txt")).unwrap(), "inner");
    assert_eq!(fs::read_to_string(local.path().join("notes.md")).unwrap(), "remote notes");

    // A key the server does not accept: a clear error, with the login hint attached.
    let err = api::list(backend.as_ref(), "badkey:", "").unwrap_err();
    let shown = with_hint(&err.message);
    eprintln!("bad key error: {shown}");
    assert!(shown.contains("non-interactive login"), "{shown}");

    exec.shutdown();
}

/// Closes the master connections at the end of a test, also when an assertion fails.
struct CloseMasters(Arc<SystemSsh>);

impl Drop for CloseMasters {
    fn drop(&mut self) {
        self.0.close_masters();
    }
}

/// rse itself, which ssh runs as its askpass program.
fn askpass() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_rse").into()
}

fn logins(dir: &Path) -> usize {
    fs::read_to_string(dir.join("sshd.log"))
        .unwrap_or_default()
        .matches("Accepted publickey")
        .count()
}

#[test]
fn builtin_sftp_remotes_share_one_system_ssh_login() {
    let _turn = RCLONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("skipped: ssh or ssh-keygen not installed");
        return;
    }
    let work = socket_work_dir();
    let served = work.path().join("served");
    for d in ["a", "b", "c", "d"] {
        fs::create_dir_all(served.join(d)).unwrap();
        fs::write(served.join(d).join("f.txt"), d).unwrap();
    }
    let key = keygen(work.path());
    let other = work.path().join("other");
    fs::create_dir(&other).unwrap();
    let other_key = keygen(&other);
    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let known = work.path().join("known_hosts");
    // A known_hosts entry for the server with some other key.
    let wrong_known = work.path().join("wrong_known_hosts");
    let wrong_key = fs::read_to_string(format!("{}.pub", other_key.display())).unwrap();
    fs::write(&wrong_known, format!("[127.0.0.1]:{port} {wrong_key}")).unwrap();
    let remote = |name: &str, key: &Path, known: &Path| {
        format!(
            "[{name}]\ntype = sftp\nhost = 127.0.0.1\nport = {port}\nuser = {user}\n\
             key_file = {}\nknown_hosts_file = {}\n\n",
            key.display(),
            known.display()
        )
    };
    let conf = work.path().join("rclone.conf");
    fs::write(
        &conf,
        remote("box", &key, &known)
            + &remote("badkey", &other_key, &known)
            + &remote("spoofed", &key, &wrong_known),
    )
    .unwrap();
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    api::set_config_path(embedded.as_ref(), conf.to_str().unwrap()).unwrap();
    let ssh = Arc::new(SystemSsh::new(embedded, &work.path().join("sockets"), Some(askpass())));
    let _close = CloseMasters(ssh.clone());
    // The server's key is new: the user is asked, with its fingerprint, and trusts it.
    let confirmed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = confirmed.clone();
    ssh.set_prompter(Box::new(move |question| {
        assert!(question.confirm, "only the host key is asked about: {question:?}");
        log.lock().unwrap().push(question.prompt.clone());
        Some(SshAnswer { text: "yes".to_owned(), remember: false })
    }));
    let root = served.to_str().unwrap().to_owned();

    // Before any master connection is up (a running one would be shared, as with any
    // ssh): a key the server refuses fails in the system ssh, then in rclone's built-in
    // one; a changed host key is an error without trying the built-in ssh.
    let err = api::list(ssh.as_ref(), "badkey:", "").unwrap_err();
    assert!(!err.message.contains("host key"), "{}", err.message);
    let err = api::list(ssh.as_ref(), "spoofed:", "").unwrap_err();
    assert!(err.message.contains("host key"), "{}", err.message);
    assert_eq!(logins(work.path()), 0);

    // Listings from several threads at once: rclone opens several connections.
    let threads: Vec<_> = ["a", "b", "c", "d"]
        .into_iter()
        .map(|d| {
            let (ssh, fs) = (ssh.clone(), format!("box:{root}/{d}"));
            std::thread::spawn(move || {
                api::list(ssh.as_ref(), &fs, "")
                    .map(|l| l.into_iter().map(|i| i.name).collect::<Vec<_>>())
            })
        })
        .collect();
    for t in threads {
        assert_eq!(t.join().unwrap().unwrap(), ["f.txt"]);
    }
    // A connection string for the same account on the same server shares the master.
    let typed = format!(
        ":sftp,host=127.0.0.1,port={port},user={user},key_file='{}',known_hosts_file='{}':{root}",
        key.display(),
        known.display()
    );
    assert_eq!(api::list(ssh.as_ref(), &typed, "").unwrap().len(), 4);
    assert_eq!(logins(work.path()), 1, "one login for all of it");
    // ssh may hash the host name, so only look for an entry.
    assert!(!fs::read_to_string(&known).unwrap().trim().is_empty(), "new host key accepted");
    let confirmed = confirmed.lock().unwrap().clone();
    assert_eq!(confirmed.len(), 1, "asked once, then known: {confirmed:?}");
    // `fingerprint is SHA256:` (OpenSSH 9.6), `fingerprint is: SHA256:` (macOS's).
    assert!(
        confirmed[0].contains("fingerprint") && confirmed[0].contains(" SHA256:"),
        "{confirmed:?}"
    );

    ssh.close_masters();
    let sockets: Vec<_> = fs::read_dir(work.path().join("sockets")).unwrap().collect();
    assert!(sockets.is_empty(), "master sockets left: {sockets:?}");
}

#[test]
fn passphrases_come_from_rclone_conf_or_the_user() {
    let _turn = RCLONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("skipped: ssh or ssh-keygen not installed");
        return;
    }
    let work = socket_work_dir();
    let served = work.path().join("served");
    fs::create_dir_all(&served).unwrap();
    fs::write(served.join("f.txt"), "x").unwrap();
    let key = keygen_with(work.path(), "open sesame");
    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    let stored = api::obscure(embedded.as_ref(), "open sesame").unwrap();
    let wrong = api::obscure(embedded.as_ref(), "not it").unwrap();
    let remote = |name: &str, extra: &str| {
        format!(
            "[{name}]\ntype = sftp\nhost = 127.0.0.1\nport = {port}\nuser = {user}\n\
             key_file = {}\nknown_hosts_file = {}\n{extra}\n",
            key.display(),
            work.path().join("known_hosts").display()
        )
    };
    let conf = work.path().join("rclone.conf");
    fs::write(
        &conf,
        remote("stored", &format!("key_file_pass = {stored}\n"))
            + &remote("asked", "")
            + &remote("unobscured", "key_file_pass = open sesame\n")
            + &remote("wrong", &format!("key_file_pass = {wrong}\n")),
    )
    .unwrap();
    api::set_config_path(embedded.as_ref(), conf.to_str().unwrap()).unwrap();
    let sockets = work.path().join("sockets");
    let root = served.to_str().unwrap().to_owned();
    let list = |ssh: &SystemSsh, remote: &str| {
        api::list(ssh, &format!("{remote}:{root}"), "").map(|l| l.len()).map_err(|e| e.message)
    };

    // The passphrase kept in rclone.conf: nobody is asked.
    let asked = Arc::new(std::sync::Mutex::new(Vec::<SshQuestion>::new()));
    let ssh = Arc::new(SystemSsh::new(embedded.clone(), &sockets, Some(askpass())));
    let _close = CloseMasters(ssh.clone());
    let log = asked.clone();
    ssh.set_prompter(Box::new(move |question| {
        if question.confirm {
            return Some(SshAnswer { text: "yes".to_owned(), remember: false });
        }
        log.lock().unwrap().push(question.clone());
        assert!(!question.can_remember, "only passwords are remembered");
        Some(SshAnswer { text: "open sesame".to_owned(), remember: false })
    }));
    assert_eq!(list(&ssh, "stored"), Ok(1));
    assert!(asked.lock().unwrap().is_empty());
    assert_eq!(logins(work.path()), 1);
    ssh.close_masters();

    // None kept: the user is asked, once, with nothing to explain.
    assert_eq!(list(&ssh, "asked"), Ok(1));
    let questions = asked.lock().unwrap().clone();
    assert_eq!(questions.len(), 1, "{questions:?}");
    assert!(questions[0].prompt.contains("passphrase"), "{questions:?}");
    assert_eq!(questions[0].notice, None, "{questions:?}");
    assert_eq!(logins(work.path()), 2);
    assert_eq!(fs::read_dir(&sockets).unwrap().count(), 1, "a master socket");
    ssh.close_masters();

    // Kept in plain text, which rse cannot use: the user is asked, and told why.
    assert_eq!(list(&ssh, "unobscured"), Ok(1));
    let questions = asked.lock().unwrap().clone();
    assert_eq!(questions.len(), 2, "{questions:?}");
    let notice = questions[1].notice.clone().unwrap_or_default();
    assert!(notice.contains("unobscured") && notice.contains("key_file_pass"), "{notice}");
    assert_eq!(logins(work.path()), 3);
    ssh.close_masters();

    // Kept, but wrong: ssh asks again, and the user is told why.
    assert_eq!(list(&ssh, "wrong"), Ok(1));
    let questions = asked.lock().unwrap().clone();
    assert_eq!(questions.len(), 3, "{questions:?}");
    let notice = questions[2].notice.clone().unwrap_or_default();
    assert!(notice.contains("wrong") && notice.contains("does not unlock"), "{notice}");
    assert_eq!(logins(work.path()), 4);
    ssh.close_masters();

    // Cancelled: an error, without trying rclone's built-in ssh.
    let ssh = Arc::new(SystemSsh::new(embedded, &sockets, Some(askpass())));
    let _close = CloseMasters(ssh.clone());
    ssh.set_prompter(Box::new(|_| None));
    let err = list(&ssh, "asked").unwrap_err();
    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(logins(work.path()), 4);
}

/// An sshd running as an ordinary user cannot check passwords, so every one is refused:
/// ssh asks again, and the dialog shows what ssh said. A failed login remembers nothing.
#[test]
fn ssh_says_why_it_asks_again() {
    let _turn = RCLONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("skipped: ssh or ssh-keygen not installed");
        return;
    }
    let work = socket_work_dir();
    let key = keygen(work.path());
    let auth = "PasswordAuthentication yes\nAuthenticationMethods password\n";
    let Some((_sshd, port)) = start_sshd_with(work.path(), &key, auth) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    let mut ssh = SystemSsh::new(embedded, &work.path().join("sockets"), Some(askpass()));
    let passwords = work.path().join("rse-passwords");
    ssh.set_password_file(PasswordFile::new(passwords.clone()));
    // The user takes longer to answer than a login may take: their time does not count.
    ssh.set_login_time(Duration::from_secs(1));
    let ssh = Arc::new(ssh);
    let _close = CloseMasters(ssh.clone());
    let asked = Arc::new(std::sync::Mutex::new(Vec::<SshQuestion>::new()));
    let log = asked.clone();
    ssh.set_prompter(Box::new(move |question| {
        if question.confirm {
            return Some(SshAnswer { text: "yes".to_owned(), remember: false });
        }
        log.lock().unwrap().push(question.clone());
        std::thread::sleep(Duration::from_millis(1500));
        Some(SshAnswer { text: "wrong".to_owned(), remember: true })
    }));
    let fs = format!(
        ":sftp,host=127.0.0.1,port={port},user={user},known_hosts_file='{}':/",
        work.path().join("known_hosts").display()
    );
    let err = api::list(ssh.as_ref(), &fs, "").unwrap_err();
    assert!(
        !err.message.contains("two minutes"),
        "not given up for the answers' time: {}",
        err.message
    );

    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "ssh's three tries: {asked:?}");
    assert!(asked.iter().all(|q| q.prompt.ends_with("password:") && q.can_remember), "{asked:?}");
    assert_eq!(asked[0].notice, None, "a new host key is no reason: {asked:?}");
    for q in &asked[1..] {
        assert_eq!(q.notice.as_deref(), Some("ssh: Permission denied, please try again."));
    }
    assert!(!passwords.exists(), "nothing remembered");
}

/// The terminal: the user's own login shell on the server, through the master connection
/// (no second login), on a clean screen in the pane's directory, following it.
#[test]
fn a_terminal_runs_the_login_shell_through_the_master() {
    use rse::terminal::{Session, ShellDir};
    let _turn = RCLONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !have("ssh") || !have("ssh-keygen") || !have("ps") {
        eprintln!("skipped: ssh, ssh-keygen or ps not installed");
        return;
    }
    let work = socket_work_dir();
    let served = work.path().join("served");
    fs::create_dir_all(served.join("it's here")).unwrap();
    let key = keygen(work.path());
    let Some((_sshd, port)) = start_sshd(work.path(), &key) else {
        eprintln!("skipped: no sshd or sftp-server");
        return;
    };
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_owned());
    let known = work.path().join("known_hosts");
    let conf = work.path().join("rclone.conf");
    fs::write(
        &conf,
        format!(
            "[box]\ntype = sftp\nhost = 127.0.0.1\nport = {port}\nuser = {user}\n\
             key_file = {}\nknown_hosts_file = {}\n",
            key.display(),
            known.display()
        ),
    )
    .unwrap();
    let embedded: Arc<dyn Backend> = Arc::new(Embedded::new());
    api::set_config_path(embedded.as_ref(), conf.to_str().unwrap()).unwrap();
    let ssh = Arc::new(SystemSsh::new(embedded, &work.path().join("sockets"), Some(askpass())));
    let _close = CloseMasters(ssh.clone());
    ssh.set_prompter(Box::new(|_| Some(SshAnswer { text: "yes".to_owned(), remember: false })));

    let root = served.to_str().unwrap().to_owned();
    let target = ssh.shell(&format!("box:{root}")).unwrap().expect("reached through ssh");
    assert_eq!(ssh.shell("/").unwrap(), None);
    let (tx, rx) = unbounded();
    let mut s =
        Session::open(1, target, ShellDir::Absolute(root.clone()), (10, 60), tx, Instant::now())
            .unwrap();
    let pump = |s: &mut Session, what: &str, done: &dyn Fn(&Session) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done(s) {
            assert!(Instant::now() < deadline, "{what}:\n{}", s.screen().contents());
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::TermOutput { bytes, .. }) => s.output(&bytes, Instant::now()),
                Ok(Event::TermIdle { idle, .. }) => {
                    assert_eq!(s.idle(idle, Instant::now()), None);
                }
                Ok(Event::TermClosed { .. }) => panic!("closed: {:?}", s.closed()),
                _ => s.tick(Instant::now()),
            }
        }
    };
    pump(&mut s, "shell shown", &|s| s.shown());
    // Settle: the prompt may come in pieces.
    let until = Instant::now() + Duration::from_millis(500);
    pump(&mut s, "", &|_| Instant::now() > until);
    let lines: Vec<String> =
        s.screen().contents().lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect();
    assert!(lines.len() <= 1, "only the prompt on a clean screen: {lines:?}");

    s.input(b"pwd\r");
    pump(&mut s, "starts in the directory", &|s| {
        s.screen().contents().contains(&format!("\n{root}\n"))
    });
    let sub = served.join("it's here").to_str().unwrap().to_owned();
    s.want(ShellDir::Absolute(sub.clone()), Instant::now());
    pump(&mut s, "cd typed", &|s| s.dir() == &ShellDir::Absolute(sub.clone()));
    s.input(b"pwd\r");
    pump(&mut s, "follows the pane", &|s| s.screen().contents().contains(&format!("\n{sub}\n")));
    assert_eq!(logins(work.path()), 1, "through the master connection");
    s.input(b"exit\r");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "not closed");
        if let Ok(Event::TermClosed { .. }) = rx.recv_timeout(Duration::from_millis(100)) {
            assert_eq!(s.closed(), None);
            break;
        }
    }
}
