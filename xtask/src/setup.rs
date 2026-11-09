//! The pinned tools and toolchain: `cargo xtask setup` installs them, and each task checks
//! before it runs that what it needs is in place. The versions a run used are recorded
//! with it and compared on their own, so this module is no task's input.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;
use std::process::Command;

use crate::USAGE;
use crate::pins::{NIGHTLY, STABLE, TOOLS};
use crate::run::status;
use crate::tasks::{TASKS, needs, subcommands};

/// Fails unless the tools and the toolchain `task` needs are the pinned ones.
pub fn check(task: &str) -> Result<(), String> {
    let installed = installed();
    for sub in subcommands(task) {
        let Some((krate, version, _)) = TOOLS.iter().find(|t| t.2 == sub) else { continue };
        let found = installed.get(*krate).map_or("not installed", String::as_str);
        if found != *version {
            return Err(format!(
                "`{task}` needs {krate} {version} ({found}); run `cargo xtask setup {task}`"
            ));
        }
        if sub == "kani" && !kani_ready(version) {
            return Err(format!(
                "`{task}` needs Kani's own toolchain and cbmc; run `cargo xtask setup {task}`"
            ));
        }
    }
    if let Some(components) = needs(task).1 {
        let installed = nightly_components();
        if let Some(missing) = components.iter().find(|c| !installed.contains(**c)) {
            return Err(format!(
                "`{task}` needs {NIGHTLY} with {missing}; run `cargo xtask setup {task}`"
            ));
        }
        if installed.is_empty() {
            return Err(format!("`{task}` needs {NIGHTLY}; run `cargo xtask setup {task}`"));
        }
    }
    Ok(())
}

/// Installs the pinned tools and toolchain that `tasks` need (all, if none is named),
/// leaving those already in place.
pub fn setup(tasks: &[String]) -> Result<(), String> {
    let tasks: Vec<&str> =
        if tasks.is_empty() { TASKS.to_vec() } else { tasks.iter().map(String::as_str).collect() };
    if let Some(t) = tasks.iter().find(|t| !TASKS.contains(t)) {
        return Err(format!("unknown task {t}\n\n{USAGE}"));
    }
    // First, since the tools are built with it.
    status(&mut install_toolchain(STABLE, &BTreeSet::new()))?;
    for (krate, version, sub) in TOOLS {
        if !tasks.iter().any(|t| subcommands(t).contains(&sub)) {
            continue;
        }
        if installed().get(krate).is_none_or(|v| v != version) {
            status(tool_cargo(&["install", "--locked", "--version", version, krate]))?;
        }
        if sub == "kani" {
            // Kani's own toolchain and cbmc; quick when they are there already.
            status(tool_cargo(&["kani", "setup"]))?;
        }
    }
    let components: BTreeSet<&str> =
        tasks.iter().filter_map(|t| needs(t).1).flatten().copied().collect();
    if tasks.iter().any(|t| needs(t).1.is_some()) {
        status(&mut install_toolchain(NIGHTLY, &components))?;
    }
    Ok(())
}

/// cargo for installing the tools: with the stable toolchain pinned for them, and outside
/// the repository, whose .cargo/config.toml would otherwise have crates come from vendor/
/// alone, offline. From /, which only root writes to: cargo reads the .cargo/config.toml of
/// every directory above, and another user may leave one in the shared temporary directory.
fn tool_cargo(args: &[&str]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg(format!("+{STABLE}")).args(args).current_dir("/");
    cmd
}

/// `rustup toolchain install` for `toolchain` with `components`, quick when it is there.
pub fn install_toolchain(toolchain: &str, components: &BTreeSet<&str>) -> Command {
    let mut install = Command::new("rustup");
    install.args(["toolchain", "install", toolchain, "--profile", "minimal"]);
    for c in components {
        install.args(["--component", c]);
    }
    install
}

/// The nightly components some task needs.
pub fn nightly_needs() -> BTreeSet<&'static str> {
    TASKS.iter().filter_map(|t| needs(t).1).flatten().copied().collect()
}

/// The crates `cargo install` put in place, with their versions. Read from its list rather
/// than from `--version`: `cargo kani --version` first downloads Kani's toolchain when it
/// is not there.
fn installed() -> BTreeMap<String, String> {
    let Ok(out) = Command::new("cargo").args(["install", "--list"]).output() else {
        return BTreeMap::new();
    };
    // `cargo-deny v0.20.2:`, then the binaries, indented.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with(char::is_whitespace))
        .filter_map(|l| {
            let (krate, rest) = l.split_once(' ')?;
            let version = rest.strip_prefix('v')?.split([':', ' ']).next()?;
            Some((krate.to_owned(), version.to_owned()))
        })
        .collect()
}

/// Whether `cargo kani setup` has been run for this version.
fn kani_ready(version: &str) -> bool {
    env::var_os("HOME")
        .is_some_and(|home| Path::new(&home).join(".kani").join(format!("kani-{version}")).is_dir())
}

/// The pinned nightly's installed components (names without the target), empty when it
/// is not installed.
fn nightly_components() -> BTreeSet<String> {
    let Ok(out) = Command::new("rustup")
        .args(["component", "list", "--installed", "--toolchain", NIGHTLY])
        .output()
    else {
        return BTreeSet::new();
    };
    if !out.status.success() {
        return BTreeSet::new();
    }
    let mut names = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        names.insert(line.to_owned());
        // `miri-x86_64-unknown-linux-gnu`, but `rust-src` has no target.
        let mut parts: Vec<&str> = line.split('-').collect();
        while parts.len() > 1 {
            parts.pop();
            names.insert(parts.join("-"));
        }
    }
    names
}
