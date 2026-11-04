//! Copies and moves between the local disk and an sftp server with the system `rsync`: a
//! changed file sends only what changed, and an interrupted one picks up where it stopped.
//!
//! rsync logs in through the master connection rse keeps to the server
//! ([`SystemSsh::rsync_remote`]), so a transfer is no new login, and is used only when the
//! server has an `rsync` of its own, and the local one is rsync 3.1 or newer ([`usable`]):
//! rse never ships or uploads one. [`args`] builds the command line, and [`Running`] runs it
//! and reads its progress.
//!
//! A file rsync is stopped in the middle of is kept in [`PARTIAL_DIR`] beside its target,
//! and the next run sends only what the kept part lacks (`--partial-dir`). Not
//! `--append-verify`, which skips a target as long as the source or longer, even when its
//! content differs.
//!
//! Progress is rsync's whole-job `--info=progress2`. It prints the bytes done and a
//! percentage, no total, so the total is narrowed down from the percentages seen. Files
//! rsync finds up to date count in the total but never as done, so a job that skips files
//! ends short of 100%.
//!
//! [`SystemSsh::rsync_remote`]: crate::rclone::system_ssh::SystemSsh::rsync_remote

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::location::Location;
use crate::rclone::api::{Stats, Transferring};

/// Where rsync keeps a file it was stopped in the middle of, in the target's directory.
pub const PARTIAL_DIR: &str = ".rse-partial";

/// Starts rsync's line for each file it updates (`--out-format`), before its size and name.
const FILE_TAG: &str = "rse-file:";
/// The longest output line read; the rest of a longer one is dropped.
const MAX_LINE: usize = 64 * 1024;
/// How much of rsync's error output is kept, from the end.
const MAX_STDERR: usize = 64 * 1024;
/// How long a stopped rsync may take to keep its partial file before it is killed.
const STOP_TIME: Duration = Duration::from_secs(10);
/// How long after rsync exits the last of its output may still come, from the processes
/// it started.
const DRAIN_TIME: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Copy,
    Move,
}

/// How rsync reaches an sftp server ([`SystemSsh::rsync_remote`]).
///
/// [`SystemSsh::rsync_remote`]: crate::rclone::system_ssh::SystemSsh::rsync_remote
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncRemote {
    /// rsync's remote shell (`--rsh`): ssh through the master connection, without the
    /// destination, which rsync adds.
    pub rsh: String,
    /// The account and server rsync puts before the `:` (`me@box`, `me@[::1]`).
    pub host: String,
    /// The fs root on the server, as rclone's sftp backend reads it: absolute, or relative
    /// to the login directory (empty for that directory itself), where rsync starts too.
    pub root: String,
}

impl RsyncRemote {
    /// For `ssh`, the command that reaches the server with its destination (`me@box`)
    /// last, and the fs root. `None` when rsync would read the destination otherwise than
    /// ssh does: as a local path, or as some other server.
    pub fn new(ssh: &[String], root: &str) -> Option<Self> {
        let (dest, command) = ssh.split_last()?;
        if command.is_empty() {
            return None;
        }
        let rsh: Vec<String> = command.iter().map(|a| rsh_quote(a)).collect();
        Some(Self { rsh: rsh.join(" "), host: host_spec(dest)?, root: root.to_owned() })
    }

    /// The server path of `path` in the fs (a [`Location`]'s path), joined to the root as
    /// [`Location::fs_string`] joins it.
    pub fn path(&self, path: &str) -> String {
        join(&self.root, path)
    }

    /// `path` in the fs as one side of a transfer.
    pub fn side(&self, path: &str) -> Side {
        Side::Remote(self.clone(), self.path(path))
    }
}

/// One end of a transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    /// An absolute local path.
    Local(PathBuf),
    /// A path on the server ([`RsyncRemote::path`]).
    Remote(RsyncRemote, String),
}

impl Side {
    /// A local [`Location`].
    pub fn local(loc: &Location) -> Self {
        Self::Local(PathBuf::from(loc.fs_string()))
    }

    fn rsh(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::Remote(remote, _) => Some(&remote.rsh),
        }
    }

    /// The argument naming `name` in this directory, or the directory itself when `name`
    /// is empty: never read as an option, nor a local path as a server's.
    fn arg(&self, name: &str) -> Vec<u8> {
        match self {
            Self::Local(dir) => {
                let dir = dir.as_os_str().as_bytes();
                // A relative path could start with `-`, or hold a `:` before any `/`,
                // which would make rsync read it as a server's.
                let mut arg = if dir.starts_with(b"/") { Vec::new() } else { b"./".to_vec() };
                arg.extend_from_slice(dir);
                if !name.is_empty() {
                    if !arg.ends_with(b"/") {
                        arg.push(b'/');
                    }
                    arg.extend_from_slice(name.as_bytes());
                }
                arg
            }
            Self::Remote(remote, dir) => {
                let path = join(dir, name);
                let path = if path.is_empty() {
                    // The login directory.
                    ".".to_owned()
                } else if path.starts_with(':') {
                    // `box::x` would be an rsync daemon's module.
                    format!("./{path}")
                } else if path.starts_with("//") {
                    // `rsync://` would be a daemon's URL.
                    format!("/{}", path.trim_start_matches('/'))
                } else {
                    path
                };
                format!("{}:{path}", remote.host).into_bytes()
            }
        }
    }
}

/// Whether the local `rsync` takes the options [`args`] passes: rsync 3.1 or newer, the first
/// with `--info=progress2`. Not openrsync, the `rsync` of macOS since 15.4, which has none
/// of `--no-inc-recursive`, `--info` and `--out-format`; nor the rsync 2.6.9 of older
/// macOS. Asked once.
pub fn usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        Command::new("rsync")
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success() && supported(&o.stdout))
    })
}

/// Whether the first line `rsync --version` printed, such as `rsync  version v3.2.7
/// protocol version 31` (3.2.3 on put a `v` there), names rsync 3.1 or newer.
fn supported(version: &[u8]) -> bool {
    let text = String::from_utf8_lossy(version);
    let mut words = text.lines().next().unwrap_or_default().split_whitespace();
    if words.next() != Some("rsync") || words.next() != Some("version") {
        return false;
    }
    let Some(number) = words.next() else { return false };
    let mut parts = number.trim_start_matches('v').split('.').map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (3, 1),
        _ => false,
    }
}

/// rsync's arguments for copying or moving `name`, one path component, from `src_dir` into
/// `dst_dir`, making `dst_dir/name`: a directory with everything in it. At most one side
/// may be remote, as rsync copies between no two servers.
///
/// Times and symbolic links are kept; owners, groups and permissions are left as the
/// receiving side makes them. Files are compared by size and time, or by checksum with
/// `checksum`. A move removes each source file once it is copied, or found already there,
/// and leaves the source directories, emptied, for the caller to remove (rclone's
/// `operations/rmdirs`, with `leaveRoot` off for a directory moved).
pub fn args(op: Op, src_dir: &Side, dst_dir: &Side, name: &str, checksum: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-rlt",
        // The whole file list first, so that the percentage is of the whole job.
        "--no-inc-recursive",
        // Paths reach the server's rsync as they are, not through its shell.
        "-s",
        &format!("--partial-dir={PARTIAL_DIR}"),
        // What rclone keeps of a file it was stopped in the middle of, as rclone skips
        // rsync's: never copied as if it were a file of the user's.
        "--exclude=*.rse-partial",
        "--info=progress2,stats1",
        "--no-human-readable",
        &format!("--out-format={FILE_TAG}%l %n"),
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if checksum {
        args.push("--checksum".into());
    }
    if op == Op::Move {
        args.push("--remove-source-files".into());
    }
    if let Some(rsh) = src_dir.rsh().or_else(|| dst_dir.rsh()) {
        args.push(format!("--rsh={rsh}").into());
    }
    args.push("--".into());
    // No trailing `/`, which would copy what is in the directory rather than it.
    let mut src = src_dir.arg(name);
    while src.len() > 1 && src.ends_with(b"/") {
        src.pop();
    }
    // A trailing `/`: into the directory, even for a single file.
    let mut dst = dst_dir.arg("");
    if !dst.ends_with(b"/") {
        dst.push(b'/');
    }
    args.extend([OsString::from_vec(src), OsString::from_vec(dst)]);
    args
}

/// `dir/name` as [`Location::fs_string`] joins a root and a path.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') {
        format!("{dir}{name}")
    } else if name.is_empty() {
        dir.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// One argument of rsync's remote shell command, which rsync splits at spaces, joining
/// what is in quotes; a quote is doubled to stand for itself.
pub(crate) fn rsh_quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\'', '"']) {
        format!("'{}'", arg.replace('\'', "''"))
    } else {
        arg.to_owned()
    }
}

/// `dest`, ssh's destination (`me@box`), as it goes before the `:` of rsync's argument;
/// `None` when rsync would read it otherwise. rsync takes the account to the last `@` and
/// passes it to ssh with `-l`, and the server in brackets when it holds a `:`.
pub(crate) fn host_spec(dest: &str) -> Option<String> {
    let (user, host) = match dest.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, dest),
    };
    // A `/` before the `:` makes the argument a local path; a `[` starts a bracketed
    // server only right after the `@`, and anywhere else a local path too. rsync passes
    // the server to ssh after its options, where a leading `-` would be one.
    let bad_user = user.is_some_and(|u| u.is_empty() || u.contains([':', '/', '[']));
    if host.is_empty() || host.starts_with('-') || host.contains(['/', '[', ']']) || bad_user {
        return None;
    }
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    Some(match user {
        Some(user) => format!("{user}@{host}"),
        None => host,
    })
}

/// rsync's standard output, read as it comes: progress lines end in `\r`, others in `\n`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Progress {
    line: Vec<u8>,
    bytes: u64,
    /// Bytes per second.
    speed: f64,
    /// Files done.
    transfers: u64,
    /// The bounds of the job's total that the percentages so far leave.
    low: u64,
    high: Option<u64>,
    /// The file rsync is on: its name, its size, and the job's bytes when it started.
    file: Option<(String, u64, u64)>,
    /// Bytes sent and received over the connection, from rsync's closing summary.
    wire: Option<(u64, u64)>,
}

/// One `--info=progress2` line.
#[derive(Debug, PartialEq)]
struct Line {
    bytes: u64,
    percent: u64,
    speed: f64,
    /// Files done, on the line printed as one ends.
    done: Option<u64>,
}

impl Progress {
    pub(crate) fn feed(&mut self, data: &[u8]) {
        for &b in data {
            if b == b'\r' || b == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.read(&line);
            } else if self.line.len() < MAX_LINE {
                self.line.push(b);
            }
        }
    }

    fn read(&mut self, line: &[u8]) {
        if let Some(rest) = line.strip_prefix(FILE_TAG.as_bytes()) {
            let Some(space) = rest.iter().position(|&b| b == b' ') else { return };
            let Some(size) = number(&String::from_utf8_lossy(&rest[..space])) else { return };
            let name = unescape(&rest[space + 1..]);
            // A directory is no file to show.
            self.file = (!name.ends_with('/')).then_some((name, size, self.bytes));
            return;
        }
        let Ok(text) = std::str::from_utf8(line) else { return };
        if let Some(line) = progress_line(text) {
            self.progressed(&line);
        } else if let Some(wire) = summary_line(text) {
            self.wire = Some(wire);
        }
    }

    fn progressed(&mut self, line: &Line) {
        self.bytes = line.bytes;
        self.speed = line.speed;
        // A line no total fits, such as 1 byte at 60%, says nothing of it.
        if let Some((low, high)) = total_bounds(line.bytes, line.percent) {
            let (both_low, both_high) = (self.low.max(low), min_some(self.high, high));
            if both_high.is_some_and(|h| both_low > h) {
                // Not one total after all: the latest line is the one to go by.
                (self.low, self.high) = (low, high);
            } else {
                (self.low, self.high) = (both_low, both_high);
            }
        }
        if let Some(done) = line.done {
            if done > self.transfers {
                self.file = None;
            }
            self.transfers = done;
        }
    }

    /// As far as rsync has said: it starts each progress line with `\r` and ends it only
    /// with the next one, so the line it is on is read as far as it goes.
    fn now(&self) -> Self {
        let mut now = Self { line: Vec::new(), file: self.file.clone(), ..*self };
        if let Some(line) = std::str::from_utf8(&self.line).ok().and_then(progress_line) {
            now.progressed(&line);
        }
        now
    }

    /// The job's total as narrowed down so far: at least `.0`, at most `.1` if known.
    #[cfg(any(test, fuzzing))]
    pub(crate) fn total_bounds(&self) -> (u64, Option<u64>) {
        let now = self.now();
        (now.low, now.high)
    }

    /// Bytes sent and received over the connection, once rsync has said.
    pub(crate) fn wire(&self) -> Option<(u64, u64)> {
        self.wire
    }

    pub(crate) fn stats(&self) -> Stats {
        let now = self.now();
        now.read_stats()
    }

    fn read_stats(&self) -> Stats {
        // Unknown until a percentage above 0 bounds it from above.
        let total = self.high.map_or(0, |high| self.low.midpoint(high).max(self.bytes));
        #[allow(clippy::cast_precision_loss)]
        let eta = (self.speed > 0.0 && total > 0)
            .then(|| total.saturating_sub(self.bytes) as f64 / self.speed);
        let transferring = self.file.iter().map(|(name, size, start)| Transferring {
            name: name.clone(),
            bytes: signed(self.bytes.saturating_sub(*start).min(*size)),
            size: signed(*size),
        });
        Stats {
            bytes: signed(self.bytes),
            total_bytes: signed(total),
            speed: self.speed,
            eta,
            transfers: signed(self.transfers),
            transferring: transferring.collect(),
            ..Stats::default()
        }
    }
}

fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn min_some(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        _ => a.or(b),
    }
}

/// The bounds of a total of which `bytes` is `percent`, `None` when there is none: rsync
/// truncates the percentage, and prints 100 only for all of it.
fn total_bounds(bytes: u64, percent: u64) -> Option<(u64, Option<u64>)> {
    if percent >= 100 {
        return Some((bytes, Some(bytes)));
    }
    let hundred = u128::from(bytes) * 100;
    let low = hundred / u128::from(percent + 1) + 1;
    let high = (percent > 0).then(|| hundred / u128::from(percent));
    let clamp = |n: u128| u64::try_from(n).unwrap_or(u64::MAX);
    let low = clamp(low).max(bytes.saturating_add(1));
    let high = high.map(clamp);
    high.is_none_or(|h| low <= h).then_some((low, high))
}

/// `      1234567  45%    1.23MB/s    0:00:12 (xfr#3, to-chk=10/20)`, the part in
/// brackets only on the line printed as a file ends.
fn progress_line(text: &str) -> Option<Line> {
    let mut words = text.split_whitespace();
    let bytes = number(words.next()?)?;
    let percent = words.next()?.strip_suffix('%')?;
    let percent: u64 = percent
        .parse()
        .ok()
        .filter(|p| *p <= 100 && percent.bytes().all(|b| b.is_ascii_digit()))?;
    let speed = rate(words.next()?)?;
    let time = words.next()?;
    // `h:mm:ss`, or `??:??:??`: never a part of one, while rsync is still writing it.
    let clock: Vec<&str> = time.split(':').collect();
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit() || b == b'?');
    if !matches!(clock[..], [h, m, s] if !h.is_empty() && m.len() == 2 && s.len() == 2)
        || !clock.iter().all(|t| digits(t))
    {
        return None;
    }
    let done = match words.next() {
        None => None,
        Some(word) => {
            let done = word.strip_prefix("(xfr#")?.strip_suffix(',')?;
            let check = words.next()?.strip_suffix(')')?;
            let (_, counts) = check.split_once("-chk=")?;
            let (left, all) = counts.split_once('/')?;
            number(left)?;
            number(all)?;
            Some(number(done)?)
        }
    };
    words.next().is_none().then_some(Line { bytes, percent, speed, done })
}

/// `sent 1000506 bytes  received 57 bytes  2001126.00 bytes/sec`.
fn summary_line(text: &str) -> Option<(u64, u64)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words[..] {
        ["sent", sent, "bytes", "received", received, "bytes", ..] => {
            Some((number(sent)?, number(received)?))
        }
        _ => None,
    }
}

/// A count as rsync prints it, maybe with digit separators.
fn number(word: &str) -> Option<u64> {
    let mut digits = word.bytes().filter(u8::is_ascii_digit).peekable();
    digits.peek()?;
    let separators = word.bytes().all(|b| b.is_ascii_digit() || b == b',' || b == b'.');
    if !separators || !word.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    digits.try_fold(0u64, |n, d| n.checked_mul(10)?.checked_add(u64::from(d - b'0')))
}

/// `1.23MB/s` in bytes per second; rsync's units are of 1024.
fn rate(word: &str) -> Option<f64> {
    let value = word.strip_suffix("B/s")?;
    let (value, power) = match value.as_bytes().last()? {
        b'k' => (&value[..value.len() - 1], 1),
        b'M' => (&value[..value.len() - 1], 2),
        b'G' => (&value[..value.len() - 1], 3),
        b'T' => (&value[..value.len() - 1], 4),
        _ => return None,
    };
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let value: f64 = value.parse().ok()?;
    Some(value * 1024f64.powi(power)).filter(|v| v.is_finite())
}

/// A name as rsync prints it, with `\#ooo` for each byte it would not print as it is.
fn unescape(name: &[u8]) -> String {
    let mut out = Vec::with_capacity(name.len());
    let mut i = 0;
    while i < name.len() {
        let octal = name.get(i..i + 5).filter(|e| {
            e.starts_with(b"\\#")
                && e[2..].iter().all(|d| (b'0'..=b'7').contains(d))
                && e[2] <= b'3'
        });
        if let Some(e) = octal {
            out.push((e[2] - b'0') * 64 + (e[3] - b'0') * 8 + (e[4] - b'0'));
            i += 5;
        } else {
            out.push(name[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// rsync at work, its output read on threads of their own. Dropping it stops rsync.
pub struct Running {
    child: Child,
    output: Arc<Mutex<Output>>,
    /// How rsync ended, and when that was seen.
    exit: Option<(ExitStatus, Instant)>,
    stopped: bool,
    /// When a rsync asked to stop and still running is killed.
    kill_at: Option<Instant>,
}

#[derive(Default)]
struct Output {
    progress: Progress,
    /// The end of rsync's error output.
    stderr: Vec<u8>,
    /// Standard output and error read to their end.
    closed: [bool; 2],
}

impl Running {
    /// Starts `rsync` with `args` (from [`args`], maybe with more options before the `--`).
    pub fn spawn(args: Vec<OsString>) -> io::Result<Self> {
        let child = Command::new("rsync")
            .args(args)
            // Numbers and messages as they are read here.
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A process group of its own, with the processes it starts (ssh and its own):
            // the terminal's signals stay away from them, and stopping reaches them all.
            .process_group(0)
            .spawn()?;
        let output = Arc::new(Mutex::new(Output::default()));
        let mut running = Self { child, output, exit: None, stopped: false, kill_at: None };
        let (stdout, stderr) = (running.child.stdout.take(), running.child.stderr.take());
        if let Some(pipe) = stdout {
            read_into(pipe, &running.output, 0, |o, data| o.progress.feed(data))?;
        }
        if let Some(pipe) = stderr {
            read_into(pipe, &running.output, 1, |o, data| {
                o.stderr.extend_from_slice(data);
                let over = o.stderr.len().saturating_sub(MAX_STDERR);
                o.stderr.drain(..over);
            })?;
        }
        Ok(running)
    }

    /// The progress so far, and how rsync ended once it has: `Err` with what it said, or
    /// that it was stopped.
    pub fn poll(&mut self) -> (Stats, Option<Result<(), String>>) {
        if self.exit.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => self.exit = Some((status, Instant::now())),
                _ if self.kill_at.is_some_and(|at| Instant::now() >= at) => {
                    self.signal(rustix::process::Signal::KILL);
                    self.kill_at = None;
                }
                _ => {}
            }
        }
        let Ok(output) = self.output.lock() else {
            return (Stats::default(), Some(Err(tr!("internal-lock"))));
        };
        let mut stats = output.progress.stats();
        let said = messages(&output.stderr);
        let errors: Vec<&String> = said.iter().filter(|l| l.starts_with("rsync: ")).collect();
        stats.errors = i64::try_from(errors.len()).unwrap_or(i64::MAX);
        stats.last_error = errors.last().map(|e| (*e).clone()).unwrap_or_default();
        let result = self.exit.and_then(|(status, seen)| {
            let read =
                self.stopped || output.closed.iter().all(|c| *c) || seen.elapsed() > DRAIN_TIME;
            read.then(|| self.result(status, &said))
        });
        (stats, result)
    }

    fn result(&self, status: ExitStatus, said: &[String]) -> Result<(), String> {
        // Finished before the stop reached it: done all the same.
        if status.success() {
            Ok(())
        } else if self.stopped {
            Err(tr!("rsync-stopped"))
        } else if !said.is_empty() {
            Err(said.join("\n"))
        } else if let Some(code) = status.code() {
            Err(tr!("rsync-exit", code = code.to_string()))
        } else {
            Err(tr!("rsync-signal", signal = status.signal().unwrap_or_default().to_string()))
        }
    }

    /// Bytes sent and received over the connection, once rsync has finished and said.
    pub fn wire(&self) -> Option<(u64, u64)> {
        self.output.lock().ok()?.progress.wire()
    }

    /// Asks rsync to stop with SIGTERM, without waiting: [`poll`] kills it if it is still
    /// running `STOP_TIME` later, and reports the end as for [`stop`].
    ///
    /// [`poll`]: Self::poll
    /// [`stop`]: Self::stop
    pub fn terminate(&mut self) {
        if self.exit.is_some() || self.stopped {
            return;
        }
        self.stopped = true;
        self.signal(rustix::process::Signal::TERM);
        self.kill_at = Some(Instant::now() + STOP_TIME);
    }

    /// Stops rsync with SIGTERM, on which it keeps the file it is in the middle of in
    /// [`PARTIAL_DIR`], and waits for it to end, killing it after `STOP_TIME`. [`poll`]
    /// then reports `Err`, unless rsync had finished already.
    ///
    /// [`poll`]: Self::poll
    pub fn stop(&mut self) {
        if self.exit.is_some() {
            return;
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            self.exit = Some((status, Instant::now()));
            return;
        }
        if !self.stopped {
            self.stopped = true;
            self.signal(rustix::process::Signal::TERM);
        }
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

    /// Sends `signal` to rsync's process group; only while rsync is not yet waited for,
    /// when the group's number cannot be another's.
    fn signal(&self, signal: rustix::process::Signal) {
        use rustix::process::{Pid, kill_process_group};
        let _ = kill_process_group(Pid::from_child(&self.child), signal);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads `pipe` on a thread of its own into `output` with `take`, marking stream `which`
/// closed at its end.
fn read_into(
    mut pipe: impl Read + Send + 'static,
    output: &Arc<Mutex<Output>>,
    which: usize,
    take: fn(&mut Output, &[u8]),
) -> io::Result<()> {
    let output = output.clone();
    std::thread::Builder::new().name("rsync output".to_owned()).spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match pipe.read(&mut buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(n) => n,
                Err(_) => 0,
            };
            let Ok(mut output) = output.lock() else { return };
            if n == 0 {
                output.closed[which] = true;
                return;
            }
            take(&mut output, &buf[..n]);
        }
    })?;
    Ok(())
}

/// rsync's error output, one message a line, without ssh's notes about new host keys.
fn messages(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    /// rsync 3.2.7 copying a directory, at 2 MB/s, with the options [`args`] gives: a
    /// directory, a file, a symbolic link, a name with a line break and an `é`.
    const FIRST: &str = "rse-file:12 d/\nrse-file:3000000 d/big\n\r          32768   1%    \
        0.00kB/s    0:00:00  \r        2097152  69%    1.96MB/s    0:00:00  \r        \
        3000000  99%    2.11MB/s    0:00:01 (xfr#1, to-chk=4/6)\nrse-file:34 d/sub/\n\
        rse-file:5 d/sub/link\nrse-file:1 d/sub/na me\\#012\\#303\\#251\n\r        3000001  \
        99%    2.11MB/s    0:00:01 (xfr#2, to-chk=1/6)\nrse-file:3 d/sub/small\n\r        \
        3000004  99%    2.11MB/s    0:00:01 (xfr#3, to-chk=0/6)\r        3000004  99%    \
        2.11MB/s    0:00:01 (xfr#3, to-chk=0/6)\r        3000004  99%    1.93MB/s    \
        0:00:01 (xfr#3, to-chk=0/6)\r        3000004  99%    1.93MB/s    0:00:01 (xfr#3, \
        to-chk=0/6)\n\nsent 3001003 bytes  received 82 bytes  2000723.33 bytes/sec\n\
        total size is 3000009  speedup is 1.00\n";

    /// The same again after adding a file and changing one: the others are up to date.
    const SECOND: &str = "rse-file:18 d/\nrse-file:1000000 d/mid\n\r          32768   0%    \
        0.00kB/s    0:00:00  \r        1000000  24%  307.47MB/s    0:00:00 (xfr#1, \
        to-chk=4/7)\nrse-file:8 d/sub/small\n\r        1000008  25%  230.61MB/s    0:00:00 \
        (xfr#2, to-chk=0/7)\r        1000008  25%  230.61MB/s    0:00:00 (xfr#2, to-chk=0/7)\n\
        \nsent 1000499 bytes  received 57 bytes  2001112.00 bytes/sec\n\
        total size is 4000014  speedup is 4.00\n";

    fn remote(root: &str) -> RsyncRemote {
        let ssh: Vec<String> =
            ["ssh", "-o", "ControlPath=/run/rse/%C-1a2b", "-p", "2222", "me@box"]
                .map(Into::into)
                .into();
        RsyncRemote::new(&ssh, root).unwrap()
    }

    fn strings(args: &[OsString]) -> Vec<&str> {
        args.iter().map(|a| a.to_str().unwrap()).collect()
    }

    const OPTIONS: [&str; 8] = [
        "-rlt",
        "--no-inc-recursive",
        "-s",
        "--partial-dir=.rse-partial",
        "--exclude=*.rse-partial",
        "--info=progress2,stats1",
        "--no-human-readable",
        "--out-format=rse-file:%l %n",
    ];

    #[test]
    fn arguments_for_each_way() {
        let box_ = remote("");
        let rsh = "--rsh=ssh -o ControlPath=/run/rse/%C-1a2b -p 2222";
        let home = Side::local(&Location::local(Path::new("/home/me")));
        let up = args(Op::Copy, &home, &box_.side("/srv/in"), "photos", false);
        let mut want = OPTIONS.to_vec();
        want.extend([rsh, "--", "/home/me/photos", "me@box:/srv/in/"]);
        assert_eq!(strings(&up), want);

        let down = args(Op::Move, &box_.side("notes"), &home, "a b.md", true);
        let mut want = OPTIONS.to_vec();
        want.extend(["--checksum", "--remove-source-files", rsh, "--", "me@box:notes/a b.md"]);
        want.push("/home/me/");
        assert_eq!(strings(&down), want);

        // The root of the local disk, and the login directory.
        let root = Side::local(&Location::local(Path::new("/")));
        let last = |a: Vec<OsString>| a[a.len() - 2..].to_vec();
        assert_eq!(
            strings(&last(args(Op::Copy, &root, &box_.side(""), "etc", false))),
            ["/etc", "me@box:./"]
        );
        assert_eq!(
            strings(&last(args(Op::Copy, &box_.side(""), &root, "x", false))),
            ["me@box:x", "/"]
        );
        // Local to local needs no remote shell.
        let local = args(Op::Copy, &root, &home, "etc", false);
        assert!(!strings(&local).iter().any(|a| a.starts_with("--rsh")));
    }

    #[test]
    fn paths_are_never_read_as_something_else() {
        let box_ = remote("");
        let arg = |path: &str| String::from_utf8(box_.side(path).arg("")).unwrap();
        assert_eq!(arg(""), "me@box:.");
        // A daemon's module, or its URL.
        assert_eq!(arg(":mod"), "me@box:./:mod");
        assert_eq!(arg("//srv"), "me@box:/srv");
        assert_eq!(arg("-rf"), "me@box:-rf");
        let local = |path: &str| String::from_utf8(Side::Local(path.into()).arg("x")).unwrap();
        assert_eq!(local("/a:b"), "/a:b/x");
        assert_eq!(local("box:dir"), "./box:dir/x");
        assert_eq!(local("-rf"), "./-rf/x");
        // A directory's trailing `/` would copy what is in it.
        let up = args(Op::Copy, &Side::Local("/a/".into()), &box_.side("b/"), "", false);
        assert_eq!(strings(&up[up.len() - 2..]), ["/a", "me@box:b/"]);
    }

    #[test]
    fn the_fs_root_is_joined_as_rclone_joins_it() {
        assert_eq!(remote("").path("docs"), "docs");
        assert_eq!(remote("").path("/srv"), "/srv");
        assert_eq!(remote("/srv").path("docs"), "/srv/docs");
        assert_eq!(remote("/srv/").path("docs"), "/srv/docs");
        assert_eq!(remote("data").path(""), "data");
        for (root, path) in [("", "docs"), ("/srv", "a/b"), ("rel/", "x")] {
            let loc = Location::new(format!("box:{root}"), path);
            let server = loc.fs_string().strip_prefix("box:").unwrap().to_owned();
            assert_eq!(remote(root).path(path), server);
        }
    }

    #[test]
    fn hosts_as_rsync_reads_them() {
        assert_eq!(host_spec("me@box").as_deref(), Some("me@box"));
        assert_eq!(host_spec("box").as_deref(), Some("box"));
        assert_eq!(host_spec("me@::1").as_deref(), Some("me@[::1]"));
        assert_eq!(host_spec("fe80::1%eth0").as_deref(), Some("[fe80::1%eth0]"));
        assert_eq!(host_spec("a@b@box").as_deref(), Some("a@b@box"));
        for bad in ["me@a/b", "us:er@box", "u/s@box", "[::1]", "me@", "@box", "x[y@box", "me@-oX"] {
            assert_eq!(host_spec(bad), None, "{bad}");
        }
        let ssh: Vec<String> = vec!["me@box".into()];
        assert_eq!(RsyncRemote::new(&ssh, ""), None, "no command");
    }

    #[test]
    fn remote_shell_quoting() {
        assert_eq!(rsh_quote("-p"), "-p");
        assert_eq!(rsh_quote(""), "''");
        assert_eq!(rsh_quote("/a b/key"), "'/a b/key'");
        assert_eq!(rsh_quote("it's"), "'it''s'");
        assert_eq!(rsh_quote("say \"hi\""), "'say \"hi\"'");
    }

    #[test]
    fn progress_of_a_whole_job() {
        let mut p = Progress::default();
        let cut = FIRST.find("\r        3000000").unwrap();
        p.feed(&FIRST.as_bytes()[..cut]);
        let s = p.stats();
        assert_eq!(s.bytes, 2_097_152);
        assert_eq!(p.total_bounds(), (2_995_932, Some(3_039_350)));
        assert_eq!(s.total_bytes, 3_017_641);
        assert!((s.speed - 1.96 * 1024.0 * 1024.0).abs() < 1.0, "{}", s.speed);
        let eta = s.eta.unwrap();
        assert!((eta - (3_017_641.0 - 2_097_152.0) / s.speed).abs() < 1e-9, "{eta}");
        assert_eq!(
            s.transferring,
            [Transferring { name: "d/big".into(), bytes: 2_097_152, size: 3_000_000 }]
        );
        assert_eq!(p.wire(), None);

        // On to the name with a line break and an `é`.
        let cut = FIRST.find("\n\r        3000001").unwrap();
        p.feed(&FIRST.as_bytes()[FIRST.find("\r        3000000").unwrap()..=cut]);
        let s = p.stats();
        assert_eq!(s.transfers, 1);
        assert_eq!(
            s.transferring,
            [Transferring { name: "d/sub/na me\né".into(), bytes: 0, size: 1 }]
        );

        p.feed(&FIRST.as_bytes()[cut + 1..]);
        let s = p.stats();
        assert_eq!((s.bytes, s.transfers), (3_000_004, 3));
        // The true total is 3000009.
        let (low, high) = p.total_bounds();
        assert!(low <= 3_000_009 && high.unwrap() >= 3_000_009, "{low} {high:?}");
        assert!((s.total_bytes - 3_000_009).abs() < 3_000_009 / 100, "{}", s.total_bytes);
        assert!(s.transferring.is_empty(), "every file done");
        assert_eq!(p.wire(), Some((3_001_003, 82)));
    }

    #[test]
    fn files_up_to_date_count_in_the_total_only() {
        let mut p = Progress::default();
        p.feed(SECOND.as_bytes());
        let s = p.stats();
        assert_eq!((s.bytes, s.transfers), (1_000_008, 2));
        let (low, high) = p.total_bounds();
        assert!(low <= 4_000_014 && high.unwrap() >= 4_000_014, "{low} {high:?}");
        assert_eq!(p.wire(), Some((1_000_499, 57)));
    }

    #[test]
    fn output_is_read_the_same_however_it_comes() {
        let mut whole = Progress::default();
        whole.feed(FIRST.as_bytes());
        for size in [1, 2, 7, 64] {
            let mut p = Progress::default();
            for chunk in FIRST.as_bytes().chunks(size) {
                p.feed(chunk);
            }
            assert_eq!(p.stats(), whole.stats(), "{size}");
        }
    }

    #[test]
    fn progress_lines() {
        let line = |t: &str| progress_line(t);
        assert_eq!(
            line("  1,234,567  45%  1.50kB/s    0:00:12 (xfr#3, ir-chk=10/20)"),
            Some(Line { bytes: 1_234_567, percent: 45, speed: 1536.0, done: Some(3) })
        );
        assert_eq!(
            line("0 0% 0.00kB/s ??:??:??"),
            Some(Line { bytes: 0, percent: 0, speed: 0.0, done: None })
        );
        assert_eq!(line("1 100% 2.00GB/s 1:02:03").map(|l| l.speed), Some(2.0 * 1024f64.powi(3)));
        for bad in [
            "",
            "1 101% 1.00kB/s 0:00:00",
            "1 +1% 1.00kB/s 0:00:00",
            "1 1% 1.00B/s 0:00:00",
            "1 1% infkB/s 0:00:00",
            "1 1% 1e3kB/s 0:00:00",
            "1 1% 1.00kB/s 0:00",
            "1 1% 1.00kB/s 0:00:0",
            "1 1% 1.00kB/s 0:0:00",
            "1 1% 1.00kB/s 0:00:00 (xfr#1, to-chk=1/2) more",
            "-1 1% 1.00kB/s 0:00:00",
            "99999999999999999999 1% 1.00kB/s 0:00:00",
            "total size is 3000009  speedup is 1.00",
        ] {
            assert_eq!(line(bad), None, "{bad}");
        }
        assert_eq!(total_bounds(0, 0), Some((1, None)));
        assert_eq!(total_bounds(u64::MAX, 1), Some((u64::MAX, Some(u64::MAX))));
        assert_eq!(total_bounds(7, 100), Some((7, Some(7))));
        assert_eq!(total_bounds(1, 50), Some((2, Some(2))));
        assert_eq!(total_bounds(1, 60), None, "no total has 1 byte as 60%");
        assert_eq!(total_bounds(0, 5), None);
    }

    #[test]
    fn escaped_names() {
        assert_eq!(unescape(b"a\\#012b"), "a\nb");
        assert_eq!(unescape(b"\\#303\\#251"), "é");
        // Not an escape: kept as it is.
        assert_eq!(unescape(b"\\#400 \\#01 \\#9"), "\\#400 \\#01 \\#9");
        assert_eq!(unescape(b"\\#134#"), "\\#");
    }

    // Running rsync itself, between two local directories.

    fn have_rsync() -> bool {
        usable()
    }

    #[test]
    fn only_rsync_3_1_on_is_used() {
        for (line, ok) in [
            ("rsync  version v3.2.7  protocol version 31", true),
            ("rsync  version 3.1.0  protocol version 31", true),
            ("rsync  version 4.0.0  protocol version 32", true),
            ("rsync  version 3.0.9  protocol version 30", false),
            ("rsync  version 2.6.9  protocol version 29", false),
            ("openrsync: protocol version 29", false),
            ("rsync version 2.6.9 compatible", false),
            ("rsync  version", false),
            ("", false),
        ] {
            assert_eq!(supported(format!("{line}\nCopyright\n").as_bytes()), ok, "{line}");
        }
    }

    fn finish(run: &mut Running) -> (Stats, Result<(), String>) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let (stats, Some(result)) = run.poll() {
                return (stats, result);
            }
            assert!(Instant::now() < deadline, "rsync did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether process `pid` is gone, not even a zombie.
    fn gone(pid: u32) -> bool {
        !Path::new(&format!("/proc/{pid}")).exists()
    }

    #[test]
    fn runs_rsync_to_its_end() {
        if !have_rsync() {
            eprintln!("skipped: no rsync");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        fs::create_dir_all(src.join("d/sub")).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src.join("d/a"), vec![7u8; 100_000]).unwrap();
        fs::write(src.join("d/sub/b"), "b").unwrap();
        let (from, to) = (Side::Local(src.clone()), Side::Local(dst.clone()));
        let mut run = Running::spawn(args(Op::Move, &from, &to, "d", false)).unwrap();
        let pid = run.child.id();
        let (stats, result) = finish(&mut run);
        assert_eq!(result, Ok(()));
        assert_eq!(stats.bytes, 100_001);
        assert_eq!(stats.transfers, 2);
        assert_eq!(fs::read(dst.join("d/a")).unwrap(), vec![7u8; 100_000]);
        assert_eq!(fs::read_to_string(dst.join("d/sub/b")).unwrap(), "b");
        assert!(!src.join("d/a").exists() && src.join("d/sub").is_dir(), "files moved, dirs left");
        assert!(run.wire().is_some());
        drop(run);
        assert!(gone(pid));

        // What rsync says when it fails.
        let mut run = Running::spawn(args(Op::Copy, &from, &to, "none", false)).unwrap();
        let (_, result) = finish(&mut run);
        let err = result.unwrap_err();
        assert!(err.contains("No such file or directory"), "{err}");
        let (stats, _) = run.poll();
        assert_eq!(stats.errors, 1, "{stats:?}");
    }

    #[test]
    fn a_stopped_run_keeps_its_partial_file() {
        if !have_rsync() {
            eprintln!("skipped: no rsync");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        let data: Vec<u8> = (0..4_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs::write(src.join("big"), &data).unwrap();
        let (from, to) = (Side::Local(src.clone()), Side::Local(dst.clone()));
        let mut slow = args(Op::Copy, &from, &to, "big", false);
        let at = slow.iter().position(|a| a == "--").unwrap();
        slow.insert(at, "--bwlimit=1000".into());
        let mut run = Running::spawn(slow).unwrap();
        let pid = run.child.id();
        let deadline = Instant::now() + Duration::from_secs(20);
        while run.poll().0.bytes < 1_000_000 {
            assert!(Instant::now() < deadline, "no progress");
            std::thread::sleep(Duration::from_millis(20));
        }
        run.stop();
        assert!(gone(pid), "waited for");
        let (stats, result) = run.poll();
        assert_eq!(result, Some(Err(tr!("rsync-stopped"))));
        assert!(stats.bytes >= 1_000_000);
        let partial = fs::metadata(dst.join(PARTIAL_DIR).join("big")).unwrap().len();
        assert!(partial >= 1_000_000, "{partial}");
        assert!(!dst.join("big").exists());

        let mut run = Running::spawn(args(Op::Copy, &from, &to, "big", false)).unwrap();
        assert_eq!(finish(&mut run).1, Ok(()));
        assert_eq!(fs::read(dst.join("big")).unwrap(), data);
        assert!(!dst.join(PARTIAL_DIR).exists(), "the partial directory is gone once used");
    }

    #[test]
    fn dropping_a_run_stops_it() {
        if !have_rsync() {
            eprintln!("skipped: no rsync");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("big"), vec![0u8; 2_000_000]).unwrap();
        fs::create_dir(dir.path().join("dst")).unwrap();
        let from = Side::Local(dir.path().to_owned());
        let mut slow = args(Op::Copy, &from, &Side::Local(dir.path().join("dst")), "big", false);
        slow.insert(0, "--bwlimit=100".into());
        let run = Running::spawn(slow).unwrap();
        let pid = run.child.id();
        std::thread::sleep(Duration::from_millis(300));
        drop(run);
        assert!(gone(pid));
    }
}
