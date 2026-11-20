//! Deletes, moves and copies inside one machine, carried out by that machine's own `rm`,
//! `mv` and `cp`: on the local disk, or on a server reached through the system ssh, over
//! the master connection rse already holds. rclone would list a directory and delete it
//! file by file, each a round trip to the server, where `rm -rf` on the server takes one;
//! and it would copy inside a server by downloading every file and uploading it again,
//! since sftp has no copy of its own. A server with an rsync of its own copies with that
//! instead of `cp` ([`rsync_command`], run and read by [`crate::rsync::Running`]).
//!
//! The command is an sh script handed to the login shell as `exec /bin/sh -c '…'`, which
//! every common one (sh, bash, zsh, fish, tcsh) passes on unchanged: the script holds no
//! single quote, no `!`, and no backslash but before a digit. Paths are spelt in octal
//! escapes, so any name passes.
//!
//! Stopping: rse keeps the command's standard input open. A watcher in the script reads it
//! and ends `rm`, `mv` or `cp` when it closes, which is when the ssh that carries the
//! command is stopped, so the work on the server stops with it.
//!
//! A copy is made under another name beside its target ([`COPY_TEMP`] and the script's
//! process number) and renamed once it is whole; stopped or failed, it is removed. So the
//! target's name never holds half a copy.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::terminal::ShellDir;

/// How long a command asked to stop may take before it is killed.
const STOP_TIME: Duration = Duration::from_secs(10);
/// How long what a command said is waited for after it ended.
const DRAIN_TIME: Duration = Duration::from_secs(1);
/// How long a server may take to say whether it has sh.
const PROBE_TIME: Duration = Duration::from_secs(10);
/// What a server with sh answers the probe.
const PROBE: &str = "rse-sh";
/// What is kept of what a command said.
const SAID_MAX: usize = 16 * 1024;
/// Starts the name a copy has, in its target's directory, until it is whole.
pub const COPY_TEMP: &str = ".rse-copy.";

/// What the machine is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Remove the entry, with all it holds.
    Delete(ShellDir),
    /// Rename the entry `from` to `to`, which must not exist.
    Move { from: ShellDir, to: ShellDir },
    /// Copy the entry `from`, with all it holds, to `to`, which must not exist. Links are
    /// copied as links; times and permissions are kept.
    Copy { from: ShellDir, to: ShellDir },
}

/// Sets the shell variable `name` to the path of `dir`, spelt in octal. The `x` keeps a
/// name ending in a line break whole through `$(…)`.
fn assign(script: &mut String, name: &str, dir: &ShellDir) {
    let (path, home) = match dir {
        ShellDir::Absolute(path) => (path, ""),
        ShellDir::Home(path) => (path, "$HOME/"),
    };
    assign_bytes(script, name, path.as_bytes(), home);
}

/// Sets the shell variable `name` to `before`, which the shell reads, and then `bytes` as
/// they are, spelt in octal.
fn assign_bytes(script: &mut String, name: &str, bytes: &[u8], before: &str) {
    let mut octal = String::new();
    for b in bytes {
        let _ = write!(octal, "\\0{b:03o}");
    }
    let _ = write!(
        script,
        r#"{name}=$(printf "%b" "{octal}"; echo x); {name}="{before}${{{name}%x}}"; "#
    );
}

/// The command line, for a login shell, that runs the machine's own `rsync` with `args`
/// ([`crate::rsync::args`], both sides paths on that machine), each passed as it is. The
/// watcher ends rsync when standard input closes, as it ends `rm`; rsync then keeps the
/// file it is in the middle of. Messages come as they are read here.
pub fn rsync_command(args: &[OsString]) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut script = String::from(r"exec 3<&0; ( read x <&3; kill $$ ) >/dev/null 2>&1 & ");
    let mut call = String::from("LC_ALL=C; export LC_ALL; exec rsync");
    for (i, arg) in args.iter().enumerate() {
        assign_bytes(&mut script, &format!("a{i}"), arg.as_bytes(), "");
        let _ = write!(call, r#" "$a{i}""#);
    }
    script.push_str(&call);
    format!("exec /bin/sh -c '{script}'")
}

/// The command line that carries `op` out, for a login shell. `None` for a path that names
/// nothing, which `rm -rf` must never be given.
pub fn command(op: &Op) -> Option<String> {
    let named = |dir: &ShellDir| match dir {
        ShellDir::Absolute(path) => !path.trim_matches('/').is_empty(),
        ShellDir::Home(path) => !path.is_empty(),
    };
    // The watcher: when standard input closes, it ends the command, which `exec` has put
    // in this shell's place, so that `$$` is its pid. It holds none of the command's
    // output, and so keeps no connection open once the command is done.
    let mut script = String::from(r"exec 3<&0; ( read x <&3; kill $$ ) >/dev/null 2>&1 & ");
    match op {
        Op::Delete(entry) => {
            if !named(entry) {
                return None;
            }
            assign(&mut script, "s", entry);
            script.push_str(r#"exec rm -rf -- "$s""#);
        }
        Op::Move { from, to } => {
            if !named(from) || !named(to) {
                return None;
            }
            assign(&mut script, "s", from);
            assign(&mut script, "d", to);
            // `mv` would put the entry inside a directory of the target's name.
            script.push_str(
                r#"if test -e "$d" || test -L "$d"; then echo "mv: the target exists" >&2; exit 1; fi; exec mv -- "$s" "$d""#,
            );
        }
        Op::Copy { from, to } => {
            if !named(from) || !named(to) {
                return None;
            }
            // Not the watcher of the others: `cp` cannot take this shell's place, which
            // has the copy to rename or remove after it. Here the watcher ends the whole
            // process group, which this shell leads, started by sshd or by `Running`: `cp`
            // ends, and the shell's `stop` removes what it copied.
            script.clear();
            script.push_str("exec 3<&0; ");
            assign(&mut script, "s", from);
            assign(&mut script, "d", to);
            let _ = write!(
                script,
                concat!(
                    r#"if test -e "$d" || test -L "$d"; then echo "cp: the target exists" >&2; exit 1; fi; "#,
                    r#"t="${{d%/*}}/{temp}$$"; "#,
                    r#"stop() {{ trap "" TERM HUP INT; rm -rf -- "$t"; exit 143; }}; trap stop TERM HUP INT; "#,
                    r"( read x <&3; kill -s TERM -- -$$ || kill $$ ) >/dev/null 2>&1 & ",
                    r#"cp -RPp -- "$s" "$t"; r=$?; "#,
                    // Again, as `mv` would put the copy inside a directory that came since.
                    r#"if test $r -eq 0; then if test -e "$d" || test -L "$d"; then echo "cp: the target exists" >&2; r=1; "#,
                    r#"else mv -- "$t" "$d"; r=$?; fi; fi; "#,
                    r#"test $r -eq 0 || rm -rf -- "$t"; exit $r"#,
                ),
                temp = COPY_TEMP
            );
        }
    }
    Some(format!("exec /bin/sh -c '{script}'"))
}

/// Which machines have sh, and which an rsync of their own that rse can use, by the
/// command that runs a command line on them.
#[derive(Debug, Default)]
pub struct Hosts {
    known: Mutex<HashMap<Vec<String>, bool>>,
    rsync: Mutex<HashMap<Vec<String>, bool>>,
}

impl Hosts {
    /// Whether the machine that `run` reaches has `/bin/sh`, asked once. A machine that
    /// does not answer is asked again the next time.
    pub fn has_sh(&self, run: &[String]) -> bool {
        if let Some(known) = self.known.lock().ok().and_then(|known| known.get(run).copied()) {
            return known;
        }
        let Some(answer) = probe(run, &format!("echo {PROBE}")) else { return false };
        let answer = answer.trim() == PROBE;
        if let Ok(mut known) = self.known.lock() {
            known.insert(run.to_vec(), answer);
        }
        answer
    }

    /// Whether the machine that `run` reaches has sh and an `rsync` that takes the options
    /// [`crate::rsync::args`] passes ([`crate::rsync::supported`]), asked once. A machine
    /// that does not answer is asked again the next time.
    pub fn has_rsync(&self, run: &[String]) -> bool {
        if let Some(known) = self.rsync.lock().ok().and_then(|known| known.get(run).copied()) {
            return known;
        }
        if !self.has_sh(run) {
            return false;
        }
        let Some(version) = probe(run, "rsync --version") else { return false };
        let answer = crate::rsync::supported(version.as_bytes());
        if let Ok(mut known) = self.rsync.lock() {
            known.insert(run.to_vec(), answer);
        }
        answer
    }
}

/// What sh on the machine `run` reaches prints for `script`, which holds nothing a login
/// shell reads; `None` when the machine does not answer in time.
fn probe(run: &[String], script: &str) -> Option<String> {
    let (program, args) = run.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .arg(format!("exec /bin/sh -c '{script}'"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIME;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// A command at work on its machine. Dropping it stops the command.
pub struct Running {
    child: Child,
    /// Held open while the command runs: closing it is what stops the command's work on
    /// a server.
    stdin: Option<ChildStdin>,
    said: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    /// How the command ended, and when that was seen.
    exit: Option<(ExitStatus, Instant)>,
    stopped: bool,
    kill_at: Option<Instant>,
}

impl Running {
    /// Starts `command` with `run`, the command that runs a command line on the machine.
    ///
    /// # Errors
    /// Fails when the program of `run` cannot be started.
    pub fn spawn(run: &[String], command: &str) -> io::Result<Self> {
        let (program, args) = run.split_first().ok_or(io::ErrorKind::InvalidInput)?;
        let mut child = Command::new(program)
            .args(args)
            .arg(command)
            // Messages as they are read here.
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // A process group of its own: the terminal's signals stay away from it, and
            // stopping reaches all it started.
            .process_group(0)
            .spawn()?;
        let said = Arc::new(Mutex::new(Vec::new()));
        let reader = child.stderr.take().and_then(|mut pipe| {
            let said = said.clone();
            std::thread::Builder::new()
                .name("host command output".to_owned())
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = pipe.read(&mut buf) {
                        let Ok(mut said) = said.lock() else { return };
                        if n == 0 {
                            return;
                        }
                        let room = SAID_MAX.saturating_sub(said.len());
                        said.extend_from_slice(&buf[..n.min(room)]);
                    }
                })
                .ok()
        });
        let stdin = child.stdin.take();
        Ok(Self { child, stdin, said, reader, exit: None, stopped: false, kill_at: None })
    }

    /// How the command ended, once it has and what it said is read.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        if self.exit.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.exit = Some((status, Instant::now()));
                    // The watcher, left reading it, ends too.
                    self.stdin = None;
                }
                _ if self.kill_at.is_some_and(|at| Instant::now() >= at) => {
                    self.signal(rustix::process::Signal::KILL);
                    self.kill_at = None;
                }
                _ => {}
            }
        }
        let (status, seen) = self.exit?;
        let read = self.reader.as_ref().is_none_or(JoinHandle::is_finished);
        if !read && !self.stopped && seen.elapsed() < DRAIN_TIME {
            return None;
        }
        Some(self.result(status))
    }

    fn result(&self, status: ExitStatus) -> Result<(), String> {
        use std::os::unix::process::ExitStatusExt;
        // Finished before the stop reached it: done all the same.
        if status.success() {
            return Ok(());
        }
        if self.stopped {
            return Err(tr!("host-stopped"));
        }
        let said = self.said.lock().map(|said| String::from_utf8_lossy(&said).trim().to_owned());
        match (said, status.code()) {
            (Ok(said), _) if !said.is_empty() => Err(said),
            (_, Some(code)) => Err(tr!("host-exit", code = code.to_string())),
            (_, None) => {
                Err(tr!("host-signal", signal = status.signal().unwrap_or_default().to_string()))
            }
        }
    }

    /// Asks the command to stop, without waiting: [`poll`](Self::poll) kills it if it is
    /// still running `STOP_TIME` later.
    pub fn terminate(&mut self) {
        if self.exit.is_some() || self.stopped {
            return;
        }
        self.stopped = true;
        // First what stops the work on a server, then the processes here.
        self.stdin = None;
        self.signal(rustix::process::Signal::TERM);
        self.kill_at = Some(Instant::now() + STOP_TIME);
    }

    /// Stops the command and waits for it to end, killing it after `STOP_TIME`.
    pub fn stop(&mut self) {
        if self.exit.is_some() {
            return;
        }
        self.terminate();
        let deadline = self.kill_at.unwrap_or_else(|| Instant::now() + STOP_TIME);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    self.signal(rustix::process::Signal::KILL);
                    break self.child.wait().ok();
                }
            }
        };
        self.exit = status.map(|s| (s, Instant::now()));
    }

    /// Sends `signal` to the command's process group; only while the command is not yet
    /// waited for, when the group's number cannot be another's.
    fn signal(&self, signal: rustix::process::Signal) {
        use rustix::process::{Pid, kill_process_group};
        if self.exit.is_none() {
            let _ = kill_process_group(Pid::from_child(&self.child), signal);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::*;

    fn sh() -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned()]
    }

    fn absolute(path: &Path) -> ShellDir {
        ShellDir::Absolute(String::from_utf8(path.as_os_str().as_bytes().to_vec()).unwrap())
    }

    fn finish(mut run: Running) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(result) = run.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "the command did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn run(op: &Op) -> Result<(), String> {
        finish(Running::spawn(&sh(), &command(op).unwrap()).unwrap())
    }

    /// Names a shell would read otherwise than as they are.
    const NAMES: &[&str] = &[
        "plain",
        "two words",
        "-rf",
        "quote'd \"twice\"",
        "$HOME `id` $(id)",
        "back\\slash\\0101",
        "line\nbreak\n",
        "star * ? [a]",
        "中文 名字",
        "!bang ~tilde #hash ;semi &amp |pipe",
    ];

    #[test]
    fn the_script_keeps_to_what_every_login_shell_passes_on() {
        let dir = ShellDir::Absolute("/tmp/it's a \\ ! name".to_owned());
        for op in [
            Op::Delete(dir.clone()),
            Op::Move { from: dir.clone(), to: dir.clone() },
            Op::Copy { from: dir.clone(), to: dir.clone() },
        ] {
            let command = command(&op).unwrap();
            keeps_to_the_rules(&command);
        }
        let args = ["-rlt", "--", "it's a \\ ! name", "$HOME `id`\n"].map(OsString::from);
        keeps_to_the_rules(&rsync_command(&args));
    }

    fn keeps_to_the_rules(command: &str) {
        let script = command.strip_prefix("exec /bin/sh -c '").unwrap();
        let script = script.strip_suffix('\'').unwrap();
        assert!(!script.contains('\'') && !script.contains('!'), "{script}");
        let after: Vec<char> =
            script.split('\\').skip(1).filter_map(|s| s.chars().next()).collect();
        assert!(after.iter().all(char::is_ascii_digit), "{script}");
    }

    #[test]
    fn a_path_that_names_nothing_gets_no_command() {
        for dir in ["", "/", "//"] {
            assert_eq!(command(&Op::Delete(ShellDir::Absolute(dir.to_owned()))), None, "{dir:?}");
        }
        assert_eq!(command(&Op::Delete(ShellDir::Home(String::new()))), None);
        let named = ShellDir::Absolute("/a".to_owned());
        let home = ShellDir::Home(String::new());
        assert_eq!(command(&Op::Move { from: named.clone(), to: home.clone() }), None);
        assert_eq!(command(&Op::Move { from: home.clone(), to: named.clone() }), None);
        assert_eq!(command(&Op::Copy { from: named.clone(), to: home.clone() }), None);
        assert_eq!(command(&Op::Copy { from: home, to: named }), None);
    }

    #[test]
    fn entries_of_any_name_are_deleted_with_all_they_hold() {
        let tmp = tempfile::tempdir().unwrap();
        for name in NAMES {
            let entry = tmp.path().join(name);
            fs::create_dir_all(entry.join("deep/er")).unwrap();
            fs::write(entry.join("deep/er/file"), "x").unwrap();
            fs::write(tmp.path().join("kept"), "x").unwrap();
            run(&Op::Delete(absolute(&entry))).unwrap();
            assert!(!entry.exists(), "{name:?}");
            assert!(tmp.path().join("kept").exists(), "{name:?}");
        }
        // A file, a link (not what it leads to), and what is not there.
        let (file, link) = (tmp.path().join("file"), tmp.path().join("link"));
        fs::write(&file, "x").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("kept"), &link).unwrap();
        for entry in [&file, &link, &tmp.path().join("missing")] {
            run(&Op::Delete(absolute(entry))).unwrap();
            assert!(fs::symlink_metadata(entry).is_err());
        }
        assert!(tmp.path().join("kept").exists());
    }

    #[test]
    fn entries_of_any_name_are_moved_but_never_onto_another() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        for name in NAMES {
            fs::create_dir_all(a.join(name).join("in")).unwrap();
            let op = Op::Move { from: absolute(&a.join(name)), to: absolute(&b.join(name)) };
            run(&op).unwrap();
            assert!(!a.join(name).exists() && b.join(name).join("in").is_dir(), "{name:?}");
            // Again, with the target there: refused, and nothing put inside it.
            fs::create_dir_all(a.join(name)).unwrap();
            let error = run(&op).unwrap_err();
            assert_eq!(error, "mv: the target exists", "{name:?}");
            assert!(a.join(name).exists() && !b.join(name).join(name).exists(), "{name:?}");
        }
        // A link that leads nowhere is a target all the same.
        std::os::unix::fs::symlink("nowhere", b.join("dangling")).unwrap();
        fs::write(a.join("dangling"), "x").unwrap();
        let op =
            Op::Move { from: absolute(&a.join("dangling")), to: absolute(&b.join("dangling")) };
        assert!(run(&op).is_err());
        assert!(a.join("dangling").exists());
    }

    /// What a directory holds under a name that starts as an unfinished copy's does.
    fn unfinished(dir: &Path) -> Vec<String> {
        let names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name());
        names
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n.starts_with(COPY_TEMP))
            .collect()
    }

    #[test]
    fn entries_of_any_name_are_copied_whole_but_never_onto_another() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        for name in NAMES {
            let from = a.join(name);
            fs::create_dir_all(from.join("in/deep")).unwrap();
            fs::write(from.join("in/deep/file"), "content").unwrap();
            fs::set_permissions(from.join("in/deep/file"), fs::Permissions::from_mode(0o640))
                .unwrap();
            // A link stays a link, also one that leads nowhere.
            std::os::unix::fs::symlink("in/deep/file", from.join("link")).unwrap();
            std::os::unix::fs::symlink("nowhere", from.join("dangling")).unwrap();
            let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
            fs::File::open(from.join("in/deep/file")).unwrap().set_modified(old).unwrap();
            let op = Op::Copy { from: absolute(&from), to: absolute(&b.join(name)) };
            run(&op).unwrap();
            let to = b.join(name);
            assert_eq!(fs::read_to_string(to.join("in/deep/file")).unwrap(), "content", "{name:?}");
            let (was, is) = (
                fs::metadata(from.join("in/deep/file")).unwrap(),
                fs::metadata(to.join("in/deep/file")).unwrap(),
            );
            assert_eq!((was.mtime(), was.mode()), (is.mtime(), is.mode()), "{name:?}");
            assert_eq!(fs::read_link(to.join("link")).unwrap(), Path::new("in/deep/file"));
            assert_eq!(fs::read_link(to.join("dangling")).unwrap(), Path::new("nowhere"));
            assert!(from.join("in/deep/file").exists(), "the source stays: {name:?}");
            // Again, with the target there: refused, and nothing put inside it.
            let error = run(&op).unwrap_err();
            assert_eq!(error, "cp: the target exists", "{name:?}");
            assert!(!to.join(name).exists(), "{name:?}");
            assert_eq!(unfinished(&b), Vec::<String>::new(), "{name:?}");
        }
        // A file, and a link, which is copied and not what it leads to.
        fs::write(a.join("file"), "x").unwrap();
        std::os::unix::fs::symlink("file", a.join("to file")).unwrap();
        for name in ["file", "to file"] {
            run(&Op::Copy { from: absolute(&a.join(name)), to: absolute(&b.join(name)) }).unwrap();
        }
        assert_eq!(fs::read_to_string(b.join("file")).unwrap(), "x");
        assert_eq!(fs::read_link(b.join("to file")).unwrap(), Path::new("file"));
        // A link that leads nowhere is a target all the same.
        std::os::unix::fs::symlink("nowhere", b.join("taken")).unwrap();
        fs::write(a.join("taken"), "x").unwrap();
        let op = Op::Copy { from: absolute(&a.join("taken")), to: absolute(&b.join("taken")) };
        assert!(run(&op).is_err());
        assert_eq!(fs::read_link(b.join("taken")).unwrap(), Path::new("nowhere"));
    }

    /// What cannot be copied whole is not left half copied.
    #[test]
    fn a_copy_that_fails_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(a.join("in")).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("in/readable"), "x").unwrap();
        let error =
            run(&Op::Copy { from: absolute(&a.join("missing")), to: absolute(&b.join("missing")) })
                .unwrap_err();
        assert!(error.contains("missing"), "{error}");
        assert_eq!(fs::read_dir(&b).unwrap().count(), 0);
    }

    /// The command of a copy of `a/dir` to `b/dir` under `tmp`, with a `cp` that makes the
    /// copy's first directory and then takes a minute.
    fn slow_copy(tmp: &Path) -> String {
        fs::create_dir_all(tmp.join("a/dir")).unwrap();
        fs::create_dir_all(tmp.join("b")).unwrap();
        let op = Op::Copy { from: absolute(&tmp.join("a/dir")), to: absolute(&tmp.join("b/dir")) };
        let command = command(&op).unwrap();
        let cp = r#"cp -RPp -- "$s" "$t""#;
        assert!(command.contains(cp));
        command.replace(cp, r#"{ mkdir "$t"; sleep 60; }"#)
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A stopped copy is taken away, and its target never appears.
    #[test]
    fn a_stopped_copy_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        let mut run = Running::spawn(&sh(), &slow_copy(tmp.path())).unwrap();
        until("the copy did not start", || unfinished(&b).len() == 1);
        let begun = Instant::now();
        run.terminate();
        assert_eq!(finish(run), Err(tr!("host-stopped")));
        assert!(begun.elapsed() < Duration::from_secs(30), "waited for the copy");
        assert_eq!(fs::read_dir(&b).unwrap().count(), 0);
    }

    /// Closing the input, which is all a stopped ssh does to the server's side, ends the
    /// copy there and takes away what it made.
    #[test]
    fn a_copy_ends_and_is_taken_away_when_its_input_closes() {
        let tmp = tempfile::tempdir().unwrap();
        let b = tmp.path().join("b");
        // The leader of its process group, as sshd starts a command.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(slow_copy(tmp.path()))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        until("the copy did not start", || unfinished(&b).len() == 1);
        assert!(child.try_wait().unwrap().is_none(), "ended with its input open");
        drop(child.stdin.take());
        until("still running with its input closed", || child.try_wait().unwrap().is_some());
        assert_eq!(fs::read_dir(&b).unwrap().count(), 0);
    }

    #[test]
    fn a_path_in_the_login_directory_starts_there() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("old/in")).unwrap();
        let command = command(&Op::Delete(ShellDir::Home("old".to_owned()))).unwrap();
        // Its input held open, as `Running` holds it.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env("HOME", tmp.path())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        // Not `wait`, which closes the input first.
        let status = loop {
            match child.try_wait().unwrap() {
                Some(status) => break status,
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        assert!(status.success());
        assert!(!tmp.path().join("old").exists());
    }

    #[test]
    fn what_the_command_says_is_the_error() {
        let tmp = tempfile::tempdir().unwrap();
        let from = absolute(&tmp.path().join("missing"));
        let error = run(&Op::Move { from, to: absolute(&tmp.path().join("b")) }).unwrap_err();
        assert!(error.contains("missing"), "{error}");
    }

    /// Closing the command's input, which is all a stopped ssh does to the server's side,
    /// ends the command there.
    #[test]
    fn a_command_ends_when_its_input_closes() {
        let script = command(&Op::Delete(ShellDir::Absolute("/nonexistent/x".to_owned())))
            .unwrap()
            .replace(r#"exec rm -rf -- "$s""#, "exec sleep 60");
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(child.try_wait().unwrap().is_none(), "ended with its input open");
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "still running with its input closed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_stopped_command_says_so() {
        let mut run = Running::spawn(&sh(), "exec sleep 60").unwrap();
        run.terminate();
        assert_eq!(finish(run), Err(tr!("host-stopped")));
    }

    /// Each argument reaches the machine's rsync as it is, whatever it holds.
    #[test]
    fn rsync_gets_its_arguments_as_they_are() {
        let hosts = Hosts::default();
        if !hosts.has_rsync(&sh()) {
            eprintln!("skipped: no rsync 3.1 or newer");
            return;
        }
        assert!(hosts.has_rsync(&sh()));
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(&b).unwrap();
        for name in NAMES {
            fs::create_dir_all(a.join(name)).unwrap();
            fs::write(a.join(name).join("file"), *name).unwrap();
            let args = crate::rsync::args(
                crate::rsync::Op::Copy,
                &crate::rsync::Side::Local(a.clone()),
                &crate::rsync::Side::Local(b.clone()),
                name,
                false,
            );
            finish(Running::spawn(&sh(), &rsync_command(&args)).unwrap()).unwrap();
            assert_eq!(fs::read_to_string(b.join(name).join("file")).unwrap(), *name);
        }
        // A machine whose rsync is too old, or not rsync, has none to use.
        let old = vec!["/bin/sh".to_owned(), "-c".to_owned(), "echo rse-sh".to_owned()];
        assert!(hosts.has_sh(&old) && !hosts.has_rsync(&old));
    }

    #[test]
    fn a_machine_is_asked_once_whether_it_has_sh() {
        let hosts = Hosts::default();
        assert!(hosts.has_sh(&sh()));
        assert!(hosts.has_sh(&sh()));
        // One whose shell runs nothing as sh would.
        let other = vec!["/bin/sh".to_owned(), "-c".to_owned(), "true".to_owned()];
        assert!(!hosts.has_sh(&other));
        assert!(!hosts.has_sh(&["/nonexistent/program".to_owned()]));
    }
}
