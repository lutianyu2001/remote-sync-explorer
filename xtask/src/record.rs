//! What the runs of each task leave in `audit/<task>/`: the latest run, and when that
//! failed, the last that succeeded in `last_succeeded/` under it. Each holds `run.log`
//! (what the run printed), `run.txt` (when, on which commit, with which tools, and how
//! it ended), `inputs.txt` (the files it read, with hashes of what in them can change a
//! run; see digest.rs) and the task's own files. A run writes into `audit/.running/<task>/`
//! and takes the place of the latest at its end, so that an interrupted one leaves the
//! others as they were.
//!
//! The results are committed with the code, so a run needs a working tree without
//! uncommitted changes (`audit/` aside), and the paths of this machine are taken out of
//! what it wrote. One with `--no-record` may run on any tree, and goes to
//! `target/audit/<task>/` instead.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::outdated::{civil, day};

/// The log of the run in progress, if any.
static LOG: Mutex<Option<File>> = Mutex::new(None);

fn err(path: &Path, e: &io::Error) -> String {
    format!("{}: {e}", path.display())
}

pub struct Run {
    root: PathBuf,
    base: PathBuf,
    dir: PathBuf,
    /// Kept in `audit/`, rather than in `target/audit/` (`--no-record`).
    record: bool,
    started: SystemTime,
    commit: String,
    /// The files the task reads, as inputs.txt has them.
    inputs: String,
    /// `audit/.running/<task>.lock`, held while the run lasts; it holds the run's memory
    /// limit in bytes, for the tasks that would start beside it.
    _lock: File,
}

/// Starts a run of `task`, which may use up to `bytes` of memory and reads `inputs` (as
/// inputs.txt has them); fails when another run of it is in progress, or when the tasks
/// running beside it leave no room.
pub fn start(
    root: &Path,
    task: &str,
    bytes: u64,
    record: bool,
    inputs: String,
) -> Result<Run, String> {
    let audit = root.join("audit");
    // Where the run goes on its way to the latest one.
    let home = if record { audit.clone() } else { root.join("target").join("audit") };
    let (base, dir) = (home.join(task), home.join(".running").join(task));
    let lock = crate::lock::task(root, task, bytes)?;
    // What an interrupted run left.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| err(&dir, &e))?;
    let path = dir.join("run.log");
    let log = File::create(&path).map_err(|e| err(&path, &e))?;
    if let Ok(mut current) = LOG.lock() {
        *current = Some(log);
    }
    Ok(Run {
        root: root.to_owned(),
        base,
        dir,
        record,
        started: SystemTime::now(),
        commit: git(root, &["rev-parse", "HEAD"]).unwrap_or_default(),
        inputs,
        _lock: lock,
    })
}

impl Run {
    /// Where the task puts its own files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Records how the run ended (`succeeded` or `failed: <why>`), with the task's own
    /// parameters (as `seconds=3600`) and tools, and makes it the latest.
    pub fn finish(self, outcome: &str, params: &str, tools: &str) {
        if let Ok(mut current) = LOG.lock() {
            *current = None;
        }
        let seconds = self.started.elapsed().map_or(0, |d| d.as_secs());
        let mut text = String::new();
        let _ = writeln!(text, "result: {outcome}");
        let _ = writeln!(text, "started: {}", time(self.started));
        let _ = writeln!(text, "seconds: {seconds}");
        let _ = writeln!(text, "commit: {}", self.commit);
        let _ = writeln!(text, "args: {params}");
        let _ = writeln!(text, "tools: {tools}");
        let _ = fs::write(self.dir.join("run.txt"), text);
        let _ = fs::write(self.dir.join("inputs.txt"), &self.inputs);
        scrub(&self.dir, &self.root);
        let last = self.dir.join(LAST);
        // Without --no-record, the last success is kept beside a failure.
        if self.record && outcome != "succeeded" {
            // The last success: the latest run when it succeeded, else the one it kept.
            let _ = if succeeded(&self.base) {
                fs::rename(&self.base, &last)
            } else {
                fs::rename(self.base.join(LAST), &last)
            };
        }
        let _ = fs::remove_dir_all(&self.base);
        let _ = fs::rename(&self.dir, &self.base);
    }
}

/// Where the last run that succeeded is kept while the latest has failed.
pub const LAST: &str = "last_succeeded";

fn succeeded(dir: &Path) -> bool {
    fields(dir).is_some_and(|f| f.get("result") == "succeeded")
}

/// Prints `line` and adds it to the log.
pub fn say(line: &str) {
    eprintln!("{line}");
    note(line);
}

/// Adds `line` to the log, to be printed later.
pub fn note(line: &str) {
    log(format!("{line}\n").as_bytes());
}

fn log(bytes: &[u8]) {
    if let Ok(mut current) = LOG.lock()
        && let Some(file) = current.as_mut()
    {
        let _ = file.write_all(bytes);
    }
}

/// Runs `cmd`, its output going to the terminal and, during a run, to the log too; its
/// standard output into `out` instead when given.
pub fn exec(cmd: &mut Command, out: Option<File>) -> io::Result<ExitStatus> {
    let logging = LOG.lock().is_ok_and(|l| l.is_some());
    if let Some(out) = out {
        cmd.stdout(out);
    } else if logging {
        cmd.stdout(Stdio::piped());
    }
    if logging {
        cmd.stderr(Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    let (out, err) = (child.stdout.take(), child.stderr.take());
    thread::scope(|s| {
        if let Some(out) = out {
            s.spawn(|| pump(out, io::stdout()));
        }
        if let Some(err) = err {
            s.spawn(|| pump(err, io::stderr()));
        }
        child.wait()
    })
}

fn pump(mut from: impl Read, mut to: impl Write) {
    let mut buf = [0; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = to.write_all(&buf[..n]);
                log(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Takes the paths of this machine out of the files a run wrote, so that they can be
/// published: the repository becomes `.` and the home directory `~`.
fn scrub(dir: &Path, root: &Path) {
    let mut pairs = vec![(root.display().to_string(), ".")];
    if let Some(home) = std::env::var_os("HOME") {
        pairs.push((home.to_string_lossy().into_owned(), "~"));
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            scrub(&path, root);
            continue;
        }
        let Ok(old) = fs::read(&path) else { continue };
        let new = pairs.iter().fold(old.clone(), |text, (from, to)| {
            replace_path(&text, from.as_bytes(), to.as_bytes())
        });
        if new != old {
            let _ = fs::write(&path, new);
        }
    }
}

/// `text` with `from` replaced by `to` where it is a whole path or begins one: not where
/// it begins a longer name, as `/home/me/rse` begins `/home/me/rse-old`.
fn replace_path(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.len() < 2 {
        return text.to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let ends = |at: usize| {
            text.get(at).is_none_or(|c| !(c.is_ascii_alphanumeric() || b"-_.".contains(c)))
        };
        if text[i..].starts_with(from) && ends(i + from.len()) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

/// The files of the working tree as git sees them (ignored files left out, `audit/` too),
/// with the hashes git gives their contents. Read through an index of its own, so that
/// the real one stays as it is.
pub fn files(root: &Path) -> Result<Vec<(String, String)>, String> {
    let index = git(root, &["rev-parse", "--git-path", "index"])
        .ok_or("not in a git repository: the runs are recorded by the commit they ran on")?;
    let temp = std::env::temp_dir().join(format!("xtask-index-{}", std::process::id()));
    // A copy keeps what git knows of the files, so that it rereads only the changed ones.
    let _ = fs::copy(root.join(index), &temp);
    let with_index = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_INDEX_FILE", &temp)
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let listed = with_index(&["add", "--all"]).and_then(|_| with_index(&["ls-files", "-s", "-z"]));
    let _ = fs::remove_file(&temp);
    let listed = listed.ok_or("git could not list the files")?;
    // `<mode> <hash> <stage>\t<path>`, each ended by a NUL.
    Ok(String::from_utf8_lossy(&listed.stdout)
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let hash = meta.split(' ').nth(1)?;
            (!path.starts_with("audit/")).then(|| (path.to_owned(), hash.to_owned()))
        })
        .collect())
}

/// Whether git tracks `path`.
pub fn tracked(root: &Path, path: &str) -> bool {
    git(root, &["ls-files", "--error-unmatch", "--", path]).is_some()
}

/// The paths with uncommitted changes, untracked files included, `audit/` aside.
pub fn uncommitted(root: &Path) -> Vec<String> {
    let args = ["status", "--porcelain", "-z", "--untracked-files=all", "--", ".", ":!audit"];
    // Not trimmed, as `git` would: the first entry may start with a space.
    let Some(out) = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    else {
        return vec!["(git status failed)".to_owned()];
    };
    // `XY <path>`, each ended by a NUL; a rename is followed by its old path.
    let mut paths = Vec::new();
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        paths.push(entry.get(3..).unwrap_or(entry).to_owned());
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    paths
}

/// Why `task` needs a run, given the files it reads now (as inputs.txt has them), the
/// tools pinned for it and the parameters it would run with; empty when its latest run
/// stands.
pub fn needed(root: &Path, task: &str, inputs: &str, tools: &str, params: &str) -> Vec<String> {
    let base = root.join("audit").join(task);
    let Some(f) = fields(&base) else { return vec!["it never ran".to_owned()] };
    let mut why = Vec::new();
    if f.get("result") != "succeeded" {
        why.push("its latest run failed".to_owned());
    }
    if f.get("tools") != tools {
        why.push("the pinned tools changed".to_owned());
    }
    if f.get("args") != params {
        why.push(format!("recorded with different arguments (`{}`)", f.get("args")));
    }
    // New advisories appear whatever the code does.
    if task == "deny" && age(f.get("started")).is_none_or(|secs| secs >= 86_400) {
        why.push("over a day old, and advisories are published daily".to_owned());
    }
    let old = fs::read_to_string(base.join("inputs.txt")).unwrap_or_default();
    let changed = changed(&old, inputs);
    if old.is_empty() {
        why.push("it recorded no list of the files it read".to_owned());
    } else if old.lines().next() != inputs.lines().next() {
        why.push("its list of the files it read was hashed another way".to_owned());
    } else if !changed.is_empty() {
        let shown: Vec<&str> = changed.iter().take(3).map(String::as_str).collect();
        let more = match changed.len() {
            n if n > 3 => format!(" and {} more", n - 3),
            _ => String::new(),
        };
        why.push(format!("changed since: {}{more}", shown.join(", ")));
    }
    why
}

/// The paths whose contents differ between two inputs.txt.
fn changed(old: &str, new: &str) -> Vec<String> {
    let read = |text: &str| -> std::collections::BTreeMap<String, String> {
        text.lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(hash, path)| (path.to_owned(), hash.to_owned()))
            .collect()
    };
    let (old, new) = (read(old), read(new));
    let mut paths: Vec<String> = old.keys().chain(new.keys()).cloned().collect();
    paths.sort();
    paths.dedup();
    paths.retain(|p| old.get(p) != new.get(p));
    paths
}

/// Seconds since a time as run.txt has it.
fn age(started: &str) -> Option<u64> {
    let days = u64::try_from(day(started)?).ok()?;
    let hours: u64 = started.get(11..13)?.parse().ok()?;
    let minutes: u64 = started.get(14..16)?.parse().ok()?;
    let then = days * 86_400 + hours * 3600 + minutes * 60;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(then)
}

/// `cargo xtask status`: how each task's latest run ended, when one last succeeded, and
/// whether it needs a run (`needed`, given the task).
pub fn status(root: &Path, tasks: &[&str], needed: impl Fn(&str) -> Vec<String>) {
    for task in tasks {
        let base = root.join("audit").join(task);
        let mut line = String::new();
        let shown = |f: &Fields| {
            let commit = f.get("commit");
            format!("{} on {}", f.get("started"), commit.get(..7).unwrap_or(commit))
        };
        match fields(&base) {
            None => line.push_str("never run"),
            Some(f) if f.get("result") == "succeeded" => {
                let _ = write!(line, "succeeded {}", shown(&f));
            }
            Some(f) => {
                let _ = write!(line, "failed {} (audit/{task}/)", shown(&f));
                match fields(&base.join(LAST)) {
                    Some(last) => {
                        let _ = write!(line, "; last succeeded {}", shown(&last));
                    }
                    None => line.push_str("; never succeeded"),
                }
            }
        }
        let why = needed(task);
        if why.is_empty() {
            line.push_str("; up to date");
        } else if base.join("run.txt").exists() {
            let _ = write!(line, "; needs a run: {}", why.join("; "));
        }
        let _ = writeln!(io::stdout(), "{task:<9} {line}");
    }
}

struct Fields(String);

impl Fields {
    fn get(&self, key: &str) -> &str {
        self.0.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(": ")).unwrap_or_default()
    }
}

fn fields(dir: &Path) -> Option<Fields> {
    fs::read_to_string(dir.join("run.txt")).ok().map(Fields)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `YYYY-MM-DD HH:MM UTC`.
fn time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let minutes = secs % 86_400 / 60;
    format!("{} {:02}:{:02} UTC", civil(days), minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::{changed, replace_path};

    #[test]
    fn paths_are_replaced_only_where_whole() {
        let text = b"/home/me/rse/src/a.rs /home/me/rse-old /home/me/rse: /home/me/rse";
        assert_eq!(
            replace_path(text, b"/home/me/rse", b"."),
            b"./src/a.rs /home/me/rse-old .: .".to_vec()
        );
    }

    #[test]
    fn changes_are_paths_whose_hash_differs() {
        let old = "a1\tCargo.lock\nb1\tsrc/a.rs\nc1\tsrc/gone.rs\n";
        let new = "a1\tCargo.lock\nb2\tsrc/a.rs\nd1\tsrc/new.rs\n";
        assert_eq!(changed(old, new), ["src/a.rs", "src/gone.rs", "src/new.rs"]);
        assert_eq!(changed(old, old), Vec::<String>::new());
    }
}
