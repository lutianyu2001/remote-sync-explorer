//! The terminal under the panes: the user's own shell where a pane is, on the local disk
//! or on a server, started in the pane's directory and following it there.
//!
//! On a server it runs `ssh -tt` through the master connection rse already holds for it
//! ([`SystemSsh::shell`](crate::rclone::system_ssh::SystemSsh::shell)), so it never logs
//! in again; on the local disk, sh. Either runs in a pseudo-terminal of its own whose
//! output [`vt100`] keeps as a screen.
//!
//! Starting clean: before the shell, the server runs a short sh script
//! ([`remote_command`]) that reports the shell's pid and name in an escape sequence of
//! rse's own, goes to the directory and starts the user's login shell. What came before
//! the report is wiped. bash, zsh and fish then get Ctrl+L as their first input, which
//! clears what their startup files printed once the prompt is up; the screen is shown
//! from that clear on (or after [`CLEAR_WAIT`], should it not come).
//!
//! Following the pane: a `cd` is typed into the shell only while the shell itself is in
//! the foreground of its terminal (`ps -o tpgid=`, asked through the master), never into
//! a program it runs; while one runs, the `cd` waits and is tried again every
//! [`RETRY`]. A directory whose name holds a control character is never typed: the
//! shell would read a line break in it as the end of the command.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::num::NonZeroU16;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::event::Event;

pub type TermId = u64;

/// How long the screen stays hidden waiting for the clear Ctrl+L asks for.
pub const CLEAR_WAIT: Duration = Duration::from_secs(2);
/// How long the screen stays hidden waiting for the shell to report itself.
pub const START_WAIT: Duration = Duration::from_secs(5);
/// How often a `cd` waiting for a running program to end is tried again.
pub const RETRY: Duration = Duration::from_secs(1);
/// Lines kept above the screen, for the wheel.
const SCROLLBACK: usize = 1000;
/// The OSC sequence number the start script reports the shell with.
const REPORT: &[u8] = b"7799";

/// How to reach a shell on a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTarget {
    /// The command that opens a terminal on the server; the remote command goes last.
    pub open: Vec<String>,
    /// A command that runs its last argument on the server, never asking anything.
    pub run: Vec<String>,
    /// The fs root on the server, as rclone's sftp backend reads it: absolute, or relative
    /// to the login directory.
    pub root: String,
    /// Environment for `open` beyond rse's own.
    pub env: Vec<(String, String)>,
}

/// A directory on the server, as the shell reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellDir {
    Absolute(String),
    /// Relative to the login directory (empty for that directory itself).
    Home(String),
}

impl ShellTarget {
    /// The local disk, from `root` (`/`).
    pub fn local(root: &str) -> Self {
        let sh = vec!["/bin/sh".to_owned(), "-c".to_owned()];
        Self { open: sh.clone(), run: sh, root: root.to_owned(), env: Vec::new() }
    }

    /// The directory of a pane's path (a [`Location`](crate::location::Location)'s).
    pub fn dir(&self, path: &str) -> ShellDir {
        let joined = crate::rsync::join(&self.root, path);
        if joined.starts_with('/') { ShellDir::Absolute(joined) } else { ShellDir::Home(joined) }
    }
}

impl ShellDir {
    fn path(&self) -> &str {
        match self {
            Self::Absolute(p) | Self::Home(p) => p,
        }
    }

    /// How the window's title shows it: `/srv/www`, `~/photos`, `~`.
    pub fn shown(&self) -> String {
        match self {
            Self::Absolute(p) => p.clone(),
            Self::Home(p) if p.is_empty() => "~".to_owned(),
            Self::Home(p) => format!("~/{p}"),
        }
    }
}

/// What the server runs: `exec /bin/sh -c '…'`, which every common login shell (sh,
/// bash, zsh, fish, tcsh) hands to sh unchanged, since the script holds no quote and no
/// backslash but before a digit. The script reports the shell, goes to `dir` (spelt in
/// octal escapes, so any name passes) and starts the user's login shell, keeping the pid.
pub fn remote_command(dir: &ShellDir) -> String {
    let mut script = String::from(r#"printf "\033]7799;rse;%s;%s\007" "$$" "${SHELL##*/}"; "#);
    let path = dir.path();
    if !path.is_empty() {
        let mut octal = String::new();
        for b in path.bytes() {
            let _ = write!(octal, "\\0{b:03o}");
        }
        // The `x` keeps a name ending in a line break whole through `$(…)`.
        let _ =
            write!(script, r#"d=$(printf "%b" "{octal}"; echo x); cd -- "${{d%x}}" 2>/dev/null; "#);
    }
    script.push_str(r#"exec "${SHELL:-/bin/sh}" -l"#);
    format!("exec /bin/sh -c '{script}'")
}

/// What is typed into the shell to go to `dir`: Ctrl+E Ctrl+U drop a half-typed line,
/// and the leading space keeps the command out of the history. `None` when the name
/// holds a control character.
pub fn cd_input(dir: &ShellDir, shell: &str) -> Option<String> {
    let path = dir.path();
    if path.contains(char::is_control) {
        return None;
    }
    let target = match dir {
        ShellDir::Absolute(p) => quote(p, shell),
        ShellDir::Home(p) if p.is_empty() => r#""$HOME""#.to_owned(),
        ShellDir::Home(p) => format!(r#""$HOME"/{}"#, quote(p, shell)),
    };
    Some(format!("\x05\x15 cd {target}\r"))
}

/// `s` in single quotes for `shell`: fish reads `\\` in them as one backslash, and the
/// csh family expands `!` even there.
fn quote(s: &str, shell: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str(r"'\''"),
            '\\' if shell == "fish" => out.push_str(r"\\"),
            '!' if matches!(shell, "tcsh" | "csh") => out.push_str(r"\!"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The shell the start script reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub pid: u32,
    pub name: String,
}

/// Answers the questions programs ask the terminal, and catches the start script's report.
#[derive(Default)]
struct Hooks {
    replies: Vec<u8>,
    report: Option<Shell>,
    reported: bool,
}

impl vt100::Callbacks for Hooks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            // Device attributes: a VT102.
            (None, 'c', 0) => self.replies.extend_from_slice(b"\x1b[?6c"),
            (Some(b'>'), 'c', 0) => self.replies.extend_from_slice(b"\x1b[>0;0;0c"),
            (None, 'n', 5) => self.replies.extend_from_slice(b"\x1b[0n"),
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                let answer = format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1);
                self.replies.extend_from_slice(answer.as_bytes());
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // Only the first: a program could print another later.
        if self.reported {
            return;
        }
        if let [REPORT, b"rse", pid, name] = params {
            self.reported = true;
            let pid = std::str::from_utf8(pid).ok().and_then(|p| p.parse().ok());
            let name = String::from_utf8_lossy(name);
            let name: String =
                name.chars().filter(char::is_ascii_graphic).take(32).collect::<String>();
            self.report = pid.map(|pid| Shell { pid, name });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// Waiting for the start script's report.
    Connecting(Instant),
    /// Ctrl+L sent; waiting for the clear.
    Clearing(Instant),
    Ready,
}

/// A shell running in a pseudo-terminal.
pub struct Session {
    pub id: TermId,
    target: ShellTarget,
    parser: vt100::Parser<Hooks>,
    master: File,
    child: Child,
    shell: Option<Shell>,
    start: Start,
    /// Where the shell is, as far as rse knows, and where the pane wants it.
    at: ShellDir,
    wanted: ShellDir,
    checking: bool,
    retry_at: Option<Instant>,
    /// Lines scrolled back with the wheel.
    pub scroll: usize,
    events: Sender<Event>,
}

impl Session {
    /// Opens a shell in `dir` with a screen of `rows` × `cols`. Its output arrives as
    /// [`Event::TermOutput`], and [`Event::TermClosed`] when it ends.
    ///
    /// # Errors
    /// Fails when no pseudo-terminal can be had or the command does not start.
    pub fn open(
        id: TermId,
        target: ShellTarget,
        dir: ShellDir,
        (rows, cols): (u16, u16),
        events: Sender<Event>,
        now: Instant,
    ) -> io::Result<Self> {
        use rustix::pty::{grantpt, ptsname, unlockpt};
        let (rows, cols) = (nonzero(rows), nonzero(cols));
        let master = open_master()?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOCTTY.bits().cast_signed())
            .open(std::ffi::OsStr::new(&*name.to_string_lossy()))?;
        set_winsize(&master, rows.get(), cols.get())?;
        let (program, args) =
            target.open.split_first().ok_or_else(|| io::Error::other("no command"))?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .arg(remote_command(&dir))
            .envs(target.env.iter().map(|(k, v)| (k, v)))
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        on_its_own_terminal(&mut cmd);
        let child = cmd.spawn()?;
        let master = File::from(master);
        let mut reader = master.try_clone()?;
        let tx = events.clone();
        thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    // EIO once the last process on the terminal ends.
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Event::TermClosed { id });
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Event::TermOutput { id, bytes: buf[..n].to_vec() }).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let parser = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, Hooks::default());
        Ok(Self {
            id,
            target,
            parser,
            master,
            child,
            shell: None,
            start: Start::Connecting(now),
            at: dir.clone(),
            wanted: dir,
            checking: false,
            retry_at: None,
            scroll: 0,
            events,
        })
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Whether the screen is shown: not while the shell starts.
    pub fn shown(&self) -> bool {
        self.start == Start::Ready
    }

    /// Where the shell is, as far as rse knows.
    pub fn dir(&self) -> &ShellDir {
        &self.at
    }

    pub fn target(&self) -> &ShellTarget {
        &self.target
    }

    /// Takes the shell's output.
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        let cleared = bytes.windows(4).any(|w| w == b"\x1b[2J") || bytes.ends_with(b"\x1b[J");
        self.parser.process(bytes);
        let hooks = self.parser.callbacks_mut();
        let replies = std::mem::take(&mut hooks.replies);
        let report = hooks.report.take();
        if !replies.is_empty() {
            self.write(&replies);
        }
        if let Some(shell) = report {
            // A banner or message of the day came before: wipe it.
            self.parser.process(b"\x1b[H\x1b[2J");
            self.parser.screen_mut().clear_scrollback();
            if matches!(shell.name.as_str(), "bash" | "zsh" | "fish") {
                self.write(b"\x0c");
                self.start = Start::Clearing(now);
            } else {
                self.start = Start::Ready;
            }
            self.shell = Some(shell);
        } else if cleared && matches!(self.start, Start::Clearing(_)) {
            self.parser.screen_mut().clear_scrollback();
            self.start = Start::Ready;
        }
        if self.start == Start::Ready {
            self.follow(now);
        }
    }

    /// Shows the screen once the shell has had its time to start, and tries a waiting
    /// `cd` again.
    pub fn tick(&mut self, now: Instant) {
        match self.start {
            Start::Connecting(t) if now.saturating_duration_since(t) >= START_WAIT => {
                self.start = Start::Ready;
            }
            Start::Clearing(t) if now.saturating_duration_since(t) >= CLEAR_WAIT => {
                self.start = Start::Ready;
            }
            _ => {}
        }
        if self.start == Start::Ready {
            self.follow(now);
        }
    }

    /// Sends the shell to `dir`, as soon as nothing runs in it.
    pub fn want(&mut self, dir: ShellDir, now: Instant) {
        self.wanted = dir;
        self.retry_at = None;
        if self.start == Start::Ready {
            self.follow(now);
        }
    }

    fn follow(&mut self, now: Instant) {
        let Some(shell) = &self.shell else { return };
        if self.wanted == self.at || self.checking || self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        self.checking = true;
        let (run, pid, id, tx) = (self.target.run.clone(), shell.pid, self.id, self.events.clone());
        thread::spawn(move || {
            let idle = foreground_is(&run, pid);
            let _ = tx.send(Event::TermIdle { id, idle });
        });
    }

    /// Whether the shell was in the foreground when asked (`None`: it could not be told).
    /// Types the `cd` when it was; returns a note for the user when it cannot be typed.
    pub fn idle(&mut self, idle: Option<bool>, now: Instant) -> Option<String> {
        self.checking = false;
        let name = self.shell.as_ref().map_or("", |s| s.name.as_str());
        match idle {
            Some(true) => {
                let input = cd_input(&self.wanted, name);
                self.at = self.wanted.clone();
                match input {
                    Some(input) => {
                        self.scroll = 0;
                        self.write(input.as_bytes());
                        None
                    }
                    None => Some(tr!("terminal-cd-control")),
                }
            }
            Some(false) => {
                self.retry_at = Some(now + RETRY);
                None
            }
            None => {
                self.at = self.wanted.clone();
                Some(tr!("terminal-cd-unknown"))
            }
        }
    }

    /// Types `bytes`, back at the bottom of the screen.
    pub fn input(&mut self, bytes: &[u8]) {
        self.scroll = 0;
        self.parser.screen_mut().set_scrollback(0);
        self.write(bytes);
    }

    /// Scrolls the screen back by `lines` (forward when negative), as far as there are lines.
    pub fn scroll_by(&mut self, lines: isize) {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(self.scroll.saturating_add_signed(lines));
        self.scroll = screen.scrollback();
    }

    fn write(&mut self, bytes: &[u8]) {
        // A shell that stopped reading only loses keys; rse goes on.
        let _ = self.master.write_all(bytes);
    }

    /// Resizes the screen to `rows` × `cols`, telling the programs on it.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (nonzero(rows), nonzero(cols));
        let (r, c) = self.parser.screen().size();
        if (r, c) == (rows, cols) {
            return;
        }
        self.parser.set_size(rows, cols);
        // The kernel tells the programs on it (SIGWINCH).
        let _ = set_winsize(&self.master, rows.get(), cols.get());
        self.scroll = self.parser.screen().scrollback();
    }

    /// The shell has ended: why, when it ended before starting (the last line it printed,
    /// such as ssh's error).
    pub fn closed(&mut self) -> Option<String> {
        let _ = self.child.wait();
        if self.shell.is_some() {
            return None;
        }
        let contents = self.parser.screen().contents();
        let last = contents.lines().rev().find(|l| !l.trim().is_empty())?;
        Some(last.trim().to_owned())
    }
}

impl Drop for Session {
    /// Hangs up, as closing a terminal window does, so that the shell ends its jobs too;
    /// kills what is still there a moment later.
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(pid) =
            i32::try_from(self.child.id()).ok().and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::HUP);
        }
        for _ in 0..HANGUP_WAIT_STEPS {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How long a hung-up shell gets to end, in steps of 10 ms.
const HANGUP_WAIT_STEPS: u32 = 20;

/// Starts `cmd` in a session of its own whose controlling terminal is its stdin, the
/// pseudo-terminal: away from rse's own terminal, which it must never read, and with
/// what a shell needs of its terminal (Ctrl+C, job control, the size changing).
#[allow(unsafe_code)]
fn on_its_own_terminal(cmd: &mut Command) {
    let attach = || {
        rustix::process::setsid()?;
        rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
        Ok(())
    };
    // SAFETY: `attach` makes two system calls, setsid and the TIOCSCTTY ioctl, and
    // allocates nothing, its errors included (rustix's Errno becomes
    // io::Error::from_raw_os_error), so it may run between fork and exec: on Linux rustix
    // makes the calls itself (its linux_raw backend), on macOS through libc's thin
    // wrappers, which make the system call and nothing else (ioctl: xnu-12377.121.6,
    // libsyscall/wrappers/ioctl.c), as BSD's login_tty does after forkpty's fork (Apple's
    // Libc-1752.120.2, util/login_tty.c and util/pty.c). std has made stdin the
    // pseudo-terminal by then.
    unsafe { cmd.pre_exec(attach) };
}

/// Opens the master side of a new pseudo-terminal, close-on-exec.
fn open_master() -> io::Result<rustix::fd::OwnedFd> {
    // Close-on-exec from the moment it is open, so that no command another thread starts
    // meanwhile inherits it. posix_openpt takes O_CLOEXEC on Linux and the BSDs that
    // have it. macOS's is open("/dev/ptmx", flags) (Libc-1752.120.2, stdlib/grantpt.c),
    // so it would take it too, but rustix leaves the flag out there: the same call, with
    // it. Elsewhere the flag is set once the terminal is open, which leaves a moment in
    // which a command another thread starts inherits it; rse is built for Linux and macOS.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd"
    ))]
    let master = {
        use rustix::pty::{OpenptFlags, openpt};
        openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?
    };
    #[cfg(target_os = "macos")]
    let master = rustix::fs::open(
        "/dev/ptmx",
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "macos"
    )))]
    let master = {
        use rustix::pty::{OpenptFlags, openpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
        master
    };
    Ok(master)
}

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

fn set_winsize(fd: impl std::os::fd::AsFd, rows: u16, cols: u16) -> io::Result<()> {
    let size = rustix::termios::Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    Ok(rustix::termios::tcsetwinsize(fd, size)?)
}

/// Whether process `pid`'s terminal has it in the foreground, asked with `run`.
fn foreground_is(run: &[String], pid: u32) -> Option<bool> {
    let (program, args) = run.split_first()?;
    let out = Command::new(program)
        .args(args)
        .arg(format!("ps -o tpgid= -p {pid}"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let tpgid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(tpgid == pid)
}

/// The bytes a key sends to the programs on the terminal, as xterm sends them;
/// `app_cursor` when they asked for the arrows' application mode.
pub fn key_bytes(key: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // xterm's modifier parameter: 1 + Shift 1 + Alt 2 + Ctrl 4.
    let m = 1
        + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(alt)
        + 4 * u8::from(ctrl);
    let csi = |end: char| {
        if m > 1 {
            format!("\x1b[1;{m}{end}")
        } else if app_cursor {
            format!("\x1bO{end}")
        } else {
            format!("\x1b[{end}")
        }
    };
    let tilde = |n: u8| if m > 1 { format!("\x1b[{n};{m}~") } else { format!("\x1b[{n}~") };
    let ss3 = |end: char| if m > 1 { format!("\x1b[1;{m}{end}") } else { format!("\x1bO{end}") };
    let text = match key.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            c @ 'a'..='z' => char::from(c as u8 - b'a' + 1).to_string(),
            ' ' | '@' | '2' => "\0".to_owned(),
            '[' | '3' => "\x1b".to_owned(),
            '\\' | '4' => "\x1c".to_owned(),
            ']' | '5' => "\x1d".to_owned(),
            '^' | '6' => "\x1e".to_owned(),
            '_' | '/' | '7' => "\x1f".to_owned(),
            '8' | '?' => "\x7f".to_owned(),
            c => c.to_string(),
        },
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".to_owned(),
        KeyCode::Tab => "\t".to_owned(),
        KeyCode::BackTab => "\x1b[Z".to_owned(),
        KeyCode::Backspace => if ctrl { "\x08" } else { "\x7f" }.to_owned(),
        KeyCode::Esc => "\x1b".to_owned(),
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => ss3(char::from(b'P' + n - 1)),
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        _ => return Vec::new(),
    };
    let mut bytes = Vec::new();
    // Alt puts ESC before a character; the other keys carry it in their parameter.
    if alt && matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace) {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(root: &str) -> ShellTarget {
        ShellTarget { open: vec![], run: vec![], root: root.to_owned(), env: vec![] }
    }

    /// The master side is close-on-exec, on Linux and on macOS: that it is so from the
    /// moment it is open, the flag being given to the call that opens it, no test can see.
    #[test]
    fn the_terminal_is_closed_on_exec_from_the_start() {
        let master = super::open_master().unwrap_or_else(|e| panic!("{e}"));
        let flags = rustix::io::fcntl_getfd(&master).unwrap_or_else(|e| panic!("{e}"));
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    }

    #[test]
    fn directories_join_the_root() {
        assert_eq!(target("").dir(""), ShellDir::Home(String::new()));
        assert_eq!(target("").dir("photos"), ShellDir::Home("photos".into()));
        assert_eq!(target("/srv").dir("www"), ShellDir::Absolute("/srv/www".into()));
        assert_eq!(target("/").dir("etc"), ShellDir::Absolute("/etc".into()));
        assert_eq!(ShellDir::Home(String::new()).shown(), "~");
        assert_eq!(ShellDir::Home("a".into()).shown(), "~/a");
    }

    /// The script passes every login shell unchanged: no quote, and a backslash only
    /// before a digit.
    #[test]
    fn the_start_script_needs_no_escaping() {
        let cmd = remote_command(&ShellDir::Absolute("/it's a \\ \"dir\"\n$(x)".into()));
        let script = cmd.strip_prefix("exec /bin/sh -c '").and_then(|s| s.strip_suffix('\''));
        let script = script.unwrap();
        assert!(!script.contains('\''), "{script}");
        for (i, _) in script.match_indices('\\') {
            assert!(script.as_bytes()[i + 1].is_ascii_digit(), "{script}");
        }
    }

    /// The script, run by sh as the server would, reports the shell and goes to the
    /// directory, whatever its name.
    #[test]
    fn the_start_script_reports_and_goes_to_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let name = "it's a \\ \"dir\" $(x) %s\n";
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        // pwd in place of the shell, to see where it would start.
        let script = remote_command(&ShellDir::Absolute(dir.to_string_lossy().into_owned()))
            .replace(r#"exec "${SHELL:-/bin/sh}" -l"#, "pwd");
        let out = Command::new("/bin/sh").arg("-c").arg(script).output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.starts_with("\x1b]7799;rse;"), "{text:?}");
        assert!(text.ends_with(&format!("{}\n", dir.display())), "{text:?}");
    }

    #[test]
    fn cd_is_quoted_for_the_shell() {
        let dir = ShellDir::Absolute("/srv/it's \\ !".into());
        assert_eq!(cd_input(&dir, "bash").unwrap(), "\x05\x15 cd '/srv/it'\\''s \\ !'\r");
        assert_eq!(cd_input(&dir, "fish").unwrap(), "\x05\x15 cd '/srv/it'\\''s \\\\ !'\r");
        assert_eq!(cd_input(&dir, "tcsh").unwrap(), "\x05\x15 cd '/srv/it'\\''s \\ \\!'\r");
        assert_eq!(
            cd_input(&ShellDir::Home("a b".into()), "zsh").unwrap(),
            "\x05\x15 cd \"$HOME\"/'a b'\r"
        );
        assert_eq!(
            cd_input(&ShellDir::Home(String::new()), "sh").unwrap(),
            "\x05\x15 cd \"$HOME\"\r"
        );
    }

    /// A line break in a name would end the command there and run the rest.
    #[test]
    fn names_with_control_characters_are_never_typed() {
        assert_eq!(cd_input(&ShellDir::Absolute("/x\nrm -rf ~".into()), "bash"), None);
        assert_eq!(cd_input(&ShellDir::Home("x\x1b[A".into()), "bash"), None);
    }

    #[test]
    fn keys_become_xterm_bytes() {
        let k = |code, mods| key_bytes(&KeyEvent::new(code, mods), false);
        let none = KeyModifiers::NONE;
        assert_eq!(k(KeyCode::Char('a'), none), b"a");
        assert_eq!(k(KeyCode::Char('é'), none), "é".as_bytes());
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), b"\x03");
        assert_eq!(k(KeyCode::Char('x'), KeyModifiers::ALT), b"\x1bx");
        assert_eq!(k(KeyCode::Enter, none), b"\r");
        assert_eq!(k(KeyCode::Backspace, none), b"\x7f");
        assert_eq!(k(KeyCode::Up, none), b"\x1b[A");
        assert_eq!(key_bytes(&KeyEvent::new(KeyCode::Up, none), true), b"\x1bOA");
        assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(k(KeyCode::Delete, none), b"\x1b[3~");
        assert_eq!(k(KeyCode::F(1), none), b"\x1bOP");
        assert_eq!(k(KeyCode::F(12), none), b"\x1b[24~");
    }

    #[test]
    fn the_report_is_taken_once() {
        let mut p = vt100::Parser::new_with_callbacks(nonzero(5), nonzero(20), 0, Hooks::default());
        p.process(b"motd\x1b]7799;rse;42;bash\x07$ ");
        assert_eq!(p.callbacks_mut().report.take(), Some(Shell { pid: 42, name: "bash".into() }));
        p.process(b"\x1b]7799;rse;1;sh\x07");
        assert_eq!(p.callbacks_mut().report.take(), None);
        p.process(b"\x1b[6n\x1b[c");
        assert_eq!(p.callbacks_mut().replies, b"\x1b[1;7R\x1b[?6c");
    }
}
