//! The tasks: what each needs, how much memory it may take, and its code in a module of its
//! own, so that a change to one task's code makes only that task need a run (see
//! `affects`).

use std::env;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::Plan;
use crate::pins::{NIGHTLY, TOOLS};
use crate::record;
use crate::run::cargo;

pub mod asan;
pub mod coverage;
pub mod deny;
pub mod fuzz;
pub mod geiger;
pub mod kani;
pub mod miri;
pub mod review;
pub mod vet;

/// Every task there is, in the order `all` runs them: the cheap ones first.
pub const KNOWN: [&str; 9] =
    ["review", "deny", "vet", "geiger", "miri", "asan", "coverage", "kani", "fuzz"];

// Per project: the tasks of KNOWN this project runs, in the same order. Not kani before
// the first proof, nor fuzz before the first target, nor asan without tests that call into
// C: a task with nothing to check fails. Here not review nor asan yet: the unsafe code
// (src/rclone/ffi.rs, src/i18n.rs, src/rclone/system_ssh.rs, src/terminal.rs) has no review
// in audit/reviews/ so far.
/// The tasks this project runs.
pub const TASKS: &[&str] = &["deny", "vet", "geiger", "miri", "coverage", "kani", "fuzz"];

/// The memory limit of the heavy tasks other than Kani, and of building the fuzz targets.
const MEMORY: &str = "4G";

/// Runs `task`, as `plan` says, writing its own files into `dir`.
pub fn run(root: &Path, task: &str, dir: &Path, plan: &Plan) -> Result<(), String> {
    match task {
        "deny" => deny::run(root),
        "vet" => vet::run(root),
        "geiger" => geiger::run(root, dir),
        "review" => review::run(root, dir),
        "miri" => miri::run(root),
        "asan" => asan::run(root),
        "coverage" => coverage::run(root, dir),
        "kani" => kani::run(root, dir, &plan.proofs),
        "fuzz" => fuzz::run(root, dir, plan),
        _ => Err(format!("unknown task {task}")),
    }
}

/// What a task runs with, as each run records it: cargo subcommands, and the nightly
/// toolchain's components, if it needs that toolchain at all.
pub fn needs(task: &str) -> (&'static [&'static str], Option<&'static [&'static str]>) {
    match task {
        "deny" => (&["deny"], None),
        "vet" => (&["vet"], None),
        "geiger" => (&["geiger"], None),
        "fuzz" => (&["fuzz"], Some(&[])),
        "kani" => (&["kani"], None),
        "miri" => (&[], Some(&["miri", "rust-src"])),
        "asan" => (&[], Some(&[])),
        "coverage" => (&["llvm-cov"], Some(&["llvm-tools"])),
        _ => (&[], None),
    }
}

/// The cargo subcommands `task` needs: those it runs with, and for fuzz cargo deny too,
/// which checks the fuzz workspace's dependencies before they are built.
pub fn subcommands(task: &str) -> Vec<&'static str> {
    let mut subs = needs(task).0.to_vec();
    if task == "fuzz" {
        subs.push("deny");
    }
    subs
}

/// The pinned versions `task` runs with, as recorded with each run.
pub fn pins(task: &str) -> String {
    let (subcommands, components) = needs(task);
    let mut pins: Vec<String> = TOOLS
        .iter()
        .filter(|t| subcommands.contains(&t.2))
        .map(|(krate, version, _)| format!("{krate} {version}"))
        .collect();
    if components.is_some() {
        pins.push(NIGHTLY.to_owned());
    }
    pins.join(", ")
}

/// The memory limit of `task` (of building, for fuzz), if it is a heavy one.
pub fn memory(task: &str) -> Option<String> {
    let default = match task {
        "review" | "deny" | "vet" => return None,
        "kani" => kani::MEMORY,
        _ => MEMORY,
    };
    Some(env::var("RUST_XTASK_MEMORY").unwrap_or_else(|_| default.to_owned()))
}

pub fn limit(task: &str) -> String {
    memory(task).unwrap_or_default()
}

/// What [`own_target`] keeps in a task's build directory: the packages it was built for.
const BUILT_FOR: &str = "xtask-built-for.txt";

/// Empties `target/<dir>`, a task's own build directory, unless it was built for the
/// workspace's packages as they are now: their names and where they are. Cargo, and the
/// tools' own clean, clear only what the packages there are now built, and what one renamed
/// or moved left behind is read as if it were still built: cargo-geiger 0.14.0 reads the
/// paths in its old `.d` files, and fails on those gone; cargo-llvm-cov 0.9.1 reports on its
/// old test programs too, their code never run, and the total drops.
pub fn own_target(root: &Path, dir: &str) -> Result<(), String> {
    let target = root.join("target").join(dir);
    let mark = target.join(BUILT_FOR);
    let now = packages(root)?;
    match fs::read_to_string(&mark) {
        Ok(was) if was == now => return Ok(()),
        _ if !target.exists() => {}
        _ => {
            record::say(&format!(
                "target/{dir}: built for other packages, or none recorded; emptied"
            ));
            fs::remove_dir_all(&target).or_else(|e| match e.kind() {
                io::ErrorKind::NotFound => Ok(()),
                _ => Err(format!("{}: {e}", target.display())),
            })?;
        }
    }
    fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    fs::write(&mark, now).map_err(|e| format!("{}: {e}", mark.display()))
}

/// The workspace's packages, one a line: the name, then the manifest's path.
fn packages(root: &Path) -> Result<String, String> {
    let out = cargo(root, &["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let mut lines: Vec<String> = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: no packages")?
        .iter()
        .map(|p| {
            let field = |k: &str| p[k].as_str().ok_or(format!("cargo metadata: a package's {k}"));
            Ok(format!("{} {}", field("name")?, field("manifest_path")?))
        })
        .collect::<Result<_, String>>()?;
    lines.sort();
    Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// What another package left is cleared once, and what the packages there are now
    /// built is kept after that.
    #[test]
    fn a_build_directory_keeps_only_this_workspaces_builds() {
        let root = crate::root();
        let dir = format!("xtask-test-{}", std::process::id());
        let target = root.join("target").join(&dir);
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(target.join("debug/old"), "").unwrap();
        fs::write(target.join(BUILT_FOR), "renamed /gone/Cargo.toml\n").unwrap();
        own_target(&root, &dir).unwrap();
        assert!(!target.join("debug/old").exists());
        let mark = fs::read_to_string(target.join(BUILT_FOR)).unwrap();
        assert!(mark.contains(&format!("xtask {}", root.join("xtask/Cargo.toml").display())));
        fs::write(target.join("new"), "").unwrap();
        own_target(&root, &dir).unwrap();
        assert!(target.join("new").exists());
        // Nothing recorded: what is there cannot be told apart.
        fs::remove_file(target.join(BUILT_FOR)).unwrap();
        own_target(&root, &dir).unwrap();
        assert!(!target.join("new").exists());
        fs::remove_dir_all(&target).unwrap();
    }
}
