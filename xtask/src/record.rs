//! What the runs of each task leave in `audit/<task>/`: the latest run, and when that
//! failed, the last that succeeded in `last_succeeded/` under it. Each holds `run.log`
//! (what the run printed), `run.txt` (when, on which commit, with which tools, and how
//! it ended), `inputs.txt` (the files it read, with hashes of what in them can change a
//! run; see digest.rs) and the task's own files. A run writes into `audit/.running/<task>/`
//! and takes the place of the latest at its end, so that an interrupted one leaves the
//! others as they were.
//!
//! The results are committed with the code, so a run needs a working tree without
//! uncommitted changes (`audit/` aside, but for the reviews), and the paths of this machine are taken out of
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
use crate::tasks::review::REVIEWS;

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

    /// Why the run cannot stand for the commit it started on, if so: a recorded run says what
    /// that commit does, and the tree was only known to match it at the start.
    pub fn moved(&self) -> Option<String> {
        if !self.record {
            return None;
        }
        let head = git(&self.root, &["rev-parse", "HEAD"]).unwrap_or_default();
        if head != self.commit {
            return Some(format!("the commit changed during the run, to {head}"));
        }
        let paths = uncommitted(&self.root);
        (!paths.is_empty()).then(|| format!("files changed during the run: {}", paths.join(", ")))
    }

    /// Records how the run ended (`succeeded` or `failed: <why>`), with the task's own
    /// parameters (as `seconds=3600`) and tools, and makes it the latest. A run that could not
    /// be written whole, or rid of the paths of this machine, is an error, and goes no
    /// further than `.running/`.
    pub fn finish(self, outcome: &str, params: &str, tools: &str) -> Result<(), String> {
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
        for (name, contents) in [("run.txt", text.as_str()), ("inputs.txt", &self.inputs)] {
            let path = self.dir.join(name);
            fs::write(&path, contents).map_err(|e| err(&path, &e))?;
        }
        scrub(&self.dir, &self.root)?;
        let last = self.dir.join(LAST);
        // Without --no-record, the last success is kept beside a failure.
        if self.record && outcome != "succeeded" {
            // The last success: the latest run when it succeeded, else the one it kept.
            let kept = if succeeded(&self.base) { self.base.clone() } else { self.base.join(LAST) };
            absent_is_fine(fs::rename(&kept, &last)).map_err(|e| err(&kept, &e))?;
        }
        absent_is_fine(fs::remove_dir_all(&self.base)).map_err(|e| err(&self.base, &e))?;
        fs::rename(&self.dir, &self.base).map_err(|e| err(&self.base, &e))
    }
}

/// A first run, or one without a success to keep, finds nothing there.
fn absent_is_fine(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
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
fn scrub(dir: &Path, root: &Path) -> Result<(), String> {
    let mut pairs = vec![(root.display().to_string(), ".")];
    if let Some(home) = std::env::var_os("HOME") {
        pairs.push((home.to_string_lossy().into_owned(), "~"));
    }
    for entry in fs::read_dir(dir).map_err(|e| err(dir, &e))? {
        let entry = entry.map_err(|e| err(dir, &e))?;
        let path = entry.path();
        if entry.file_type().map_err(|e| err(&path, &e))?.is_dir() {
            scrub(&path, root)?;
            continue;
        }
        let old = fs::read(&path).map_err(|e| err(&path, &e))?;
        let new = pairs.iter().fold(old.clone(), |text, (from, to)| {
            replace_path(&text, from.as_bytes(), to.as_bytes())
        });
        if new != old {
            fs::write(&path, new).map_err(|e| err(&path, &e))?;
        }
    }
    Ok(())
}

/// `text` with `from` replaced by `to` where it is a whole path or begins one: not where
/// it begins a longer name, as `/home/me/rext` begins `/home/me/rext-old` and `.old` makes
/// `/home/me/rext.old`, nor where it is the end of one, as in `/mnt/home/me/rext`. A `.` with
/// no more of a name after it ends a sentence, not the name.
fn replace_path(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.len() < 2 {
        return text.to_vec();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    let name = |c: &u8| c.is_ascii_alphanumeric() || b"-_".contains(c);
    while i < text.len() {
        let ends = |at: usize| match text.get(at) {
            None => true,
            Some(b'.') => !text.get(at + 1).is_some_and(name),
            Some(c) => !name(c),
        };
        let starts = i == 0 || !(name(&text[i - 1]) || text[i - 1] == b'.');
        if starts && text[i..].starts_with(from) && ends(i + from.len()) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

/// Whether `path` is under `audit/` and not a review: the results and what runs leave there,
/// no input of a task, and changed by the runs themselves. The reviews are written by hand,
/// and read by the review task.
fn results(path: &str) -> bool {
    let reviews = format!("{REVIEWS}/");
    path.starts_with("audit/") && !path.starts_with(&reviews)
}

/// The files of the working tree as git sees them (ignored files left out, `audit/` too but
/// for the reviews),
/// with the hashes git gives their contents. Read through an index of its own, so that
/// the real one stays as it is.
pub fn files(root: &Path) -> Result<Vec<(String, String)>, String> {
    let index = git(root, &["rev-parse", "--git-path", "index"])
        .ok_or("not in a git repository: the runs are recorded by the commit they ran on")?;
    // Under target/, not the shared temporary directory, where another user could put a
    // link at this name beforehand.
    let dir = root.join("target").join("audit");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let temp = dir.join(format!("index-{}", std::process::id()));
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
            (!results(path)).then(|| (path.to_owned(), hash.to_owned()))
        })
        .collect())
}

/// Whether git tracks `path`.
pub fn tracked(root: &Path, path: &str) -> bool {
    git(root, &["ls-files", "--error-unmatch", "--", path]).is_some()
}

/// The paths with uncommitted changes, untracked files included, `audit/` aside but for the
/// reviews, which a run of review reads.
pub fn uncommitted(root: &Path) -> Vec<String> {
    let args = ["status", "--porcelain", "-z", "--untracked-files=all", "--", "."];
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
    // git status gives paths from the top of the repository, which is above the workspace
    // when the workspace is in a subdirectory of it.
    let prefix = git(root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    // `XY <path>`, each ended by a NUL; a rename is followed by its old path.
    let mut paths = Vec::new();
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let path = from_workspace(entry.get(3..).unwrap_or(entry), prefix.trim_end());
        // A directory untracked as a whole is listed file by file (--untracked-files=all).
        if !results(path) {
            paths.push(path.to_owned());
        }
        if entry.starts_with('R') || entry.starts_with('C') {
            entries.next();
        }
    }
    paths
}

/// `path`, given from the top of the repository, as from the workspace, `prefix` (as
/// `git rev-parse --show-prefix` says) below it.
fn from_workspace<'a>(path: &'a str, prefix: &str) -> &'a str {
    path.strip_prefix(prefix).unwrap_or(path)
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
    if !covers(f.get("args"), params) {
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

/// Whether a run recorded with the arguments `recorded` stands for one with `wanted`: the
/// same ones, or a fuzz run at least as long (`seconds=N`), as CI's runs are longer.
fn covers(recorded: &str, wanted: &str) -> bool {
    let seconds = |args: &str| args.strip_prefix("seconds=").and_then(|n| n.parse::<u64>().ok());
    recorded == wanted
        || matches!((seconds(recorded), seconds(wanted)), (Some(r), Some(w)) if r >= w)
}

/// How the latest run of `task` ended: whether it succeeded; None when it never ran.
pub fn latest_succeeded(root: &Path, task: &str) -> Option<bool> {
    fields(&root.join("audit").join(task)).map(|f| f.get("result") == "succeeded")
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{LAST, REVIEWS, changed, covers, from_workspace, replace_path, results, start};
    use std::fs;

    /// A run of review replaces audit/review/, and leaves audit/reviews/ as it was, whether it
    /// succeeded or failed.
    #[test]
    fn the_results_of_review_are_kept_apart_from_the_reviews() {
        let root =
            crate::root().join("target").join(format!("xtask-record-{}", std::process::id()));
        let reviews = root.join(REVIEWS);
        fs::create_dir_all(&reviews).unwrap();
        fs::write(reviews.join("a.md"), "a review").unwrap();
        for outcome in ["succeeded", "failed: stale", "failed: stale", "succeeded"] {
            let run = start_again(&root);
            fs::write(run.dir().join("review.txt"), outcome).unwrap();
            run.finish(outcome, "", "").unwrap();
            let kept = fs::read_to_string(root.join("audit/review/review.txt")).unwrap();
            assert_eq!(kept, outcome);
            assert_eq!(fs::read_to_string(reviews.join("a.md")).unwrap(), "a review");
            let last = root.join("audit/review").join(LAST).join("review.txt");
            assert_eq!(last.exists(), outcome != "succeeded");
        }
        fs::remove_dir_all(&root).unwrap();
        assert!(results("audit/review/run.txt"));
        assert!(results("audit/.running/review/run.log"));
        assert!(!results("audit/reviews/a.md"));
        assert!(!results("crates/a/src/lib.rs"));
    }

    /// Starts a run of review at `root`, whose lock the last run let go of. A child process
    /// another test starts meanwhile may still hold it for a moment: between fork and exec,
    /// a child has every descriptor of this process, the closed lock's among them.
    fn start_again(root: &std::path::Path) -> super::Run {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match start(root, "review", 0, true, String::new()) {
                Err(e) if e.contains("in progress") && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                run => return run.unwrap(),
            }
        }
    }

    #[test]
    fn a_longer_fuzz_run_stands_for_a_shorter_one() {
        assert!(covers("", ""));
        assert!(covers("seconds=3600", "seconds=3600"));
        assert!(covers("seconds=14400", "seconds=3600"));
        assert!(!covers("seconds=3600", "seconds=14400"));
        assert!(!covers("seconds=x", "seconds=3600"));
        assert!(!covers("", "seconds=3600"));
    }

    #[test]
    fn paths_are_replaced_only_where_whole() {
        let text = b"/home/me/rext/src/a.rs /home/me/rext-old /home/me/rext: /home/me/rext";
        assert_eq!(
            replace_path(text, b"/home/me/rext", b"."),
            b"./src/a.rs /home/me/rext-old .: .".to_vec()
        );
    }

    #[test]
    fn a_full_stop_ends_a_path_and_a_longer_path_is_left_alone() {
        let text = b"see /home/me/rext. /home/me/rext.old /mnt/home/me/rext x=/home/me/rext";
        assert_eq!(
            replace_path(text, b"/home/me/rext", b"."),
            b"see .. /home/me/rext.old /mnt/home/me/rext x=.".to_vec()
        );
    }

    #[test]
    fn changes_are_paths_whose_hash_differs() {
        let old = "a1\tCargo.lock\nb1\tsrc/a.rs\nc1\tsrc/gone.rs\n";
        let new = "a1\tCargo.lock\nb2\tsrc/a.rs\nd1\tsrc/new.rs\n";
        assert_eq!(changed(old, new), ["src/a.rs", "src/gone.rs", "src/new.rs"]);
        assert_eq!(changed(old, old), Vec::<String>::new());
    }

    /// git status gives paths from the top of the repository; the workspace may be below it.
    #[test]
    fn paths_are_read_from_the_workspace() {
        assert_eq!(from_workspace("sub/audit/miri/run.txt", "sub/"), "audit/miri/run.txt");
        assert_eq!(from_workspace("audit/miri/run.txt", ""), "audit/miri/run.txt");
        assert_eq!(from_workspace("subway/x.rs", "sub/"), "subway/x.rs");
    }
}
