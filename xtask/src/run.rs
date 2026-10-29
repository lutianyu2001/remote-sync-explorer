//! How the tasks run their commands: cargo with the toolchain each needs, in a systemd scope
//! with a memory limit where the task is a heavy one, its output kept in the run's log.

use std::borrow::BorrowMut;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::OnceLock;

use crate::pins::NIGHTLY;
use crate::record;

pub fn cargo(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args(args).current_dir(root);
    cmd
}

pub fn nightly(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg(format!("+{NIGHTLY}")).args(args).current_dir(root);
    cmd
}

/// This program, to run again inside a scope: the one running now, through /proc, rather
/// than the path it started from, where a build meanwhile (a `cargo xtask status` in
/// another terminal) puts another program, or none.
pub fn this_program() -> PathBuf {
    PathBuf::from(format!("/proc/{}/exe", std::process::id()))
}

/// `cmd` in a systemd scope limited to `memory`, without swap, so that going over it
/// ends this command alone; `cmd` as it is with `RUST_XTASK_NO_SCOPE=1`.
pub fn capped(cmd: impl BorrowMut<Command>, memory: &str) -> Command {
    scoped(cmd, memory, &[])
}

/// `capped`, with more properties for the scope.
pub fn scoped(mut cmd: impl BorrowMut<Command>, memory: &str, properties: &[&str]) -> Command {
    let cmd = cmd.borrow_mut();
    let mut out;
    if scopes() {
        out = Command::new("systemd-run");
        out.args(["--user", "--scope", "--quiet", "-p"])
            .arg(format!("MemoryMax={memory}"))
            .args(["-p", "MemorySwapMax=0"]);
        for p in properties {
            out.args(["-p", p]);
        }
        out.arg("--").arg(cmd.get_program());
    } else {
        out = Command::new(cmd.get_program());
    }
    out.args(cmd.get_args());
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => out.env(key, value),
            None => out.env_remove(key),
        };
    }
    if let Some(dir) = cmd.get_current_dir() {
        out.current_dir(dir);
    }
    out
}

/// Whether heavy tasks are to run without a scope: on a machine of their own, as in CI.
fn no_scope() -> bool {
    env::var_os("RUST_XTASK_NO_SCOPE").is_some_and(|v| v == "1")
}

/// Whether heavy tasks run in memory-limited scopes.
pub fn scopes() -> bool {
    !no_scope() && usable_scopes()
}

/// Whether memory-limited systemd scopes can be had here.
fn usable_scopes() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        // A scope starts even where its memory limit is not applied (cgroup v1, or no
        // memory controller delegated to the user), so ask one what its limit is.
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "-p", "MemoryMax=1G", "--"])
            .arg(this_program())
            .arg("__memory-max")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1073741824")
    })
}

/// Whether scopes take `OOMPolicy=` (systemd 253 and later).
pub fn oom_policy() -> bool {
    static TAKEN: OnceLock<bool> = OnceLock::new();
    *TAKEN.get_or_init(|| {
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "-p", "OOMPolicy=continue", "--"])
            .arg(this_program())
            .arg("__memory-max")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// Fails unless a heavy task can run here: in a scope, or without one when told to.
pub fn scope() -> Result<(), String> {
    static TOLD: OnceLock<()> = OnceLock::new();
    if no_scope() {
        TOLD.get_or_init(|| eprintln!("xtask: RUST_XTASK_NO_SCOPE=1: no memory limits"));
        return Ok(());
    }
    if usable_scopes() {
        return Ok(());
    }
    Err("no usable memory-limited scope: here systemd-run --user --scope does not apply \
         MemoryMax, and without a limit cbmc or fuzzing can take all the memory; \
         RUST_XTASK_NO_SCOPE=1 runs without one, on a machine of its own"
        .to_owned())
}

/// A file of this process's cgroup (v2), such as `memory.max`.
pub fn cgroup(file: &str) -> Option<String> {
    let own = fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = own.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(fs::read_to_string(format!("/sys/fs/cgroup{path}/{file}")).ok()?.trim().to_owned())
}

/// `__memory-max`, run inside a scope: its memory limit, to see that the limit asked for
/// holds.
pub fn memory_max() -> ExitCode {
    println!("{}", cgroup("memory.max").unwrap_or_default());
    ExitCode::SUCCESS
}

/// Bytes in a size as systemd takes it: a number, or one with K, M, G or T (powers of
/// 1024).
pub fn bytes(size: &str) -> Option<u64> {
    let (number, shift) = match size.chars().last()? {
        'K' => (&size[..size.len() - 1], 10),
        'M' => (&size[..size.len() - 1], 20),
        'G' => (&size[..size.len() - 1], 30),
        'T' => (&size[..size.len() - 1], 40),
        _ => (size, 0),
    };
    number.parse::<u64>().ok()?.checked_mul(1 << shift)
}

/// `bytes`, failing on what is not a size.
pub fn size(memory: &str) -> Result<u64, String> {
    bytes(memory).ok_or_else(|| format!("not a size of memory: {memory}"))
}

pub fn status(mut cmd: impl BorrowMut<Command>) -> Result<(), String> {
    let cmd = cmd.borrow_mut();
    let line = shown(cmd);
    record::say(&format!("$ {line}"));
    match record::exec(cmd, None) {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{line}: {s}")),
        Err(e) => Err(format!("{line}: {e}")),
    }
}

pub fn shown(cmd: &Command) -> String {
    let mut words = vec![cmd.get_program().to_string_lossy().into_owned()];
    words.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn sizes_read_as_systemd_reads_them() {
        assert_eq!(bytes("4G"), Some(4 << 30));
        assert_eq!(bytes("1536M"), Some(1536 << 20));
        assert_eq!(bytes("512K"), Some(512 << 10));
        assert_eq!(bytes("100"), Some(100));
        assert_eq!(bytes("4GB"), None);
        assert_eq!(bytes("G"), None);
        assert_eq!(bytes(""), None);
        assert_eq!(bytes("99999999999T"), None);
    }
}
