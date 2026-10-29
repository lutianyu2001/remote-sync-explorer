//! The tasks: what each needs, how much memory it may take, and its code in a module of its
//! own, so that a change to one task's code makes only that task need a run (see
//! `affects`).

use std::env;
use std::path::Path;

use crate::Plan;
use crate::pins::{NIGHTLY, TOOLS};

pub mod coverage;
pub mod deny;
pub mod fuzz;
pub mod geiger;
pub mod kani;
pub mod miri;
pub mod vet;

/// Every task there is, in the order `all` runs them: the cheap ones first.
pub const KNOWN: [&str; 7] = ["deny", "vet", "geiger", "miri", "coverage", "kani", "fuzz"];

// Per project: the tasks of KNOWN this project runs, in the same order. Not kani before
// the first proof, nor fuzz before the first target: a task with nothing to check fails.
/// The tasks this project runs.
pub const TASKS: &[&str] = &KNOWN;

/// The memory limit of the heavy tasks other than Kani, and of building the fuzz targets.
const MEMORY: &str = "4G";

/// Runs `task`, as `plan` says, writing its own files into `dir`.
pub fn run(root: &Path, task: &str, dir: &Path, plan: &Plan) -> Result<(), String> {
    match task {
        "deny" => deny::run(root),
        "vet" => vet::run(root),
        "geiger" => geiger::run(root, dir),
        "miri" => miri::run(root),
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
        "deny" | "vet" => return None,
        "kani" => kani::MEMORY,
        _ => MEMORY,
    };
    Some(env::var("RUST_XTASK_MEMORY").unwrap_or_else(|_| default.to_owned()))
}

pub fn limit(task: &str) -> String {
    memory(task).unwrap_or_default()
}
