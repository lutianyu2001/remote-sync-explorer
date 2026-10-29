//! The locks that keep runs apart. `audit/lock` is held while tasks run, alone, or shared
//! with other `cargo xtask` processes when `RUST_XTASK_PARALLEL=1`; changing the tools
//! takes it alone in any case. Each run of a task holds `audit/.running/<task>.lock` too,
//! with its memory limit in it, so that a task starting beside it sees how much is taken.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Memory left to the system and the desktop when tasks run side by side.
const RESERVE: u64 = 4 << 30;

/// `audit/lock`, held while tasks run, `all` included.
pub struct Session(#[allow(dead_code)] File);

pub fn session(root: &Path) -> Result<Session, String> {
    lock(root, parallel(), false)
}

/// The lock alone, whatever `RUST_XTASK_PARALLEL` says: for changing the tools, which no
/// task may be using meanwhile.
pub fn exclusive(root: &Path) -> Result<Session, String> {
    lock(root, false, true)
}

fn lock(root: &Path, shared: bool, tools: bool) -> Result<Session, String> {
    let audit = root.join("audit");
    fs::create_dir_all(&audit).map_err(|e| err(&audit, &e))?;
    let path = audit.join("lock");
    let lock = File::create(&path).map_err(|e| err(&path, &e))?;
    let held = if shared { lock.try_lock_shared() } else { lock.try_lock() };
    if held.is_err() {
        let hint = if tools {
            "; the tools change only while no task runs"
        } else {
            "; RUST_XTASK_PARALLEL=1 for both lets tasks run side by side when the memory allows"
        };
        return Err(format!("another cargo xtask is running ({}){hint}", path.display()));
    }
    Ok(Session(lock))
}

fn parallel() -> bool {
    std::env::var_os("RUST_XTASK_PARALLEL").is_some_and(|v| v == "1")
}

/// Takes `audit/.running/<task>.lock` for a run of `task` that may use up to `bytes` of
/// memory, and writes that in it; fails when another run of it is in progress, or when
/// the tasks running beside it leave no room.
pub fn task(root: &Path, task: &str, bytes: u64) -> Result<File, String> {
    let audit = root.join("audit");
    let running = audit.join(".running");
    fs::create_dir_all(&running).map_err(|e| err(&running, &e))?;
    let path = running.join(format!("{task}.lock"));
    let mut lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| err(&path, &e))?;
    if lock.try_lock().is_err() && !retried(&audit, &lock) {
        return Err(format!("another run of {task} is in progress"));
    }
    // Truncated only now that it is ours: another run of it may have been writing.
    lock.set_len(0)
        .and_then(|()| lock.write_all(bytes.to_string().as_bytes()))
        .map_err(|e| err(&path, &e))?;
    if parallel() {
        // One task at a time looks at the others.
        let budget = audit.join("budget");
        let held = File::create(&budget).map_err(|e| err(&budget, &e))?;
        held.lock().map_err(|e| err(&budget, &e))?;
        room(&running, task, bytes)?;
    }
    Ok(lock)
}

/// Whether `lock` is ours at a second try, made holding `audit/budget`: a task checking
/// its room holds a run's lock shared for a moment, and only while it holds that.
fn retried(audit: &Path, lock: &File) -> bool {
    let budget = audit.join("budget");
    let Ok(held) = File::create(&budget) else { return false };
    held.lock().is_ok() && lock.try_lock().is_ok()
}

/// Fails unless `task`, using up to `bytes`, can run beside the tasks running now:
/// Kani and fuzzing never together, and all their limits within the memory there is.
fn room(running: &Path, task: &str, bytes: u64) -> Result<(), String> {
    let (total, available) = meminfo();
    let mut used: u64 = 0;
    for entry in fs::read_dir(running).map_err(|e| err(running, &e))?.flatten() {
        let path = entry.path();
        let Some(other) = path.file_name().and_then(|n| n.to_str()?.strip_suffix(".lock")) else {
            continue;
        };
        let Ok(file) = File::open(&path) else { continue };
        // Lockable: nothing holds it, whatever an interrupted run left.
        if other == task || file.try_lock_shared().is_ok() {
            continue;
        }
        if ["kani", "fuzz"].contains(&task) && ["kani", "fuzz"].contains(&other) {
            return Err(format!("{other} is running, and kani and fuzz never run together"));
        }
        // Empty, or not a number: it has not written its limit yet, so it may take all.
        used += fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(total);
    }
    let mib = |b: u64| format!("{} MiB", b >> 20);
    if used.saturating_add(bytes) > total.saturating_sub(RESERVE) {
        return Err(format!(
            "the tasks running have {} as their limits; with {} more for {task} that comes \
             within {} of the {} of memory",
            mib(used),
            mib(bytes),
            mib(RESERVE),
            mib(total)
        ));
    }
    if bytes > available {
        return Err(format!(
            "{task} may use {}, and only {} is free now",
            mib(bytes),
            mib(available)
        ));
    }
    Ok(())
}

/// `MemTotal` and `MemAvailable`, in bytes.
fn meminfo() -> (u64, u64) {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let field = |name: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(name)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()
            })
            .map_or(0, |kb| kb * 1024)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

fn err(path: &Path, e: &io::Error) -> String {
    format!("{}: {e}", path.display())
}
