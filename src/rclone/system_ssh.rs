//! sftp through the system ssh by default, with one login per server.
//!
//! [`SystemSsh`] wraps a backend and rewrites the fs strings of rc calls. An sftp fs that
//! would use rclone's built-in ssh (`host=`, `user=`, … and no `ssh=`) gets an `ssh=`
//! command, so rclone runs the system `ssh` and `~/.ssh/config` applies. Before the
//! first use, rext opens an OpenSSH master connection to the server; the ssh processes
//! rclone starts for its connections go through it, so the server sees one login
//! however many connections rclone opens.
//!
//! Logging in may need a password, a key passphrase or a one-time code. ssh asks for
//! them through `SSH_ASKPASS`, which is rext itself ([`askpass_main`]): it hands ssh's
//! question to the running rext over a socket. A password or passphrase kept in
//! rclone.conf answers it, then a password rext remembers ([`PasswordFile`]); anything
//! else is asked in the interface (see [`SystemSsh::set_prompter`]), which can offer to
//! remember a password. This needs OpenSSH 8.4 or newer.
//!
//! A server's host key never seen before is trusted only when the user says so: ssh's
//! question, with the fingerprint, is put to them ([`SshQuestion::confirm`]).
//!
//! A password or passphrase from rclone.conf or remembered goes only to ssh's own question
//! for this server's account or `key_file`, never to one a server words, which OpenSSH
//! marks with a `(user@host)` prefix, nor to a jump host's.
//!
//! rclone's built-in ssh stays in use when there is no `ssh` program, or when the remote's
//! settings have no ssh equivalent (an inline key, a proxy, …, or a password before
//! OpenSSH 8.4). When the master connection cannot be opened, the built-in ssh is used
//! too, but checking the host key against ssh's `known_hosts` file: by itself it checks
//! none, so an attacker could otherwise block ssh to get past the check. A changed host
//! key or a cancelled login is an error instead, since falling back would only fail again.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::backend::{Backend, RpcError};
use super::obscure::reveal;
use super::ssh_passwords::PasswordFile;
use crate::location::remote_colon;

/// rc parameters that hold fs strings.
const FS_KEYS: [&str; 3] = ["fs", "srcFs", "dstFs"];
/// How long a master connection stays up after its last use.
const PERSIST: &str = "15m";
/// How often a master connection in use is checked to be alive.
const RECHECK: Duration = Duration::from_secs(10);
/// How long a login may take, not counting the time the user takes to answer: enough for
/// a slow server or a push to a phone, not for a server that stalls after its banner.
const LOGIN_TIME: Duration = Duration::from_secs(120);

/// sftp options the system ssh takes over. rclone still reads some of them when it runs
/// ssh (the key file, failing on one with a passphrase) and warns about others, so they
/// are left out of the fs sent to rclone, or blanked when rclone.conf sets them.
const SSH_TAKES_OVER: [&str; 10] = [
    "host",
    "user",
    "port",
    "pass",
    "key_file",
    "key_file_pass",
    "pubkey_file",
    "known_hosts_file",
    "ask_password",
    "key_use_agent",
];

/// sftp options the system ssh can honour only by asking through `SSH_ASKPASS`.
const NEED_ASKPASS: [&str; 3] = ["pass", "key_file_pass", "ask_password"];

/// sftp options that only the built-in ssh can honour. Pinned host keys are among them:
/// rclone ignores them once it runs ssh.
const BUILTIN_ONLY: [&str; 11] = [
    "host_keys",
    "pin_host_key",
    "key_pem",
    "pubkey",
    "socks_proxy",
    "http_proxy",
    "ciphers",
    "key_exchange",
    "macs",
    "host_key_algorithms",
    "use_insecure_cipher",
];

/// Options with secrets, left out of the log.
const SECRET_OPTIONS: [&str; 3] = ["pass", "key_file_pass", "key_pem"];

/// What OpenSSH's own sftp and scp set for a transfer, whatever ~/.ssh/config says: no
/// agent, X11 or port forwarding for the server to use, no local or remote command and
/// no terminal.
pub(crate) const TRANSFER_ONLY: [&str; 12] = [
    "-o",
    "ForwardAgent=no",
    "-o",
    "ForwardX11=no",
    "-o",
    "ClearAllForwardings=yes",
    "-o",
    "PermitLocalCommand=no",
    "-o",
    "RemoteCommand=none",
    "-o",
    "RequestTTY=no",
];

/// Asks the user ssh's question; `None` when they cancel.
pub type Prompter = dyn Fn(&SshQuestion) -> Option<SshAnswer> + Send + Sync;

/// A question from ssh for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshQuestion {
    /// ssh's words, such as `me@box's password:`.
    pub prompt: String,
    /// Offer to remember the answer as the login's password.
    pub can_remember: bool,
    /// Why the user is asked: why a remembered password did not spare the question, and
    /// what ssh said about the last answer. One line each.
    pub notice: Option<String>,
    /// A yes-or-no question of ssh's own, such as whether to trust a server's host key
    /// never seen before, with its fingerprint: answered `yes`, or cancelled.
    pub confirm: bool,
}

/// What the user answered to one of ssh's questions.
#[derive(Clone, PartialEq, Eq)]
pub struct SshAnswer {
    pub text: String,
    /// Remember it as the login's password once the login succeeds.
    pub remember: bool,
}

impl std::fmt::Debug for SshAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshAnswer").field("text", &"…").field("remember", &self.remember).finish()
    }
}

pub struct SystemSsh {
    inner: Arc<dyn Backend>,
    /// Whether there is an `ssh` program to use.
    have_ssh: bool,
    /// Where master sockets and ssh's logs live; `None` without an `ssh` program or a
    /// private directory for them.
    socket_dir: Option<PathBuf>,
    /// Why sftp cannot use the system ssh, to tell the user.
    warning: Option<String>,
    /// How long a login may take without the user's answers (`LOGIN_TIME`).
    login_time: Duration,
    /// The `SSH_ASKPASS` program; `None` before OpenSSH 8.4, which cannot be made to use
    /// it without a display.
    askpass: Option<PathBuf>,
    prompter: Mutex<Option<Arc<Prompter>>>,
    /// Remembered passwords; `None` when rext remembers none.
    passwords: Option<PasswordFile>,
    /// Why remembering a login's password failed last time, by its `password_key`.
    save_errors: Mutex<HashMap<String, String>>,
    /// How each fs name (`nas`, `:sftp,host=box`) is reached. Each has its own lock, so
    /// concurrent first uses of a server open one master connection.
    routes: Mutex<HashMap<String, Arc<Mutex<Route>>>>,
    next_log: AtomicU64,
}

// One per server name, so the size of `Login` does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Undecided,
    /// Not an sftp fs that rclone would reach with its built-in ssh.
    Untouched,
    Builtin,
    /// The built-in ssh after the system one failed, under this fs name, which has it check
    /// the host key.
    Checked(String),
    Ssh(Login),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Login {
    /// The ssh arguments naming the server and the account, e.g. `-p 2222 me@box`.
    dest: Vec<String>,
    /// `ControlPath` options when rext supplies the master socket; empty when the user's
    /// ssh config sets up connection sharing itself.
    control: Vec<String>,
    /// Whether rext opened the master, and so closes it at the end.
    opened: bool,
    checked: Option<Instant>,
    secrets: Secrets,
    /// Why the password or passphrase from rclone.conf cannot be used, if it cannot: told
    /// whenever ssh's question for it is put to the user instead.
    unusable: Unusable,
    /// The remote as the user named it, with secrets left out, for telling them why they
    /// are asked.
    remote: String,
    /// What a remembered password is kept under (`me@box`), from ssh's own settings.
    password_key: Option<String>,
    /// The fs name sent to rclone in place of the original one.
    fs_name: String,
    /// The accounts whose password questions are this login's, `me@box` as `ssh -G`
    /// resolves them: for the `HostName` and for a `HostKeyAlias`.
    accounts: Vec<String>,
    /// `key_file` as ssh names it when asking for its passphrase.
    key_file: Option<String>,
    /// How the built-in ssh checks host keys if the system ssh fails; `None` for the remote
    /// as it is, when it checks them its own way.
    fallback: Option<HostKeys>,
}

/// Clear-text answers from rclone.conf, then the remembered password, each offered to
/// ssh once.
#[derive(Clone, Default, PartialEq, Eq)]
struct Secrets {
    password: Option<String>,
    passphrase: Option<String>,
    remembered: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("passphrase", &self.passphrase.as_ref().map(|_| "…"))
            .field("remembered", &self.remembered.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Why the password or the passphrase from rclone.conf cannot be used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Unusable {
    password: Option<String>,
    passphrase: Option<String>,
}

/// How ssh's questions were answered during one login.
#[derive(Debug, Default)]
struct Asked {
    unused: Secrets,
    /// The remembered password was given; asked again, ssh refused it.
    gave_remembered: bool,
    /// The last answer was the remembered password.
    remembered_last: bool,
    /// The last password the user typed, if they chose to remember it.
    typed: Option<SshAnswer>,
    /// Why the remembered password could not be had.
    notice: Option<String>,
    /// ssh's log (`-E`), and how much of it has been read.
    log: Option<PathBuf>,
    log_read: usize,
    cancelled: bool,
}

impl Asked {
    /// What ssh logged since the last question, such as `Permission denied, please try
    /// again.`, one line each; notes about new host keys are left out.
    fn ssh_said(&mut self) -> Vec<String> {
        let Some(bytes) = self.log.as_ref().and_then(|l| fs::read(l).ok()) else {
            return Vec::new();
        };
        let new = String::from_utf8_lossy(bytes.get(self.log_read..).unwrap_or_default());
        self.log_read = bytes.len();
        new.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
            .map(|l| format!("ssh: {l}"))
            .collect()
    }
}

/// Why opening a master connection failed.
#[derive(Debug, PartialEq, Eq)]
enum OpenError {
    /// The server's host key does not match the known one; never fall back.
    HostKey(String),
    /// The user cancelled a question; falling back would only fail again.
    Cancelled,
    /// The login took longer than `login_time` without the user's answers.
    TimedOut,
    Other(String),
}

impl SystemSsh {
    /// Wraps `inner`. Master sockets go in `socket_dir`, which is created private. Without
    /// an `ssh` program every fs passes through unchanged; without a usable directory,
    /// sftp uses rclone's built-in ssh, checking host keys as ssh would.
    /// `askpass` is a program that runs [`askpass_main`] (rext itself).
    pub fn new(inner: Arc<dyn Backend>, socket_dir: &Path, askpass: Option<PathBuf>) -> Self {
        let version = Command::new("ssh").arg("-V").stdin(Stdio::null()).output().ok();
        let have_ssh = version.as_ref().is_some_and(|o| o.status.success());
        let askpass = askpass.filter(|_| {
            let v = version.as_ref().map(|o| String::from_utf8_lossy(&o.stderr).into_owned());
            v.as_deref().and_then(openssh_version).is_some_and(|v| v >= (8, 4))
        });
        let mut warning = None;
        let socket_dir = if have_ssh {
            match private_dir(socket_dir) {
                Ok(()) => Some(socket_dir.to_owned()),
                Err(e) => {
                    let why = tr!(
                        "ssh-socket-dir",
                        dir = socket_dir.display().to_string(),
                        error = e.to_string()
                    );
                    note(&why);
                    warning = Some(why);
                    None
                }
            }
        } else {
            note(&tr!("ssh-missing"));
            None
        };
        Self {
            inner,
            have_ssh,
            socket_dir,
            warning,
            login_time: LOGIN_TIME,
            askpass,
            prompter: Mutex::new(None),
            passwords: None,
            save_errors: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            next_log: AtomicU64::new(0),
        }
    }

    /// Sets how to ask the user what ssh wants to know. Until then, only answers kept in
    /// rclone.conf are given.
    pub fn set_prompter(&self, prompter: Box<Prompter>) {
        if let Ok(mut p) = self.prompter.lock() {
            *p = Some(Arc::from(prompter));
        }
    }

    /// How long a login may take, not counting the time the user takes to answer
    /// (`LOGIN_TIME` until set).
    pub fn set_login_time(&mut self, time: Duration) {
        self.login_time = time;
    }

    /// Remembers passwords in `file`, and answers ssh with them. Until then, none are.
    pub fn set_password_file(&mut self, file: PasswordFile) {
        self.passwords = Some(file);
    }

    /// Why sftp cannot use the system ssh as it should, to tell the user: there is no
    /// private directory for its sockets.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// The default socket directory: `rext` in the session's runtime directory, in which
    /// no other user can create anything, else `/tmp/rext-<uid>`, where one could first.
    /// Short either way, because socket paths are limited to about 100 bytes and a socket
    /// is named by a 40-character hash, a tag for its options and ssh's temporary suffix.
    pub fn default_socket_dir() -> PathBuf {
        runtime_dir(std::env::var_os("XDG_RUNTIME_DIR")).map_or_else(
            || PathBuf::from(format!("/tmp/rext-{}", current_uid())),
            |dir| dir.join("rext"),
        )
    }

    /// The fs string to send for `fs`, opening a master connection first if needed.
    fn route(&self, fs: &str) -> Result<Option<String>, RpcError> {
        let Some(colon) = remote_colon(fs) else { return Ok(None) };
        let name = &fs[..colon];
        let entry = {
            let mut routes = self.routes.lock().map_err(|_| poisoned())?;
            routes
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(Route::Undecided)))
                .clone()
        };
        let mut route = entry.lock().map_err(|_| poisoned())?;
        if *route == Route::Undecided {
            *route = self.decide(name)?;
        }
        if let Route::Ssh(login) = &mut *route
            && login.checked.is_none_or(|t| t.elapsed() > RECHECK)
            && !self.connect(name, login)?
        {
            let fallback = login.fallback();
            *route = fallback;
        }
        Ok(match &*route {
            Route::Ssh(Login { fs_name, .. }) | Route::Checked(fs_name) => {
                Some(format!("{fs_name}{}", &fs[colon..]))
            }
            _ => None,
        })
    }

    /// How to reach a fs name seen for the first time.
    fn decide(&self, name: &str) -> Result<Route, RpcError> {
        let Some(params) = self.sftp_params(name) else { return Ok(Route::Untouched) };
        if params.get("ssh").is_some_and(|v| !v.is_empty()) {
            return Ok(Route::Untouched);
        }
        if !self.have_ssh {
            return Ok(Route::Builtin);
        }
        let dest = match ssh_dest(&params, self.askpass.is_some()) {
            Ok(dest) => dest,
            Err(why) => {
                note(&format!("{}: using rclone's built-in ssh: {why}", redacted(name)));
                // Still checking host keys as ssh would, where ssh can be asked how.
                let fallback = ssh_target(&params)
                    .ok()
                    .and_then(|target| host_key_fallback(name, &params, &ssh_config(&target)));
                return Ok(fallback.map_or(Route::Builtin, |keys| keys.route()));
            }
        };
        let effective = ssh_config(&dest);
        let fallback = host_key_fallback(name, &params, &effective);
        let Some(dir) = &self.socket_dir else {
            // Nowhere private for ssh's sockets and logs: as when the system ssh fails.
            return Ok(fallback.map_or(Route::Builtin, |keys| keys.route()));
        };
        let own_sharing = effective.get("controlmaster").is_some_and(|v| v != "false")
            && effective.get("controlpath").is_some_and(|v| v != "none");
        let control = if own_sharing {
            Vec::new()
        } else {
            vec![
                "-o".to_owned(),
                format!("ControlPath={}/%C-{}", dir.display(), options_tag(&dest)),
            ]
        };
        let (secrets, unusable) = conf_secrets(name, &params);
        let password_key = login_key(&effective);
        let fs_name = ssh_fs_name(name, &params, &ssh_command(&control, &dest));
        let mut login = Login {
            dest,
            control,
            opened: false,
            checked: None,
            secrets,
            unusable,
            remote: redacted(name),
            password_key,
            fs_name,
            accounts: accounts(&effective),
            key_file: params.get("key_file").filter(|k| !k.is_empty()).map(|k| expand_home(k)),
            fallback,
        };
        Ok(if self.connect(name, &mut login)? { Route::Ssh(login) } else { login.fallback() })
    }

    /// Makes sure the master connection is up. `Ok(false)`: it cannot be, so use the
    /// built-in ssh.
    fn connect(&self, name: &str, login: &mut Login) -> Result<bool, RpcError> {
        if !alive(login) {
            match self.open(login) {
                Ok(()) => login.opened = true,
                Err(OpenError::HostKey(m)) => return Err(RpcError::new(500, m)),
                Err(OpenError::Cancelled) => return Err(RpcError::new(500, tr!("ssh-cancelled"))),
                Err(OpenError::TimedOut) => return Err(RpcError::new(500, tr!("ssh-timed-out"))),
                Err(OpenError::Other(m)) => {
                    note(&format!("{}: using rclone's built-in ssh: {m}", redacted(name)));
                    return Ok(false);
                }
            }
        }
        login.checked = Some(Instant::now());
        Ok(true)
    }

    /// The sftp options of a fs name, or `None` when it is not sftp.
    fn sftp_params(&self, name: &str) -> Option<BTreeMap<String, String>> {
        let mut parts = split_params(name);
        let base = parts.remove(0).0;
        let mut params = BTreeMap::new();
        if let Some(backend) = base.strip_prefix(':') {
            if backend != "sftp" {
                return None;
            }
        } else {
            let conf = self.inner.rpc("config/get", json!({ "name": base })).ok()?;
            let conf = conf.as_object()?;
            if conf.get("type").and_then(Value::as_str) != Some("sftp") {
                return None;
            }
            for (k, v) in conf {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                params.insert(k.clone(), v);
            }
        }
        params.extend(parts);
        Some(params)
    }

    /// Logs in once and leaves a master connection in the background.
    fn open(&self, login: &Login) -> Result<(), OpenError> {
        let dir = self.socket_dir.as_deref().ok_or(OpenError::Other("no ssh".to_owned()))?;
        // ssh goes to the background with our pipes open after logging in, so its
        // messages go to a file instead.
        let n = self.next_log.fetch_add(1, Ordering::SeqCst);
        let log = dir.join(format!("open-{}-{n}.log", std::process::id()));
        let mut args: Vec<String> = [
            "-E",
            &log.to_string_lossy(),
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ControlMaster=yes",
            "-o",
            &format!("ControlPersist={PERSIST}"),
        ]
        .iter()
        .chain(&TRANSFER_ONLY)
        .map(|s| (*s).to_owned())
        .collect();
        args.extend(login.control.iter().cloned());
        args.extend(["-N".to_owned(), "-f".to_owned()]);
        args.extend(login.dest.iter().cloned());
        let mut cmd = Command::new("ssh");
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut asked = Asked::default();
        let status = match &self.askpass {
            Some(program) => {
                let socket = dir.join(format!("ask-{}-{n}", std::process::id()));
                asked = self.asking(login);
                asked.log = Some(log.clone());
                let status = self.run_with_askpass(&mut cmd, program, &socket, login, &mut asked);
                let _ = fs::remove_file(&socket);
                status
            }
            None => self.run_until(cmd.arg("-o").arg("BatchMode=yes")),
        };
        let message = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_file(&log);
        match status {
            Ok(s) if s.success() => {
                self.keep_password(login, &asked);
                Ok(())
            }
            Ok(_) if asked.cancelled => Err(OpenError::Cancelled),
            Ok(_) => Err(classify(&message)),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(OpenError::TimedOut),
            Err(e) => Err(OpenError::Other(format!("cannot run ssh: {e}"))),
        }
    }

    /// How to start answering ssh's questions for `login`.
    fn asking(&self, login: &Login) -> Asked {
        let mut asked = Asked { unused: login.secrets.clone(), ..Asked::default() };
        match self.remembered(login) {
            Ok(pass) => asked.unused.remembered = pass,
            Err(e) => asked.notice = Some(e),
        }
        asked
    }

    /// The password remembered for `login`, if any, or why there is none to be had: the
    /// file cannot be read, or saving it failed last time.
    fn remembered(&self, login: &Login) -> Result<Option<String>, String> {
        let (Some(file), Some(key)) = (&self.passwords, login.password_key.as_deref()) else {
            return Ok(None);
        };
        let pass = file.get(key).map_err(|e| {
            note(&format!("{key}: {e}"));
            tr!("passwords-unusable", error = e.to_string())
        })?;
        if pass.is_none()
            && let Some(e) = self.save_errors.lock().ok().and_then(|m| m.get(key).cloned())
        {
            return Err(tr!("passwords-not-saved-before", error = e));
        }
        Ok(pass)
    }

    /// After a successful login: remembers the password the user typed if they asked to,
    /// or forgets a remembered one that ssh refused.
    fn keep_password(&self, login: &Login, asked: &Asked) {
        let (Some(file), Some(key)) = (&self.passwords, login.password_key.as_deref()) else {
            return;
        };
        let result = match &asked.typed {
            Some(answer) if answer.remember => file.set(key, &answer.text),
            Some(_) if asked.gave_remembered => file.remove(key).map(drop),
            _ => return,
        };
        let Ok(mut errors) = self.save_errors.lock() else { return };
        match result {
            Ok(()) => {
                errors.remove(key);
            }
            Err(e) => {
                note(&tr!(
                    "passwords-not-saved",
                    login = key,
                    path = file.path().display().to_string(),
                    error = e.to_string()
                ));
                errors.insert(key.to_owned(), e.to_string());
            }
        }
    }

    /// Runs ssh with `program` as its `SSH_ASKPASS`, answering its questions until it
    /// exits, and records how in `asked`. Returns its exit status.
    #[allow(unsafe_code)]
    fn run_with_askpass(
        &self,
        cmd: &mut Command,
        program: &Path,
        socket: &Path,
        login: &Login,
        asked: &mut Asked,
    ) -> std::io::Result<std::process::ExitStatus> {
        let listener = UnixListener::bind(socket).and_then(|l| {
            l.set_nonblocking(true)?;
            Ok(l)
        })?;
        cmd.env("SSH_ASKPASS", program)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_ENV, socket);
        // Without a controlling terminal, ssh cannot ask on the terminal rext draws on.
        let detach = || {
            let _ = rustix::process::setsid();
            Ok(())
        };
        // SAFETY: `detach` only calls setsid, which is async-signal-safe, so it may run
        // between fork and exec.
        unsafe { cmd.pre_exec(detach) };
        let mut child = cmd.spawn()?;
        // A server that stalls must not hold this remote, and a worker, for good.
        let mut deadline = Instant::now() + self.login_time;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                end_session(&mut child);
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let asking = Instant::now();
                    let answer = self.answer(stream, login, asked);
                    deadline += asking.elapsed();
                    if answer == Answer::Cancelled {
                        // The user gave this login up: end it, rather than let ssh, or a
                        // server asking again and again, put more questions to them.
                        asked.cancelled = true;
                        end_session(&mut child);
                        return child.wait();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Runs `cmd`, a login that asks nothing, to its end, or ends it after `login_time`.
    fn run_until(&self, cmd: &mut Command) -> std::io::Result<std::process::ExitStatus> {
        let mut child = cmd.spawn()?;
        let deadline = Instant::now() + self.login_time;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Answers one question from the askpass program.
    fn answer(&self, mut stream: UnixStream, login: &Login, asked: &mut Asked) -> Answer {
        let mut request = String::new();
        if stream.set_nonblocking(false).is_err() || stream.read_to_string(&mut request).is_err() {
            return Answer::Failed;
        }
        let (kind, prompt) = request.split_once('\n').unwrap_or(("", &request));
        let password = is_password_prompt(prompt);
        // Secrets only for ssh's own questions about this login, never a server's words.
        let owned = prompt_account(prompt).is_some_and(|a| login.accounts.iter().any(|x| x == a));
        let our_key = passphrase_key(prompt)
            .is_some_and(|k| login.key_file.as_deref().is_some_and(|f| shown_as(f, k)));
        let ssh_said = asked.ssh_said();
        let refused = std::mem::take(&mut asked.remembered_last);
        let unused = &mut asked.unused;
        let reply = if kind == "none" {
            // Only information, such as "confirm user presence": nothing to answer.
            Some(String::new())
        } else if our_key && unused.passphrase.is_some() {
            unused.passphrase.take()
        } else if password && owned && unused.password.is_some() {
            unused.password.take()
        } else if password && owned && unused.remembered.is_some() {
            asked.gave_remembered = true;
            asked.remembered_last = true;
            unused.remembered.take()
        } else {
            let Some(ask) = self.prompter.lock().ok().and_then(|p| p.clone()) else {
                // Nobody to ask yet: a failed login, not a cancelled one.
                let _ = stream.write_all(b"NO\n");
                return Answer::Failed;
            };
            // rext's own reasons first, then whatever ssh said about the last answer.
            // rclone.conf's answer, if it was given already, was wrong.
            let given =
                |conf: &Option<String>, left: &Option<String>| conf.is_some() && left.is_none();
            let remote = login.remote.as_str();
            let conf = if our_key {
                login.unusable.passphrase.clone().or_else(|| {
                    given(&login.secrets.passphrase, &unused.passphrase)
                        .then(|| tr!("ssh-passphrase-wrong", remote = remote))
                })
            } else if password && owned {
                login.unusable.password.clone().or_else(|| {
                    given(&login.secrets.password, &unused.password)
                        .then(|| tr!("ssh-password-refused", remote = remote))
                })
            } else {
                None
            };
            let own = if !password {
                None
            } else if refused {
                Some(tr!("passwords-refused"))
            } else {
                asked.notice.clone()
            };
            let notes: Vec<String> = conf.into_iter().chain(own).chain(ssh_said).collect();
            let notice = (!notes.is_empty()).then(|| notes.join("\n"));
            let question = SshQuestion {
                confirm: !password && is_confirmation(prompt),
                prompt: prompt.trim().to_owned(),
                can_remember: password
                    && owned
                    && self.passwords.is_some()
                    && login.password_key.is_some(),
                notice,
            };
            let answer = ask(&question);
            if password
                && owned
                && let Some(a) = &answer
            {
                asked.typed = Some(a.clone());
            }
            answer.map(|a| a.text)
        };
        let (bytes, answer) = match reply {
            Some(text) => (format!("OK\n{text}"), Answer::Given),
            None => ("NO\n".to_owned(), Answer::Cancelled),
        };
        let _ = stream.write_all(bytes.as_bytes());
        answer
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Given,
    Cancelled,
    Failed,
}

/// Set for ssh's askpass child: the socket to ask the running rext through.
const ASKPASS_ENV: &str = "REXT_ASKPASS";

/// Whether this process was started as ssh's askpass program.
pub fn askpass_socket() -> Option<std::ffi::OsString> {
    std::env::var_os(ASKPASS_ENV)
}

/// rext as ssh's `SSH_ASKPASS` program: passes the question (the first argument) to the
/// running rext through `socket` and prints the answer. Returns the exit code.
pub fn askpass_main(socket: &OsStr) -> i32 {
    let prompt = std::env::args_os().nth(1).map(|p| p.to_string_lossy().into_owned());
    let prompt = prompt.unwrap_or_default();
    let kind = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let reply = (|| {
        let mut stream = UnixStream::connect(socket)?;
        stream.write_all(format!("{kind}\n{prompt}").as_bytes())?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        Ok::<_, std::io::Error>(reply)
    })();
    match reply.as_deref().map(|r| r.strip_prefix("OK\n")) {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        _ => 1,
    }
}

impl Backend for SystemSsh {
    fn rpc(&self, method: &str, mut input: Value) -> Result<Value, RpcError> {
        for key in FS_KEYS {
            let Some(fs) = input.get(key).and_then(Value::as_str) else { continue };
            if let Some(routed) = self.route(fs)? {
                input[key] = Value::String(routed);
            }
        }
        self.inner.rpc(method, input)
    }

    fn finalize(&self) {
        self.close_masters();
        self.inner.finalize();
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

impl SystemSsh {
    /// Closes the master connections rext opened. Finalizing does this too.
    pub fn close_masters(&self) {
        let routes: Vec<_> =
            self.routes.lock().map(|r| r.values().cloned().collect()).unwrap_or_default();
        for route in routes {
            if let Ok(route) = route.lock()
                && let Route::Ssh(login) = &*route
                && login.opened
            {
                let mut args = login.control.clone();
                args.extend(["-O".to_owned(), "exit".to_owned()]);
                args.extend(login.dest.iter().cloned());
                let _ = quiet_ssh(&args);
            }
        }
    }
}

fn poisoned() -> RpcError {
    RpcError::new(0, tr!("internal-lock"))
}

/// A message for rclone.log (stderr goes there while the interface is open).
/// A short tag for the options in `dest`, so that remotes reaching one account with
/// different settings, such as their own `known_hosts` file, never share a master.
fn options_tag(dest: &[String]) -> String {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    let mut hasher = DefaultHasher::new();
    dest.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xffff_ffff)
}

/// How rclone's built-in ssh is to check host keys in place of the system ssh: against
/// ssh's `known_hosts` file, as ssh would; `None` when the remote checks them its own way,
/// with its own file or pinned keys.
fn host_key_fallback(
    name: &str,
    params: &BTreeMap<String, String>,
    effective: &HashMap<String, String>,
) -> Option<HostKeys> {
    let set = |k: &str| params.get(k).is_some_and(|v| !v.is_empty() && v != "false");
    if set("known_hosts_file") || set("host_keys") || set("pin_host_key") {
        return None;
    }
    let strict = effective.get("stricthostkeychecking").map(String::as_str);
    Some(HostKeys {
        fs: format!("{name},known_hosts_file={}", conn_quote(&known_hosts(effective))),
        lets_new_in: matches!(strict, Some("no" | "off" | "false" | "accept-new")),
        names: server_names(params, effective),
        files: known_hosts_files(effective),
    })
}

/// How the built-in ssh checks host keys in place of the system ssh, decided when it is
/// needed: a server that ssh lets in becomes known during the session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostKeys {
    /// The fs name checking host keys against ssh's `known_hosts` file.
    fs: String,
    /// ssh lets in a server it does not know (`StrictHostKeyChecking no` or `accept-new`).
    lets_new_in: bool,
    /// The names a `known_hosts` file may list the server under.
    names: Vec<String>,
    /// The `known_hosts` files ssh reads; `None` when one cannot be named here.
    files: Option<Vec<PathBuf>>,
}

impl HostKeys {
    fn route(&self) -> Route {
        if self.lets_new_in && self.new_server() {
            // As OpenSSH with those settings. rclone checks against a file or not at all.
            Route::Builtin
        } else {
            Route::Checked(self.fs.clone())
        }
    }

    /// Whether no file ssh reads lists the server under any of its names. Anything unsure
    /// (a file `ssh-keygen` cannot read, no `ssh-keygen`) counts as known, so that the key
    /// is checked rather than not.
    fn new_server(&self) -> bool {
        let Some(files) = &self.files else { return false };
        files.iter().filter(|file| file.exists()).all(|file| {
            self.names.iter().all(|name| {
                Command::new("ssh-keygen")
                    .arg("-F")
                    .arg(name)
                    .arg("-f")
                    .arg(file)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.code() == Some(1))
            })
        })
    }
}

/// The names a `known_hosts` file may list the server under, in lower case as ssh compares
/// them: as rclone dials it (`host`, or `[host]:port` off port 22), and as ssh resolves it
/// (`HostName`, `HostKeyAlias`).
fn server_names(
    params: &BTreeMap<String, String>,
    effective: &HashMap<String, String>,
) -> Vec<String> {
    let at = |host: &str, port: Option<&String>| {
        let host = host.to_lowercase();
        match port.filter(|p| !p.is_empty() && *p != "22") {
            Some(port) => format!("[{host}]:{port}"),
            None => host,
        }
    };
    let mut names = Vec::new();
    if let Some(host) = params.get("host").filter(|h| !h.is_empty()) {
        names.push(at(host, params.get("port")));
    }
    if let Some(host) = effective.get("hostname") {
        names.push(at(host, effective.get("port")));
    }
    if let Some(alias) = effective.get("hostkeyalias").filter(|a| *a != "none") {
        names.push(alias.to_lowercase());
    }
    names.sort();
    names.dedup();
    names
}

/// The `known_hosts` files ssh reads, or `None` when one cannot be named here: a path
/// with a `%` token, or a `KnownHostsCommand`.
fn known_hosts_files(effective: &HashMap<String, String>) -> Option<Vec<PathBuf>> {
    if effective.get("knownhostscommand").is_some_and(|c| c != "none") {
        return None;
    }
    let mut files = Vec::new();
    for key in ["userknownhostsfile", "globalknownhostsfile"] {
        for file in effective.get(key).into_iter().flat_map(|v| v.split_whitespace()) {
            if file.contains('%') {
                return None;
            }
            if file != "none" {
                files.push(PathBuf::from(expand_home(file)));
            }
        }
    }
    Some(files)
}

/// The remote's password and key passphrase in clear text. One that is not as `rclone
/// obscure` writes it is left out, with why, to tell the user when they are asked for it.
fn conf_secrets(name: &str, params: &BTreeMap<String, String>) -> (Secrets, Unusable) {
    let secret = |key: &str| {
        let Some(obscured) = params.get(key).filter(|v| !v.is_empty()) else {
            return (None, None);
        };
        match reveal(obscured) {
            Ok(clear) => (Some(clear), None),
            Err(e) => {
                note(&format!("{}: {key}: {e}", redacted(name)));
                (None, Some(tr!("ssh-secret-unusable", remote = redacted(name), key = key)))
            }
        }
    };
    let (password, password_why) = secret("pass");
    let (passphrase, passphrase_why) = secret("key_file_pass");
    let secrets = Secrets { password, passphrase, remembered: None };
    (secrets, Unusable { password: password_why, passphrase: passphrase_why })
}

/// `name` for the log, with obscured passwords, which anyone can reveal, left out.
fn redacted(name: &str) -> String {
    let mut parts = split_params(name).into_iter();
    let mut out = parts.next().map(|(base, _)| base).unwrap_or_default();
    for (key, value) in parts {
        let secret = SECRET_OPTIONS.contains(&key.as_str());
        let value = if secret { "…".to_owned() } else { conn_quote(&value) };
        let _ = write!(out, ",{key}={value}");
    }
    out
}

fn note(message: &str) {
    eprintln!("rext: sftp {message}");
}

/// Whether the master connection for `login` is up.
fn alive(login: &Login) -> bool {
    let mut args = login.control.clone();
    args.extend(["-O".to_owned(), "check".to_owned()]);
    args.extend(login.dest.iter().cloned());
    quiet_ssh(&args).is_ok_and(|s| s.success())
}

fn quiet_ssh(args: &[String]) -> std::io::Result<std::process::ExitStatus> {
    Command::new("ssh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}

/// The settings ssh would use for `dest` (`ssh -G`), keys in lower case.
fn ssh_config(dest: &[String]) -> HashMap<String, String> {
    let Ok(out) = Command::new("ssh").arg("-G").args(dest).stdin(Stdio::null()).output() else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_lowercase(), v.to_owned()))
        .collect()
}

/// What a remembered password is kept under: `user@host`, with `:port` off port 22, as
/// `ssh -G` resolves them (so a host alias and its address share one).
pub(crate) fn login_key(effective: &HashMap<String, String>) -> Option<String> {
    let user = effective.get("user").filter(|u| !u.is_empty())?;
    let host = effective.get("hostname").filter(|h| !h.is_empty())?;
    Some(match effective.get("port").map(String::as_str) {
        None | Some("22") => format!("{user}@{host}"),
        Some(port) => format!("{user}@{host}:{port}"),
    })
}

impl Login {
    fn fallback(&self) -> Route {
        self.fallback.as_ref().map_or(Route::Builtin, HostKeys::route)
    }
}

/// `me@box` for each name ssh may give the server in its questions: the `HostName`, and
/// the `HostKeyAlias` if one is set.
fn accounts(effective: &HashMap<String, String>) -> Vec<String> {
    let Some(user) = effective.get("user").filter(|u| !u.is_empty()) else { return Vec::new() };
    ["hostname", "hostkeyalias"]
        .iter()
        .filter_map(|k| effective.get(*k))
        .filter(|h| !h.is_empty() && *h != "none")
        .map(|h| format!("{user}@{h}"))
        .collect()
}

/// Whether `prompt` asks for the account's password: ssh's `me@box's password:`, or
/// `(me@box) Password:` from the server's keyboard-interactive login. Not a one-time
/// password or another code, which a stored password cannot answer and which must not
/// be remembered as one.
fn is_password_prompt(prompt: &str) -> bool {
    let lower = prompt.trim().to_lowercase();
    let text = lower.strip_prefix('(').and_then(|r| r.split_once(") ")).map_or(&*lower, |(_, t)| t);
    text == "password:" || text.ends_with("'s password:")
}

/// Whose question `prompt` is: `me@box` from ssh's `me@box's password:`, or from the
/// `(me@box) ` OpenSSH 8.4 puts in front of every question a server words.
pub(crate) fn prompt_account(prompt: &str) -> Option<&str> {
    match prompt.strip_prefix('(') {
        Some(rest) => rest.split_once(") ").map(|(account, _)| account),
        None => prompt.trim_end().strip_suffix("'s password:"),
    }
}

/// Whether `prompt` is ssh's own yes-or-no question, such as `The authenticity of host …
/// Are you sure you want to continue connecting (yes/no/[fingerprint])?`. A server's words
/// start with `(user@host)` and never are.
pub(crate) fn is_confirmation(prompt: &str) -> bool {
    let end = prompt.trim_end();
    !prompt.starts_with('(')
        && (end.ends_with("(yes/no/[fingerprint])?") || end.ends_with("(yes/no)?"))
}

/// The key file in ssh's own `Enter passphrase for key '/k':`; never in a server's words,
/// which start with `(`.
pub(crate) fn passphrase_key(prompt: &str) -> Option<&str> {
    prompt.strip_prefix("Enter passphrase for key '")?.trim_end().strip_suffix("':")
}

/// Whether ssh names key `file` as `shown`, which it cuts at 100 bytes.
fn shown_as(file: &str, shown: &str) -> bool {
    file == shown || (shown.len() == 100 && file.starts_with(shown))
}

/// `~/x` with the home directory in place of `~`, as ssh reads `-i`.
fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), crate::config::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => path.to_owned(),
    }
}

/// The user's `known_hosts` file as ssh would use it, for rclone's `known_hosts_file`.
fn known_hosts(effective: &HashMap<String, String>) -> String {
    let first = |key: &str| {
        effective.get(key).and_then(|v| v.split_whitespace().next()).filter(|f| !f.contains('%'))
    };
    match first("userknownhostsfile") {
        // ssh then reads only its global file; rclone would take `none` as checking nothing,
        // so an empty file stands in when there is no global one either.
        Some("none") => match first("globalknownhostsfile") {
            Some(file) if file != "none" => expand_home(file),
            _ => "/dev/null".to_owned(),
        },
        Some(file) => expand_home(file),
        None => expand_home("~/.ssh/known_hosts"),
    }
}

/// `OpenSSH_9.6p1 …` → (9, 6).
pub(crate) fn openssh_version(banner: &str) -> Option<(u32, u32)> {
    let v = banner.trim().strip_prefix("OpenSSH_")?;
    let (major, rest) = v.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The ssh arguments for rclone's sftp options, or why there are none. Passwords and
/// passphrases need `askpass`.
pub(crate) fn ssh_dest(
    params: &BTreeMap<String, String>,
    askpass: bool,
) -> Result<Vec<String>, String> {
    let get = |k: &str| params.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let set = |k: &str| get(k).is_some_and(|v| v != "false");
    if let Some(k) = BUILTIN_ONLY.iter().find(|k| set(k)) {
        return Err(format!("{k} is set, which the system ssh cannot use"));
    }
    if !askpass && let Some(k) = NEED_ASKPASS.iter().find(|k| set(k)) {
        return Err(format!("{k} is set, which needs OpenSSH 8.4 or newer"));
    }
    ssh_target(params)
}

/// The ssh arguments naming the server and the account, whether or not the system ssh
/// can do all the remote asks; or why none can be passed to ssh safely.
fn ssh_target(params: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let get = |k: &str| params.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let host = get("host").ok_or("no host")?;
    // ssh would read a leading `-` as an option, such as -oProxyCommand=…, not a name.
    if host.starts_with('-') || get("user").is_some_and(|u| u.starts_with('-')) {
        return Err("the host or user starts with -".to_owned());
    }
    // A line break would end rclone's reading of the ssh command early.
    let fields = ["host", "user", "port", "key_file", "pubkey_file", "known_hosts_file"];
    if let Some(k) = fields.iter().find(|k| get(k).is_some_and(|v| v.contains(char::is_control))) {
        return Err(format!("{k} holds a control character"));
    }
    let mut args = Vec::new();
    if let Some(port) = get("port") {
        args.extend(["-p".to_owned(), port.to_owned()]);
    }
    if let Some(key) = get("key_file") {
        // Offer only this key, as rclone does: trying others counts as failed logins.
        args.extend([
            "-i".to_owned(),
            key.to_owned(),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
        ]);
    }
    if let Some(cert) = get("pubkey_file") {
        args.extend(["-o".to_owned(), format!("CertificateFile={cert}")]);
    }
    if let Some(known) = get("known_hosts_file") {
        args.extend(["-o".to_owned(), format!("UserKnownHostsFile={known}")]);
    }
    args.push(match get("user") {
        Some(user) => format!("{user}@{host}"),
        None => host.to_owned(),
    });
    Ok(args)
}

/// The command rclone runs for each connection, through the master and never asking, as
/// the value of rclone's space-separated `ssh` option.
pub(crate) fn ssh_command(control: &[String], dest: &[String]) -> String {
    let mut cmd = vec!["ssh".to_owned(), "-o".to_owned(), "BatchMode=yes".to_owned()];
    cmd.extend(TRANSFER_ONLY.map(str::to_owned));
    if !control.is_empty() {
        cmd.extend(["-o".to_owned(), "ControlMaster=no".to_owned()]);
        cmd.extend(control.iter().cloned());
    }
    cmd.extend(dest.iter().cloned());
    let cmd: Vec<String> = cmd.iter().map(|a| space_sep_quote(a)).collect();
    cmd.join(" ")
}

fn classify(message: &str) -> OpenError {
    let lower = message.to_lowercase();
    // ssh's own reason comes last, after warnings such as a newly added host key.
    let last = message.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("ssh failed").trim();
    if lower.contains("remote host identification has changed")
        || lower.contains("host key verification failed")
    {
        OpenError::HostKey(tr!("ssh-host-key", reason = last))
    } else {
        OpenError::Other(last.to_owned())
    }
}

/// `name,k=v,k='v,w'` → `[(name, ""), (k, v), (k, "v,w")]`, unquoting values.
pub(crate) fn split_params(name: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                if chars.peek() == Some(&q) {
                    cur.push(q);
                    chars.next();
                } else {
                    quote = None;
                }
            }
            (None, '\'' | '"') if cur.ends_with('=') => quote = Some(c),
            (None, ',') => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .enumerate()
        .map(|(i, p)| match p.split_once('=') {
            Some((k, v)) if i > 0 => (k.to_owned(), v.to_owned()),
            _ => (p, String::new()),
        })
        .collect()
}

/// The fs name to send in place of `name` once the system ssh takes over: its other
/// options, the ones ssh took over left out or blanked, and the `ssh` command.
pub(crate) fn ssh_fs_name(name: &str, params: &BTreeMap<String, String>, cmd: &str) -> String {
    let mut parts = split_params(name);
    let base = parts.remove(0).0;
    let mut out = base.clone();
    // An empty `ssh=` is replaced too: rclone must not see the option twice.
    let kept = |k: &str| k != "ssh" && !SSH_TAKES_OVER.contains(&k);
    for (key, value) in parts.iter().filter(|(k, _)| kept(k)) {
        let _ = write!(out, ",{key}={}", conn_quote(value));
    }
    if !base.starts_with(':') {
        for key in SSH_TAKES_OVER.iter().filter(|k| params.get(**k).is_some_and(|v| !v.is_empty()))
        {
            let blank =
                if matches!(*key, "ask_password" | "key_use_agent") { "false" } else { "''" };
            let _ = write!(out, ",{key}={blank}");
        }
    }
    let _ = write!(out, ",ssh={}", conn_quote(cmd));
    out
}

/// A connection-string value, quoted when empty or holding `,` `:` or a quote.
pub(crate) fn conn_quote(value: &str) -> String {
    if value.is_empty() || value.contains([',', ':', '\'', '"', ' ']) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_owned()
    }
}

/// One argument of rclone's space-separated `ssh` option, quoted CSV-style if needed.
pub(crate) fn space_sep_quote(arg: &str) -> String {
    if arg.contains([' ', '"']) {
        format!("\"{}\"", arg.replace('"', "\"\""))
    } else {
        arg.to_owned()
    }
}

/// Ends ssh, started in a session of its own, with what it started there, such as a
/// `ProxyCommand`; then waits for it.
fn end_session(child: &mut std::process::Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let _ = kill_process_group(Pid::from_child(child), Signal::KILL);
    let _ = child.kill();
    let _ = child.wait();
}

/// The user rext runs as.
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// `dir` if it is a runtime directory as XDG describes it, an absolute path to a
/// directory of this user that only they can use, and short enough for sockets in it.
fn runtime_dir(dir: Option<std::ffi::OsString>) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dir = PathBuf::from(dir?);
    let meta = fs::symlink_metadata(&dir).ok()?;
    let others = meta.mode() & 0o077;
    let private = meta.is_dir() && meta.uid() == current_uid() && others == 0;
    (dir.is_absolute() && private && dir.as_os_str().len() <= 32).then_some(dir)
}

/// Creates `dir` readable by this user only, or checks an existing one is.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != current_uid() {
        return Err(std::io::Error::other(tr!("ssh-dir-not-private")));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn options_become_ssh_arguments() {
        let p = params(&[
            ("type", "sftp"),
            ("host", "box"),
            ("user", "me"),
            ("port", "2222"),
            ("key_file", "~/.ssh/id_box"),
            ("known_hosts_file", "/k/known"),
            ("shell_type", "unix"),
        ]);
        assert_eq!(
            ssh_dest(&p, true).unwrap(),
            [
                "-p",
                "2222",
                "-i",
                "~/.ssh/id_box",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "UserKnownHostsFile=/k/known",
                "me@box"
            ]
        );
        assert_eq!(ssh_dest(&params(&[("host", "box")]), false).unwrap(), ["box"]);
    }

    #[test]
    fn some_options_need_askpass_or_the_builtin_ssh() {
        let pass = params(&[("host", "box"), ("pass", "OBSC")]);
        assert!(ssh_dest(&pass, true).is_ok());
        let err = ssh_dest(&pass, false).unwrap_err();
        assert!(err.contains("pass") && err.contains("8.4"), "{err}");
        assert!(ssh_dest(&params(&[("host", "box"), ("key_pem", "-----BEGIN")]), true).is_err());
        assert!(
            ssh_dest(&params(&[("host", "box"), ("use_insecure_cipher", "false")]), true).is_ok()
        );
        assert!(ssh_dest(&params(&[("user", "me")]), true).is_err());
    }

    #[test]
    fn a_host_or_user_is_never_an_option() {
        let evil = "-oProxyCommand=touch /tmp/x";
        assert!(ssh_dest(&params(&[("host", evil)]), true).is_err());
        assert!(ssh_dest(&params(&[("host", "box"), ("user", evil)]), true).is_err());
        let fields = ["host", "user", "port", "key_file", "pubkey_file", "known_hosts_file"];
        for field in fields {
            let p = params(&[("host", "box"), (field, "x\n-oProxyCommand=touch /tmp/x")]);
            assert!(ssh_dest(&p, true).is_err(), "{field}");
        }
        assert!(ssh_dest(&params(&[("host", "box"), ("user", "me")]), true).is_ok());
    }

    #[test]
    fn quoted_values_split_back_unchanged() {
        // Every value of up to 4 characters from the ones quoting cares about.
        let alphabet = ['a', ',', ':', '\'', '"', ' ', '=', '日'];
        let mut values = vec![String::new()];
        for len in 1..=4 {
            let start = values.len() - alphabet.len().pow(len - 1);
            let longer: Vec<String> = values[start..]
                .iter()
                .flat_map(|v| alphabet.iter().map(move |c| format!("{v}{c}")))
                .collect();
            values.extend(longer);
        }
        for v in values {
            let name = format!("box,k={},t=x", conn_quote(&v));
            assert_eq!(
                split_params(&name),
                [("box".into(), String::new()), ("k".into(), v.clone()), ("t".into(), "x".into())],
                "{v:?} as {name:?}"
            );
        }
    }

    #[test]
    fn openssh_versions() {
        assert_eq!(
            openssh_version("OpenSSH_9.6p1 Ubuntu-3ubuntu13.20, OpenSSL 3.0.13"),
            Some((9, 6))
        );
        assert_eq!(openssh_version("OpenSSH_8.4p1, LibreSSL 3.3.6"), Some((8, 4)));
        assert_eq!(openssh_version("OpenSSH_for_Windows_8.1p1"), None);
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let s = Secrets { password: Some("hunter2".to_owned()), ..Secrets::default() };
        assert!(!format!("{s:?}").contains("hunter2"));
        let a = SshAnswer { text: "hunter2".to_owned(), remember: true };
        assert!(!format!("{a:?}").contains("hunter2"));
    }

    /// Asks `ssh` the way the askpass program does; the reply, or `None` for a refusal.
    fn askpass(ssh: &SystemSsh, login: &Login, asked: &mut Asked, prompt: &str) -> Option<String> {
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        ours.write_all(format!("\n{prompt}").as_bytes()).unwrap();
        ours.shutdown(std::net::Shutdown::Write).unwrap();
        ssh.answer(theirs, login, asked);
        let mut reply = String::new();
        ours.read_to_string(&mut reply).unwrap();
        reply.strip_prefix("OK\n").map(str::to_owned)
    }

    type Questions = Arc<Mutex<Vec<SshQuestion>>>;
    type Answers = Arc<Mutex<Vec<SshAnswer>>>;

    /// Secrets go only to ssh's own questions about this login: not to a jump host's, nor
    /// to a server's words, which OpenSSH starts with `(user@host)`.
    #[test]
    fn secrets_only_answer_this_logins_questions() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, mut login, questions, answers) = remembering(&dir.path().join("p"));
        login.secrets = Secrets {
            password: Some("pw".to_owned()),
            passphrase: Some("phrase".to_owned()),
            remembered: None,
        };
        let given = |prompt: &str| {
            answers.lock().unwrap().push(answer("typed", true));
            let mut asked = ssh.asking(&login);
            let reply = askpass(&ssh, &login, &mut asked, prompt);
            let question = questions.lock().unwrap().pop();
            (reply.unwrap(), question.map(|q| q.can_remember), asked.typed.is_some())
        };
        assert_eq!(given("me@box's password: "), ("pw".to_owned(), None, false));
        assert_eq!(given("(me@box) Password: "), ("pw".to_owned(), None, false));
        assert_eq!(given("Enter passphrase for key '/k/id': "), ("phrase".to_owned(), None, false));
        // A jump host, or a server asking for a passphrase: the user decides, and what they
        // type is not kept for this login.
        assert_eq!(given("me@jump's password: "), ("typed".to_owned(), Some(false), false));
        assert_eq!(given("(me@jump) Password: "), ("typed".to_owned(), Some(false), false));
        let server = "(me@box) Enter passphrase for key '/k/id': ";
        assert_eq!(given(server), ("typed".to_owned(), Some(false), false));
        let other_key = "Enter passphrase for key '/k/other': ";
        assert_eq!(given(other_key), ("typed".to_owned(), Some(false), false));
        // The login's own server asking for something other than its password.
        assert_eq!(given("(me@box) Verification code: "), ("typed".to_owned(), Some(false), false));
        assert_eq!(given("(me@box) One-time password: "), ("typed".to_owned(), Some(false), false));
    }

    /// A remembered password, like one from rclone.conf, answers only the password question
    /// for the login's own account.
    #[test]
    fn remembered_passwords_answer_only_this_logins_password_question() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p");
        let (ssh, login, questions, answers) = remembering(&path);
        PasswordFile::new(path).set("me@box", "kept").unwrap();
        for prompt in
            ["(me@box) Verification code: ", "me@jump's password: ", "(me@jump) Password: "]
        {
            answers.lock().unwrap().push(answer("typed", false));
            let mut asked = ssh.asking(&login);
            assert_eq!(askpass(&ssh, &login, &mut asked, prompt).as_deref(), Some("typed"));
            assert!(questions.lock().unwrap().pop().is_some(), "{prompt}");
        }
        let mut asked = ssh.asking(&login);
        assert_eq!(askpass(&ssh, &login, &mut asked, PASSWORD).as_deref(), Some("kept"));
    }

    /// A password or passphrase in rclone.conf that rext cannot read is asked for instead,
    /// saying why, but only in ssh's own question for it.
    #[test]
    fn unreadable_secrets_are_asked_for_saying_why() {
        let params = BTreeMap::from([
            ("pass".to_owned(), "hunter2".to_owned()),
            ("key_file_pass".to_owned(), "3T9PgUymd9w-jZk2CoHnCElf".to_owned()),
        ]);
        let (secrets, unusable) = conf_secrets("nas", &params);
        assert_eq!((secrets.password, secrets.passphrase.as_deref()), (None, Some("pw")));
        assert_eq!(unusable.passphrase, None);
        let why = unusable.password.unwrap();
        assert!(why.contains("nas") && why.contains("pass"), "{why}");

        let dir = tempfile::tempdir().unwrap();
        let (ssh, mut login, questions, answers) = remembering(&dir.path().join("p"));
        login.unusable = Unusable {
            password: Some("no pass".to_owned()),
            passphrase: Some("no phrase".to_owned()),
        };
        let notice = |prompt: &str| {
            answers.lock().unwrap().push(answer("typed", false));
            askpass(&ssh, &login, &mut ssh.asking(&login), prompt);
            questions.lock().unwrap().pop().unwrap().notice
        };
        assert_eq!(notice(PASSWORD).as_deref(), Some("no pass"));
        assert_eq!(notice("Enter passphrase for key '/k/id': ").as_deref(), Some("no phrase"));
        assert_eq!(notice("me@jump's password: "), None);
        assert_eq!(notice("(me@box) Enter passphrase for key '/k/id': "), None);
    }

    /// When the password or passphrase from rclone.conf does not work, ssh asks again, and
    /// the user is told why they are asked.
    #[test]
    fn refused_conf_secrets_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, mut login, questions, answers) = remembering(&dir.path().join("p"));
        login.secrets = Secrets {
            password: Some("pw".to_owned()),
            passphrase: Some("phrase".to_owned()),
            remembered: None,
        };
        answers.lock().unwrap().extend([answer("typed", false), answer("typed", false)]);
        let notice = || questions.lock().unwrap().pop().unwrap().notice.unwrap_or_default();
        let mut asked = ssh.asking(&login);
        assert_eq!(askpass(&ssh, &login, &mut asked, PASSWORD).as_deref(), Some("pw"));
        assert_eq!(askpass(&ssh, &login, &mut asked, PASSWORD).as_deref(), Some("typed"));
        assert_eq!(notice(), "The server refused the pass set for nas.");
        let key = "Enter passphrase for key '/k/id': ";
        assert_eq!(askpass(&ssh, &login, &mut asked, key).as_deref(), Some("phrase"));
        assert_eq!(askpass(&ssh, &login, &mut asked, key).as_deref(), Some("typed"));
        assert_eq!(notice(), "The key_file_pass set for nas does not unlock the key.");
    }

    /// When the system ssh fails, rclone's built-in ssh checks the host key against ssh's
    /// own `known_hosts` file, unless ssh is set not to check or the remote names its own.
    #[test]
    fn falling_back_keeps_checking_host_keys() {
        let effective = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let set = effective(&[("userknownhostsfile", "/k/known /k/known2")]);
        assert_eq!(known_hosts(&set), "/k/known");
        // rclone would take `none` as checking nothing: ssh's global file, or no host.
        let global = effective(&[
            ("userknownhostsfile", "none"),
            ("globalknownhostsfile", "/etc/ssh/ssh_known_hosts /etc/ssh/ssh_known_hosts2"),
        ]);
        assert_eq!(known_hosts(&global), "/etc/ssh/ssh_known_hosts");
        let nothing =
            effective(&[("userknownhostsfile", "none"), ("globalknownhostsfile", "none")]);
        assert_eq!(known_hosts(&nothing), "/dev/null");
        let home = crate::config::home_dir().unwrap();
        let tokens = effective(&[("userknownhostsfile", "%d/.ssh/known_hosts")]);
        assert_eq!(known_hosts(&tokens), home.join(".ssh/known_hosts").to_string_lossy());
        // Each condition decides on its own: ssh checks host keys; the remote has no file.
        let route = |p: &[(&str, &str)], set: &HashMap<String, String>| {
            host_key_fallback("nas", &params(p), set).map_or(Route::Builtin, |keys| keys.route())
        };
        let known = effective(&[("userknownhostsfile", "/k/known")]);
        let checked = Route::Checked("nas,known_hosts_file=/k/known".to_owned());
        assert_eq!(route(&[], &known), checked);
        for v in ["ask", "yes"] {
            let set =
                effective(&[("userknownhostsfile", "/k/known"), ("stricthostkeychecking", v)]);
            assert_eq!(route(&[("host", "new")], &set), checked, "{v}");
        }
        assert_eq!(route(&[("known_hosts_file", "/k/own")], &known), Route::Builtin);
        assert_eq!(route(&[("known_hosts_file", "")], &known), checked);
        for pinned in [("host_keys", "ssh-ed25519 AAAA"), ("pin_host_key", "true")] {
            assert_eq!(route(&[pinned], &known), Route::Builtin, "{pinned:?}");
        }
        assert_eq!(route(&[("pin_host_key", "false")], &known), checked);

        // As OpenSSH with `no` and `accept-new`: a server listed in any file ssh reads, under
        // any of its names, must show its key; only one listed nowhere is let in.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOKPN4ktFECKotDTQ+I1QDP+C+6ta5hksp+Ix9pAeLNC";
        fs::write(&file, format!("box {key}\n[box]:2222 {key}\nreal.example {key}\n")).unwrap();
        let file = file.to_string_lossy().into_owned();
        let found = Route::Checked(format!("nas,known_hosts_file={file}"));
        let missing = dir.path().join("missing").to_string_lossy().into_owned();
        let have_keygen = Command::new("ssh-keygen").arg("-?").output().is_ok();
        for v in ["no", "accept-new"] {
            let set = effective(&[("userknownhostsfile", &file), ("stricthostkeychecking", v)]);
            assert_eq!(route(&[("host", "box")], &set), found, "{v}: a known one is checked");
            assert_eq!(route(&[("host", "BOX"), ("port", "2222")], &set), found, "{v}");
            let resolved = effective(&[
                ("userknownhostsfile", &missing),
                ("globalknownhostsfile", &file),
                ("stricthostkeychecking", v),
                ("hostname", "real.example"),
            ]);
            assert_eq!(
                route(&[("host", "alias")], &resolved),
                Route::Checked(format!("nas,known_hosts_file={missing}")),
                "{v}: under its HostName, in the global file"
            );
            let unsure =
                effective(&[("userknownhostsfile", "%d/kh"), ("stricthostkeychecking", v)]);
            assert!(matches!(route(&[("host", "new")], &unsure), Route::Checked(_)), "{v}: unsure");
            if have_keygen {
                assert_eq!(
                    route(&[("host", "new")], &set),
                    Route::Builtin,
                    "{v}: a new one is let in"
                );
                assert_eq!(
                    route(&[("host", "box"), ("port", "2200")], &set),
                    Route::Builtin,
                    "{v}"
                );
                let none =
                    effective(&[("userknownhostsfile", &missing), ("stricthostkeychecking", v)]);
                assert_eq!(route(&[("host", "box")], &none), Route::Builtin, "{v}: no file");
            }
        }
        // Hashed entries count too.
        if have_keygen
            && Command::new("ssh-keygen")
                .args(["-H", "-f", &file])
                .output()
                .is_ok_and(|o| o.status.success())
        {
            let set = effective(&[
                ("userknownhostsfile", &file),
                ("stricthostkeychecking", "accept-new"),
            ]);
            assert!(!fs::read_to_string(&file).unwrap().contains("box "), "hashed");
            assert_eq!(route(&[("host", "box")], &set), found);
        }
        let (_, mut login, _, _) = remembering(&dir.path().join("p"));
        assert_eq!(login.fallback(), Route::Builtin);
        login.fallback = host_key_fallback("nas", &params(&[]), &known);
        assert_eq!(login.fallback(), checked);
    }

    /// Sockets go in the session's runtime directory only when no one else can use it.
    #[test]
    fn sockets_go_where_only_this_user_can_create_them() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let fits = path.as_os_str().len() <= 40;
        assert_eq!(runtime_dir(Some(path.clone().into())), fits.then(|| path.clone()));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(runtime_dir(Some(path.clone().into())), None, "others can look in");
        assert_eq!(runtime_dir(Some("run/user/1000".into())), None, "relative");
        assert_eq!(runtime_dir(Some(path.join("missing").into())), None);
        assert_eq!(runtime_dir(None), None);
        let file = path.join("file");
        fs::write(&file, "").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(runtime_dir(Some(file.into())), None, "not a directory");
        let long = path.join("x".repeat(40));
        fs::create_dir(&long).unwrap();
        fs::set_permissions(&long, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(runtime_dir(Some(long.into())), None, "too long for socket paths");
    }

    /// Without a private directory for its sockets, sftp goes through rclone's built-in
    /// ssh as when the system ssh fails: still checking host keys, and saying why.
    #[test]
    fn no_socket_dir_keeps_checking_host_keys() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("taken");
        fs::write(&taken, "").unwrap();
        let fake = Arc::new(crate::rclone::FakeBackend::new(|m, input| match m {
            "config/get" if input["name"] == "nas" => {
                Ok(json!({"type": "sftp", "host": "box", "user": "me"}))
            }
            _ => Ok(json!({})),
        }));
        let ssh = SystemSsh::new(fake, &taken, None);
        if !ssh.have_ssh {
            eprintln!("skipped: ssh not installed");
            return;
        }
        assert!(ssh.socket_dir.is_none());
        assert!(ssh.warning().is_some_and(|w| w.contains("taken")), "{:?}", ssh.warning());
        let dest = ssh_dest(&params(&[("host", "box"), ("user", "me")]), false).unwrap();
        let checking = !ssh_config(&dest)
            .get("stricthostkeychecking")
            .is_some_and(|v| matches!(v.as_str(), "false" | "no" | "off"));
        match ssh.decide("nas").unwrap() {
            Route::Checked(fs) => assert!(fs.starts_with("nas,known_hosts_file="), "{fs}"),
            Route::Builtin => assert!(!checking, "ssh checks host keys, so must rclone"),
            other => panic!("{other:?}"),
        }
        // Options only rclone's built-in ssh honours: it still checks host keys, unless the
        // remote pins its own.
        for (fs, expect_checked) in [("nas,key_pem=KEY", checking), ("nas,host_keys=K", false)] {
            match ssh.decide(fs).unwrap() {
                Route::Checked(_) => assert!(expect_checked, "{fs}"),
                Route::Builtin => assert!(!expect_checked, "{fs}"),
                other => panic!("{fs}: {other:?}"),
            }
        }
    }

    /// A login that makes no progress, and asks nothing, is given up and its ssh ended.
    #[test]
    fn a_stalled_login_is_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ssh, login, _, _) = remembering(&dir.path().join("p"));
        ssh.login_time = Duration::from_millis(300);
        let mut stall = Command::new("sleep");
        stall.arg("30");
        let started = Instant::now();
        let socket = dir.path().join("ask.sock");
        let program = Path::new("/bin/false");
        let result =
            ssh.run_with_askpass(&mut stall, program, &socket, &login, &mut Asked::default());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    /// File transfers never forward the agent, X11 or ports, whatever ~/.ssh/config says,
    /// and remotes with different settings for one account get masters of their own.
    #[test]
    fn transfers_forward_nothing_and_settings_keep_masters_apart() {
        let cmd = ssh_command(&[], &["box".to_owned()]);
        for option in ["ForwardAgent=no", "ForwardX11=no", "ClearAllForwardings=yes"] {
            assert!(cmd.contains(option), "{cmd}");
        }
        let plain = ["box".to_owned()];
        let own = ["-o".to_owned(), "UserKnownHostsFile=/k/known".to_owned(), "box".to_owned()];
        assert_eq!(options_tag(&plain), options_tag(&plain.clone()));
        assert_ne!(options_tag(&plain), options_tag(&own));
        assert_eq!(options_tag(&own).len(), 8);
    }

    #[test]
    fn the_log_shows_no_passwords() {
        let name = ":sftp,host=box,pass=OBSC,key_file_pass='a,b',user=me,key_pem=KEY";
        assert_eq!(redacted(name), ":sftp,host=box,pass=…,key_file_pass=…,user=me,key_pem=…");
    }

    #[test]
    fn host_key_questions_are_confirmations() {
        let new_key = "The authenticity of host 'box (10.0.0.2)' can't be established.\n\
                       ED25519 key fingerprint is SHA256:abc.\n\
                       Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert!(is_confirmation(new_key));
        assert!(!is_confirmation(&format!("(me@box) {new_key}")), "a server's words");
        assert!(!is_confirmation("me@box's password: "));
    }

    #[test]
    fn whose_question() {
        assert_eq!(prompt_account("me@box's password: "), Some("me@box"));
        assert_eq!(prompt_account("(me@box) Verification code: "), Some("me@box"));
        assert_eq!(prompt_account("Password: "), None);
        assert_eq!(passphrase_key("Enter passphrase for key '/a b': "), Some("/a b"));
        assert_eq!(passphrase_key("(me@box) Enter passphrase for key '/k': "), None);
        let long = format!("/{}", "k".repeat(120));
        assert!(shown_as(&long, &long[..100]) && !shown_as(&long, &long[..99]));
    }

    /// A `SystemSsh` remembering passwords in `path`, with a user who answers from
    /// `Answers` in turn and whose questions go to `Questions`; and a login for `me@box`.
    fn remembering(path: &Path) -> (SystemSsh, Login, Questions, Answers) {
        let ssh = SystemSsh {
            inner: Arc::new(crate::rclone::FakeBackend::new(|_, _| Ok(json!({})))),
            have_ssh: false,
            socket_dir: None,
            warning: None,
            login_time: LOGIN_TIME,
            askpass: None,
            prompter: Mutex::new(None),
            passwords: Some(PasswordFile::new(path.to_owned())),
            save_errors: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            next_log: AtomicU64::new(0),
        };
        let questions = Questions::default();
        let answers = Answers::default();
        let (q, a) = (questions.clone(), answers.clone());
        ssh.set_prompter(Box::new(move |question| {
            q.lock().unwrap().push(question.clone());
            Some(a.lock().unwrap().remove(0))
        }));
        let login = Login {
            dest: vec!["box".to_owned()],
            control: Vec::new(),
            opened: false,
            checked: None,
            secrets: Secrets::default(),
            unusable: Unusable::default(),
            remote: "nas".to_owned(),
            password_key: Some("me@box".to_owned()),
            fs_name: String::new(),
            accounts: vec!["me@box".to_owned()],
            key_file: Some("/k/id".to_owned()),
            fallback: None,
        };
        (ssh, login, questions, answers)
    }

    fn answer(text: &str, remember: bool) -> SshAnswer {
        SshAnswer { text: text.to_owned(), remember }
    }

    const PASSWORD: &str = "me@box's password: ";

    #[test]
    fn passwords_are_remembered_after_a_login() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rext-passwords");
        let (ssh, login, questions, answers) = remembering(&path);
        let file = PasswordFile::new(path.clone());
        let asked_last = || questions.lock().unwrap().pop().unwrap();
        let password = PASSWORD;

        // Nothing remembered: asked, with nothing to explain, and remembered once the
        // login succeeds.
        answers.lock().unwrap().push(answer("first", true));
        let mut asked = ssh.asking(&login);
        assert_eq!(askpass(&ssh, &login, &mut asked, password).as_deref(), Some("first"));
        let expected = SshQuestion {
            confirm: false,
            prompt: "me@box's password:".to_owned(),
            can_remember: true,
            notice: None,
        };
        assert_eq!(asked_last(), expected);
        ssh.keep_password(&login, &asked);
        assert_eq!(file.get("me@box").unwrap().as_deref(), Some("first"));

        // Remembered: given without asking. Refused: asked again, saying so; a new one
        // chosen to be remembered replaces it.
        answers.lock().unwrap().push(answer("second", true));
        let mut asked = ssh.asking(&login);
        assert_eq!(askpass(&ssh, &login, &mut asked, password).as_deref(), Some("first"));
        assert!(questions.lock().unwrap().is_empty());
        assert_eq!(askpass(&ssh, &login, &mut asked, password).as_deref(), Some("second"));
        let notice = asked_last().notice.unwrap();
        assert!(notice.contains("refused"), "{notice}");
        ssh.keep_password(&login, &asked);
        assert_eq!(file.get("me@box").unwrap().as_deref(), Some("second"));

        // Refused, and the one that works is not to be remembered: the wrong one goes.
        answers.lock().unwrap().push(answer("third", false));
        let mut asked = ssh.asking(&login);
        askpass(&ssh, &login, &mut asked, password);
        askpass(&ssh, &login, &mut asked, password);
        ssh.keep_password(&login, &asked);
        assert_eq!(file.get("me@box").unwrap(), None);

        // Other questions are never offered for remembering, nor explained.
        answers.lock().unwrap().push(answer("123456", true));
        let mut asked = ssh.asking(&login);
        askpass(&ssh, &login, &mut asked, "Verification code: ");
        let code = asked_last();
        assert_eq!((code.can_remember, code.notice), (false, None));
        ssh.keep_password(&login, &asked);
        assert_eq!(file.get("me@box").unwrap(), None);

        // Saving fails: the next time the user is asked, they are told why.
        let mode = |m| fs::set_permissions(dir.path(), fs::Permissions::from_mode(m)).unwrap();
        mode(0o500);
        answers.lock().unwrap().push(answer("fourth", true));
        let mut asked = ssh.asking(&login);
        askpass(&ssh, &login, &mut asked, password);
        ssh.keep_password(&login, &asked);
        mode(0o700);
        answers.lock().unwrap().push(answer("fourth", true));
        let mut asked = ssh.asking(&login);
        askpass(&ssh, &login, &mut asked, password);
        let notice = asked_last().notice.unwrap();
        assert!(notice.contains("could not be remembered"), "{notice}");
        ssh.keep_password(&login, &asked);
        assert_eq!(ssh.asking(&login).notice, None, "saved this time");

        // A file others can read is not used, and the user is told so.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        answers.lock().unwrap().push(answer("fifth", false));
        let mut asked = ssh.asking(&login);
        assert_eq!(askpass(&ssh, &login, &mut asked, password).as_deref(), Some("fifth"));
        let notice = asked_last().notice.unwrap();
        assert!(notice.contains("chmod 600"), "{notice}");
    }

    #[test]
    fn what_ssh_says_tells_why_it_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rext-passwords");
        let (ssh, login, questions, answers) = remembering(&path);
        let asked_last = || questions.lock().unwrap().pop().unwrap();
        let log = dir.path().join("ssh.log");
        let added = "Warning: Permanently added 'box' (ED25519) to the list of known hosts.\n";
        fs::write(&log, added).unwrap();
        answers
            .lock()
            .unwrap()
            .extend(["wrong", "wrong again", "123456"].map(|a| answer(a, false)));
        let mut asked = ssh.asking(&login);
        asked.log = Some(log.clone());
        askpass(&ssh, &login, &mut asked, PASSWORD);
        assert_eq!(asked_last().notice, None, "a new host key is no reason");

        // After rext's own reason, if any; for any question; each line once.
        fs::write(&log, format!("{added}Permission denied, please try again.\n")).unwrap();
        asked.notice = Some("own reason".to_owned());
        askpass(&ssh, &login, &mut asked, PASSWORD);
        assert_eq!(
            asked_last().notice.as_deref(),
            Some("own reason\nssh: Permission denied, please try again.")
        );
        askpass(&ssh, &login, &mut asked, "Verification code: ");
        assert_eq!(asked_last().notice, None, "nothing new from ssh");
    }

    #[test]
    fn remembered_passwords_are_kept_per_resolved_login() {
        let conf = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let key = |pairs: &[(&str, &str)]| login_key(&conf(pairs));
        assert_eq!(
            key(&[("user", "me"), ("hostname", "10.0.0.5"), ("port", "22")]).as_deref(),
            Some("me@10.0.0.5")
        );
        assert_eq!(
            key(&[("user", "me"), ("hostname", "box"), ("port", "2222")]).as_deref(),
            Some("me@box:2222")
        );
        assert_eq!(key(&[]), None, "ssh -G failed");
    }

    #[test]
    fn connection_string_params() {
        assert_eq!(
            split_params(":sftp,host=box,ssh='ssh -o A=b,c',user=\"o\"\"k\""),
            [
                (":sftp".to_owned(), String::new()),
                ("host".to_owned(), "box".to_owned()),
                ("ssh".to_owned(), "ssh -o A=b,c".to_owned()),
                ("user".to_owned(), "o\"k".to_owned()),
            ]
        );
        assert_eq!(split_params("nas"), [("nas".to_owned(), String::new())]);
    }

    #[test]
    fn host_key_changes_are_not_fallen_back_from() {
        let changed = "@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n";
        assert!(matches!(classify(changed), OpenError::HostKey(_)));
        assert!(matches!(classify("Host key verification failed.\n"), OpenError::HostKey(_)));
        assert_eq!(
            classify("Warning: Permanently added 'box'.\nme@box: Permission denied (publickey).\n"),
            OpenError::Other("me@box: Permission denied (publickey).".to_owned())
        );
    }

    #[test]
    fn options_ssh_takes_over_leave_the_fs() {
        let conf = params(&[
            ("type", "sftp"),
            ("host", "box"),
            ("key_file", "/k"),
            ("md5sum_command", "md5"),
        ]);
        assert_eq!(
            ssh_fs_name("nas,shell_type=unix,user=x", &conf, "ssh box"),
            "nas,shell_type=unix,host='',key_file='',ssh='ssh box'"
        );
        let typed = params(&[("host", "box"), ("key_use_agent", "true"), ("shell_type", "unix")]);
        assert_eq!(
            ssh_fs_name(
                ":sftp,host=box,key_use_agent=true,shell_type=unix",
                &typed,
                "ssh -p 2 box"
            ),
            ":sftp,shell_type=unix,ssh='ssh -p 2 box'"
        );
        assert_eq!(
            ssh_fs_name(":sftp,host=box,ssh=''", &params(&[("host", "box")]), "ssh box"),
            ":sftp,ssh='ssh box'"
        );
    }

    #[test]
    fn quoting_for_rclone() {
        assert_eq!(space_sep_quote("ControlPath=/tmp/a b/%C"), "\"ControlPath=/tmp/a b/%C\"");
        assert_eq!(space_sep_quote("me@box"), "me@box");
    }

    /// Without an ssh program or socket directory: non-sftp and built-in-only fs strings
    /// pass through unchanged, and a remote's type is looked up once.
    #[test]
    fn untouched_fs_strings() {
        let fake = Arc::new(crate::rclone::FakeBackend::new(|m, input| match m {
            "config/get" if input["name"] == "nas" => Ok(json!({"type": "smb", "host": "nas"})),
            _ => Ok(json!({})),
        }));
        let ssh = SystemSsh {
            inner: fake.clone(),
            have_ssh: false,
            socket_dir: None,
            warning: None,
            login_time: LOGIN_TIME,
            askpass: None,
            prompter: Mutex::new(None),
            passwords: None,
            save_errors: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            next_log: AtomicU64::new(0),
        };
        for fs in ["/home", "nas:x", "nas:y", ":sftp,host=box:z", ":s3,env_auth=true:b"] {
            ssh.rpc("operations/list", json!({ "fs": fs })).unwrap();
        }
        let sent: Vec<_> =
            fake.calls_to("operations/list").into_iter().map(|c| c["fs"].clone()).collect();
        assert_eq!(sent, ["/home", "nas:x", "nas:y", ":sftp,host=box:z", ":s3,env_auth=true:b"]);
        assert_eq!(fake.calls_to("config/get").len(), 1);
    }
}
