//! Removing a directory with all it holds over sftp alone, for a server that runs no
//! command, so has no `rm -rf` to ask for: one that lets an account in for sftp only.
//!
//! sftp answers each request by its number, so many may be on their way at once. The
//! usual clients send one and wait for its answer, a round trip for every file; here up
//! to [`WINDOW`] requests are under way, in one session, so a tree of files takes its
//! number of round trips divided by that.
//!
//! What a server says is not taken on trust: an entry it calls a directory is asked about
//! again with `lstat`, which follows no link, before it is listed, so a link is removed
//! and never what it leads to; and a name that is not one path component is refused, so
//! nothing outside the entry is reached.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::librse::Failure;
use crate::librse::shell::ShellDir;

/// How many requests are under way at once.
pub const WINDOW: usize = 64;
/// The longest packet taken from a server: a listing's is some tens of kilobytes.
const MAX_PACKET: usize = 4 * 1024 * 1024;
/// How often a removal waiting for the server looks whether it was stopped.
const TICK: Duration = Duration::from_millis(100);
/// How long a server may take to answer the first packet.
const HELLO_TIME: Duration = Duration::from_secs(30);

const FXP_INIT: u8 = 1;
const FXP_VERSION: u8 = 2;
const FXP_CLOSE: u8 = 4;
const FXP_LSTAT: u8 = 7;
const FXP_OPENDIR: u8 = 11;
const FXP_READDIR: u8 = 12;
const FXP_REMOVE: u8 = 13;
const FXP_RMDIR: u8 = 15;
const FXP_STATUS: u8 = 101;
const FXP_HANDLE: u8 = 102;
const FXP_NAME: u8 = 104;
const FXP_ATTRS: u8 = 105;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;
const FX_NO_SUCH_FILE: u32 = 2;

const ATTR_SIZE: u32 = 1;
const ATTR_UIDGID: u32 = 2;
const ATTR_PERMISSIONS: u32 = 4;
const ATTR_ACMODTIME: u32 = 8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;

/// The command that starts the sftp subsystem on the machine `run` reaches (a command
/// that runs its last argument there, as ssh does).
pub fn subsystem(run: &[String]) -> Vec<String> {
    let mut command = run.to_vec();
    command.extend(["-s".to_owned(), "sftp".to_owned()]);
    command
}

/// One answer of the server.
struct Packet {
    kind: u8,
    id: u32,
    body: Vec<u8>,
}

/// What is read out of a packet's body, in order.
struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).and_then(|b| b.try_into().ok()).map(u32::from_be_bytes)
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }

    /// A file's attributes, of which its kind is read.
    fn attrs(&mut self) -> Option<Kind> {
        let flags = self.u32()?;
        if flags & ATTR_SIZE != 0 {
            self.take(8)?;
        }
        if flags & ATTR_UIDGID != 0 {
            self.take(8)?;
        }
        let mode = if flags & ATTR_PERMISSIONS != 0 { Some(self.u32()?) } else { None };
        if flags & ATTR_ACMODTIME != 0 {
            self.take(8)?;
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.string()?;
                self.string()?;
            }
        }
        Some(Kind(mode))
    }
}

/// A file's permissions, with the bits that tell its kind, when the server gave them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kind(Option<u32>);

impl Kind {
    fn is_dir(self) -> bool {
        self.0.is_some_and(|mode| mode & S_IFMT == S_IFDIR)
    }

    /// Not said, so to be asked about.
    fn unknown(self) -> bool {
        self.0.is_none()
    }
}

/// One sftp session: the program that carries it, and its answers as they come.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    answers: Receiver<io::Result<Packet>>,
    reader: Option<JoinHandle<()>>,
    next_id: u32,
}

impl Session {
    /// Starts `command`, which speaks sftp on its standard input and output, and greets
    /// the server.
    fn open(command: &[String], stop: &AtomicBool) -> Result<Self, Failure> {
        let (program, args) = command.split_first().ok_or(Failure::Internal)?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // A process group of its own: the terminal's signals stay away from it.
            .process_group(0)
            .spawn()
            .map_err(|e| Failure::Said(e.to_string()))?;
        let stdin = child.stdin.take();
        let (tx, answers) = mpsc::channel();
        let reader = child.stdout.take().and_then(|mut out| {
            std::thread::Builder::new()
                .name("sftp answers".to_owned())
                .spawn(move || {
                    loop {
                        let packet = read_packet(&mut out);
                        let ended = packet.is_err();
                        if tx.send(packet).is_err() || ended {
                            return;
                        }
                    }
                })
                .ok()
        });
        let mut session = Self { child, stdin, answers, reader, next_id: 0 };
        session.write(FXP_INIT, &3u32.to_be_bytes(), &[])?;
        let deadline = Instant::now() + HELLO_TIME;
        let hello = session.answer(stop, Some(deadline))?;
        if hello.kind != FXP_VERSION {
            return Err(Failure::Said("the server does not speak sftp".to_owned()));
        }
        Ok(session)
    }

    /// Sends one packet: its kind, `head` as it is, and each of `strings` with its length.
    fn write(&mut self, kind: u8, head: &[u8], strings: &[&[u8]]) -> Result<(), Failure> {
        let mut packet = vec![0, 0, 0, 0, kind];
        packet.extend_from_slice(head);
        for s in strings {
            let len = u32::try_from(s.len()).map_err(|_| Failure::Internal)?;
            packet.extend_from_slice(&len.to_be_bytes());
            packet.extend_from_slice(s);
        }
        let len = u32::try_from(packet.len() - 4).map_err(|_| Failure::Internal)?;
        packet[..4].copy_from_slice(&len.to_be_bytes());
        let stdin = self.stdin.as_mut().ok_or(Failure::Internal)?;
        stdin.write_all(&packet).and_then(|()| stdin.flush()).map_err(|e| lost(&e))
    }

    /// Sends a request about `path`, and returns its number.
    fn request(&mut self, kind: u8, path: &[u8]) -> Result<u32, Failure> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.write(kind, &id.to_be_bytes(), &[path])?;
        Ok(id)
    }

    /// The next answer, waited for until `deadline` if there is one; `Stopped` once
    /// `stop` is set.
    fn answer(&self, stop: &AtomicBool, deadline: Option<Instant>) -> Result<Packet, Failure> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped);
            }
            match self.answers.recv_timeout(TICK) {
                Ok(Ok(packet)) => return Ok(packet),
                Ok(Err(e)) => return Err(lost(&e)),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Said("the connection to the server ended".to_owned()));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|at| Instant::now() >= at) {
                        return Err(Failure::Said("the server does not answer".to_owned()));
                    }
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Its input closed, the server's side ends; the program here is not waited on
        // for longer than it takes to end it.
        self.stdin = None;
        // The whole group, not yet waited for, so its number is no other's: what the
        // program started holds its output open, which the reader waits on.
        let group = rustix::process::Pid::from_child(&self.child);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn lost(e: &io::Error) -> Failure {
    Failure::Said(format!("the connection to the server ended: {e}"))
}

fn read_packet(from: &mut impl Read) -> io::Result<Packet> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut len = [0u8; 4];
    from.read_exact(&mut len)?;
    let len = usize::try_from(u32::from_be_bytes(len)).map_err(|_| bad("packet too long"))?;
    if !(1..=MAX_PACKET).contains(&len) {
        return Err(bad("a packet of a length no sftp server sends"));
    }
    let mut data = vec![0u8; len];
    from.read_exact(&mut data)?;
    let kind = data[0];
    // The first packet has a version where the others have their request's number.
    let id = data.get(1..5).and_then(|b| b.try_into().ok()).map_or(0, u32::from_be_bytes);
    let body = data.get(5..).unwrap_or_default().to_vec();
    Ok(Packet { kind, id, body })
}

/// What a request under way is for.
enum Asked {
    /// Whether the path is a directory, before it is listed; `usize` is its depth.
    Kind(Vec<u8>, usize),
    Open(Vec<u8>, usize),
    Read(Vec<u8>, Vec<u8>, usize),
    Close,
    Remove(Vec<u8>),
    Rmdir(Vec<u8>),
}

/// What is still to ask.
enum Todo {
    Kind(Vec<u8>, usize),
    Open(Vec<u8>, usize),
    Read(Vec<u8>, Vec<u8>, usize),
    Close(Vec<u8>),
    Remove(Vec<u8>),
}

/// A status answer: `Ok` for success, and for a file that is not there when `gone` lets
/// that pass; otherwise what the server said of `path`.
fn status(packet: &Packet, path: &[u8], gone: bool) -> Result<u32, Failure> {
    let mut fields = Fields(&packet.body);
    let code = if packet.kind == FXP_STATUS { fields.u32() } else { None };
    let Some(code) = code else {
        return Err(Failure::Said("the server answered what was not asked".to_owned()));
    };
    if code == FX_OK || code == FX_EOF || (gone && code == FX_NO_SUCH_FILE) {
        return Ok(code);
    }
    let said = fields.string().map(String::from_utf8_lossy).unwrap_or_default();
    let said = if said.is_empty() { format!("sftp status {code}") } else { said.into_owned() };
    Err(Failure::Said(format!("{}: {said}", String::from_utf8_lossy(path))))
}

fn child_path(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

/// Removes `root` with all it holds, counting what is removed in `removed`.
fn remove_tree(
    session: &mut Session,
    root: &[u8],
    stop: &AtomicBool,
    removed: &AtomicU64,
) -> Result<(), Failure> {
    let mut todo = VecDeque::from([Todo::Kind(root.to_vec(), 0)]);
    let mut asked: HashMap<u32, Asked> = HashMap::new();
    // The directories found, with their depth: removed last, deepest first.
    let mut dirs: Vec<(Vec<u8>, usize)> = Vec::new();
    let mut first_error = None;
    while !todo.is_empty() || !asked.is_empty() {
        while asked.len() < WINDOW {
            let Some(next) = todo.pop_front() else { break };
            let (id, what) = match next {
                Todo::Kind(path, depth) => {
                    (session.request(FXP_LSTAT, &path)?, Asked::Kind(path, depth))
                }
                Todo::Open(path, depth) => {
                    (session.request(FXP_OPENDIR, &path)?, Asked::Open(path, depth))
                }
                Todo::Read(handle, dir, depth) => {
                    (session.request(FXP_READDIR, &handle)?, Asked::Read(handle, dir, depth))
                }
                Todo::Close(handle) => (session.request(FXP_CLOSE, &handle)?, Asked::Close),
                Todo::Remove(path) => (session.request(FXP_REMOVE, &path)?, Asked::Remove(path)),
            };
            asked.insert(id, what);
        }
        let packet = session.answer(stop, None)?;
        let Some(what) = asked.remove(&packet.id) else {
            return Err(Failure::Said("the server answered what was not asked".to_owned()));
        };
        match what {
            Asked::Kind(path, depth) => {
                if packet.kind == FXP_ATTRS {
                    if Fields(&packet.body).attrs().is_some_and(Kind::is_dir) {
                        todo.push_back(Todo::Open(path.clone(), depth));
                        dirs.push((path, depth));
                    } else {
                        todo.push_back(Todo::Remove(path));
                    }
                } else if let Err(e) = status(&packet, &path, true) {
                    first_error.get_or_insert(e);
                }
            }
            Asked::Open(path, depth) => {
                let handle =
                    if packet.kind == FXP_HANDLE { Fields(&packet.body).string() } else { None };
                match handle {
                    Some(handle) => todo.push_back(Todo::Read(handle.to_vec(), path, depth)),
                    None => {
                        if let Err(e) = status(&packet, &path, true) {
                            first_error.get_or_insert(e);
                        }
                    }
                }
            }
            Asked::Read(handle, dir, depth) => {
                if packet.kind == FXP_NAME {
                    for (name, kind) in names(&packet.body)? {
                        if name == b"." || name == b".." {
                            continue;
                        }
                        if !single_component(&name) {
                            return Err(Failure::Said(format!(
                                "the server listed {:?} in {}, which is no file's name",
                                String::from_utf8_lossy(&name),
                                String::from_utf8_lossy(&dir)
                            )));
                        }
                        let path = child_path(&dir, &name);
                        // What the server calls a directory is asked about again: a
                        // link to one must be removed, not listed.
                        if kind.is_dir() || kind.unknown() {
                            todo.push_back(Todo::Kind(path, depth + 1));
                        } else {
                            todo.push_back(Todo::Remove(path));
                        }
                    }
                    todo.push_back(Todo::Read(handle, dir, depth));
                } else {
                    if let Err(e) = status(&packet, &dir, false) {
                        first_error.get_or_insert(e);
                    }
                    todo.push_back(Todo::Close(handle));
                }
            }
            Asked::Close | Asked::Rmdir(_) => {}
            Asked::Remove(path) => match status(&packet, &path, true) {
                Ok(_) => {
                    removed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            },
        }
    }
    if let Some(e) = first_error {
        return Err(e);
    }
    remove_dirs(session, dirs, stop, removed)
}

/// Whether `name` is one path component, so that it names something in the directory
/// it was listed in and nowhere else. (`.` and `..` are passed over before.)
fn single_component(name: &[u8]) -> bool {
    !name.is_empty() && !name.contains(&b'/') && !name.contains(&0) && name != b"." && name != b".."
}

/// The entries of a listing: each name with its kind.
fn names(body: &[u8]) -> Result<Vec<(Vec<u8>, Kind)>, Failure> {
    let bad = || Failure::Said("the server sent a listing that cannot be read".to_owned());
    let mut fields = Fields(body);
    let count = fields.u32().ok_or_else(bad)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let name = fields.string().ok_or_else(bad)?;
        fields.string().ok_or_else(bad)?;
        let kind = fields.attrs().ok_or_else(bad)?;
        entries.push((name.to_vec(), kind));
    }
    Ok(entries)
}

/// Removes the emptied directories, the deepest first: those of one depth hold none of
/// each other, so they go together.
fn remove_dirs(
    session: &mut Session,
    mut dirs: Vec<(Vec<u8>, usize)>,
    stop: &AtomicBool,
    removed: &AtomicU64,
) -> Result<(), Failure> {
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.1));
    let mut asked: HashMap<u32, Asked> = HashMap::new();
    let mut dirs = dirs.into_iter().peekable();
    let mut depth = dirs.peek().map_or(0, |d| d.1);
    loop {
        while asked.len() < WINDOW {
            let Some((path, _)) = dirs.next_if(|d| d.1 == depth) else { break };
            asked.insert(session.request(FXP_RMDIR, &path)?, Asked::Rmdir(path));
        }
        if asked.is_empty() {
            match dirs.peek() {
                Some(next) => depth = next.1,
                None => return Ok(()),
            }
            continue;
        }
        let packet = session.answer(stop, None)?;
        match asked.remove(&packet.id) {
            Some(Asked::Rmdir(path)) => {
                status(&packet, &path, true)?;
                removed.fetch_add(1, Ordering::Relaxed);
            }
            _ => return Err(Failure::Said("the server answered what was not asked".to_owned())),
        }
    }
}

/// A removal at work, on a thread of its own. Dropping it stops the removal.
pub struct Removal {
    thread: Option<JoinHandle<Result<(), Failure>>>,
    ended: Arc<Mutex<Option<Result<(), Failure>>>>,
    stop: Arc<AtomicBool>,
    removed: Arc<AtomicU64>,
}

impl Removal {
    /// Starts removing `entry`, with all it holds, through `command`, which speaks sftp
    /// on its standard input and output ([`subsystem`]). `None` for a path that names
    /// nothing, which must never be removed: the root, or the login directory.
    pub fn spawn(command: Vec<String>, entry: &ShellDir) -> Option<io::Result<Self>> {
        let path = match entry {
            ShellDir::Absolute(path) if !path.trim_matches('/').is_empty() => path.clone(),
            ShellDir::Home(path) if !path.trim_matches('/').is_empty() => {
                // sftp starts in the login directory; `./` keeps a name from being read
                // as anything else.
                format!("./{path}")
            }
            _ => return None,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let removed = Arc::new(AtomicU64::new(0));
        let ended = Arc::new(Mutex::new(None));
        let (stopped, count, end) = (stop.clone(), removed.clone(), ended.clone());
        let thread = std::thread::Builder::new().name("sftp removal".to_owned()).spawn(move || {
            let result = Session::open(&command, &stopped).and_then(|mut session| {
                remove_tree(&mut session, path.as_bytes(), &stopped, &count)
            });
            // Stopped while the last answers came in: done all the same.
            if let Ok(mut end) = end.lock() {
                *end = Some(result.clone());
            }
            result
        });
        Some(thread.map(|thread| Self { thread: Some(thread), ended, stop, removed }))
    }

    /// How the removal ended, once it has.
    pub fn poll(&mut self) -> Option<Result<(), Failure>> {
        self.ended.lock().map_or(Some(Err(Failure::Internal)), |end| end.clone())
    }

    /// How many files and directories are removed so far.
    pub fn removed(&self) -> u64 {
        self.removed.load(Ordering::Relaxed)
    }

    /// Asks the removal to stop, without waiting: what is removed by then stays so.
    pub fn terminate(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stops the removal and waits for it to end.
    pub fn stop(&mut self) {
        self.terminate();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Removal {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use super::*;

    const SERVERS: [&str; 3] = [
        "/usr/lib/openssh/sftp-server",
        "/usr/libexec/openssh/sftp-server",
        "/usr/libexec/sftp-server",
    ];

    /// The sftp server of OpenSSH, which speaks sftp on its standard input and output.
    fn server() -> Option<Vec<String>> {
        let found = SERVERS.iter().find(|s| Path::new(s).exists());
        if found.is_none() {
            eprintln!("skipped: no sftp-server");
        }
        found.map(|s| vec![(*s).to_owned()])
    }

    fn absolute(path: &Path) -> ShellDir {
        ShellDir::Absolute(path.to_str().unwrap().to_owned())
    }

    fn finish(mut removal: Removal) -> (Result<(), Failure>, u64) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(result) = removal.poll() {
                return (result, removal.removed());
            }
            assert!(Instant::now() < deadline, "the removal did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn remove(command: Vec<String>, entry: &Path) -> (Result<(), Failure>, u64) {
        finish(Removal::spawn(command, &absolute(entry)).unwrap().unwrap())
    }

    #[test]
    fn a_tree_goes_with_all_it_holds_and_nothing_else() {
        let Some(server) = server() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("it's a $HOME `tree`\n -rf");
        let mut entries = 1;
        for d in 0..12 {
            let dir = tree.join(format!("dir {d}")).join("deep").join("er");
            fs::create_dir_all(&dir).unwrap();
            entries += 3;
            // More in one directory than one listing brings, and than the window.
            for f in 0..150 {
                fs::write(dir.join(format!("file {f}")), "x").unwrap();
                entries += 1;
            }
        }
        fs::create_dir(tree.join("empty")).unwrap();
        entries += 1;
        // Links are removed, never what they lead to: a file, a directory, nothing.
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("kept dir")).unwrap();
        fs::write(outside.join("kept dir/kept file"), "x").unwrap();
        symlink(outside.join("kept dir"), tree.join("to a directory")).unwrap();
        symlink(outside.join("kept dir/kept file"), tree.join("to a file")).unwrap();
        symlink("nowhere", tree.join("dangling")).unwrap();
        entries += 3;
        fs::write(tmp.path().join("beside"), "x").unwrap();

        let (result, removed) = remove(server, &tree);
        assert_eq!(result, Ok(()));
        assert_eq!(removed, entries);
        assert!(fs::symlink_metadata(&tree).is_err(), "the tree is gone");
        assert!(outside.join("kept dir/kept file").exists(), "what a link led to stays");
        assert!(tmp.path().join("beside").exists());
    }

    #[test]
    fn a_file_a_link_and_what_is_not_there() {
        let Some(server) = server() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let kept = tmp.path().join("kept");
        fs::create_dir(&kept).unwrap();
        fs::write(kept.join("in"), "x").unwrap();
        let (file, link) = (tmp.path().join("file"), tmp.path().join("link"));
        fs::write(&file, "x").unwrap();
        symlink(&kept, &link).unwrap();
        assert_eq!(remove(server.clone(), &file), (Ok(()), 1));
        // The link to a directory, not the directory.
        assert_eq!(remove(server.clone(), &link), (Ok(()), 1));
        assert!(fs::symlink_metadata(&link).is_err() && kept.join("in").exists());
        assert_eq!(remove(server, &tmp.path().join("missing")), (Ok(()), 0));
    }

    #[test]
    fn a_path_that_names_nothing_is_never_removed() {
        let command = vec!["/nonexistent".to_owned()];
        for dir in ["", "/", "//"] {
            let entry = ShellDir::Absolute(dir.to_owned());
            assert!(Removal::spawn(command.clone(), &entry).is_none(), "{dir:?}");
        }
        for dir in ["", "/"] {
            assert!(Removal::spawn(command.clone(), &ShellDir::Home(dir.to_owned())).is_none());
        }
    }

    #[test]
    fn a_path_in_the_login_directory_starts_there() {
        let Some(server) = server() else { return };
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("-rf/in")).unwrap();
        // The server started in the directory that stands for the login directory.
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("cd \"$1\" && exec {}", server[0]),
            "sh".to_owned(),
            tmp.path().to_str().unwrap().to_owned(),
        ];
        let removal = Removal::spawn(command, &ShellDir::Home("-rf".to_owned())).unwrap().unwrap();
        assert_eq!(finish(removal), (Ok(()), 2));
        assert!(!tmp.path().join("-rf").exists());
    }

    #[test]
    fn what_cannot_be_removed_is_said_and_the_rest_goes() {
        use std::os::unix::fs::PermissionsExt;
        let Some(server) = server() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("locked")).unwrap();
        fs::write(tree.join("locked/file"), "x").unwrap();
        fs::write(tree.join("free"), "x").unwrap();
        fs::set_permissions(tree.join("locked"), fs::Permissions::from_mode(0o500)).unwrap();
        let (result, _) = remove(server, &tree);
        fs::set_permissions(tree.join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
        // Root removes it all the same.
        if tree.exists() {
            let said = result.unwrap_err().to_string();
            assert!(said.contains("locked/file"), "{said}");
            assert!(!tree.join("free").exists(), "the rest went");
        }
    }

    #[test]
    fn a_program_that_speaks_no_sftp_is_an_error() {
        let command = vec!["/bin/sh".to_owned(), "-c".to_owned(), "echo hello; sleep 5".to_owned()];
        let entry = ShellDir::Absolute("/tmp/x".to_owned());
        let (result, _) = finish(Removal::spawn(command, &entry).unwrap().unwrap());
        assert!(result.is_err());
        let gone = vec!["/nonexistent/program".to_owned()];
        assert!(finish(Removal::spawn(gone, &entry).unwrap().unwrap()).0.is_err());
    }

    #[test]
    fn a_stopped_removal_ends_at_once() {
        // A server that greets and then never answers.
        let hello = r"printf '\000\000\000\005\002\000\000\000\003'; sleep 60";
        let command = vec!["/bin/sh".to_owned(), "-c".to_owned(), hello.to_owned()];
        let entry = ShellDir::Absolute("/tmp/x".to_owned());
        let mut removal = Removal::spawn(command, &entry).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(removal.poll(), None);
        let begun = Instant::now();
        removal.stop();
        assert!(begun.elapsed() < Duration::from_secs(5));
        assert_eq!(removal.poll(), Some(Err(Failure::Stopped)));
    }

    fn string(s: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(s.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(s);
        out
    }

    /// A name that leads out of the directory it was listed in is no entry of it.
    #[test]
    fn a_listing_is_read_and_only_single_components_are_names() {
        for name in ["../../etc", "a/b", "/etc", "", "x\0y", "..", "."] {
            assert!(!single_component(name.as_bytes()), "{name:?}");
        }
        for name in ["a", "...", "a b", "-rf", "it's"] {
            assert!(single_component(name.as_bytes()), "{name:?}");
        }

        let mut body = 1u32.to_be_bytes().to_vec();
        body.extend(string(b"../../etc"));
        body.extend(string(b"long"));
        body.extend(ATTR_PERMISSIONS.to_be_bytes());
        body.extend(0o100_644u32.to_be_bytes());
        let listed = names(&body).unwrap();
        assert_eq!(listed, [(b"../../etc".to_vec(), Kind(Some(0o100_644)))]);
        // Cut short, it is no listing at all.
        assert!(names(&body[..body.len() - 2]).is_err());
        assert!(Kind(Some(0o040_755)).is_dir() && !Kind(Some(0o120_777)).is_dir());
        assert!(!Kind(None).is_dir() && Kind(None).unknown());
    }
}
