use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Instant;

use serde_json::Value;

use crate::record;
use crate::run::{
    capped, cargo, cgroup, oom_policy, scoped, scopes, shown, size, status, this_program,
};
use crate::tasks::limit;

// Per project: the limit, and where the most a proof took was read.
/// The memory limit of each Kani proof, unless `RUST_XTASK_MEMORY` says otherwise: 4G, or
/// 1.5 times the most a proof took, whichever is larger. The most was 1017 MiB (keymap's,
/// in audit/kani/harnesses.txt of 2026-11-01); set it again when the proofs change.
pub const MEMORY: &str = "4G";

/// How a proof's peak memory is reported, as the last line of its log.
const PEAK: &str = "xtask: peak memory in bytes:";

// Per project: the features and targets the proofs are built with. Here without the default
// features, which bring in librclone and which no proof needs.
/// What the proofs are built with, when they are listed and when each runs. `cargo kani
/// list` takes these only before `list`, and its own `-Z` after it.
const BUILD: [&str; 2] = ["--no-default-features", "--lib"];

// Per project: Kani's unstable features the proofs use, for listing them and running them;
// for example `-Z function-contracts` for `#[kani::proof_for_contract]`.
const UNSTABLE: [&str; 0] = [];

/// How Kani runs one proof (after `cargo kani`, given its name).
fn proof(harness: &str) -> Vec<&str> {
    let mut args = BUILD.to_vec();
    args.extend(UNSTABLE);
    args.extend(["--harness", harness, "--exact"]);
    args.extend(["-Z", "unstable-options", "--harness-timeout", "1800s"]);
    args
}

/// The Kani proofs named (every one when none is), one at a time, each in a scope of its
/// own that measures its peak memory.
pub fn run(root: &Path, dir: &Path, named: &[String]) -> Result<(), String> {
    let memory = limit("kani");
    let mut list = cargo(root, &["kani"]);
    list.args(BUILD).args(["list", "--format", "json"]).args(UNSTABLE);
    status(capped(list, &memory))?;
    let listed = root.join("kani-list.json");
    let text = fs::read_to_string(&listed).map_err(|e| format!("{}: {e}", listed.display()))?;
    let _ = fs::rename(&listed, dir.join("kani-list.json"));
    let json: Value = serde_json::from_str(&text).map_err(|e| format!("kani-list.json: {e}"))?;
    let mut harnesses = Vec::new();
    for kind in ["standard-harnesses", "contract-harnesses"] {
        for names in json[kind].as_object().into_iter().flat_map(|files| files.values()) {
            harnesses.extend(names.as_array().into_iter().flatten().filter_map(Value::as_str));
        }
    }
    if harnesses.is_empty() {
        return Err("cargo kani list found no proof".to_owned());
    }
    if let Some(unknown) = named.iter().find(|n| !harnesses.contains(&n.as_str())) {
        return Err(format!("no proof {unknown}; there are {}", harnesses.join(", ")));
    }
    if !named.is_empty() {
        harnesses.retain(|h| named.iter().any(|n| n == h));
    }
    let mut table = String::new();
    let mut failed = Vec::new();
    // One failing, even before it runs, stops none of the others.
    for harness in harnesses {
        let (ok, outcome) = measure(root, dir, harness, &memory)
            .unwrap_or_else(|e| (false, format!("failed before it ran ({e})")));
        if !ok {
            failed.push(harness.to_owned());
        }
        let line = format!("{harness}: {outcome}");
        record::say(&line);
        table.push_str(&line);
        table.push('\n');
    }
    let _ = fs::write(dir.join("harnesses.txt"), table);
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("Kani proofs failed: {}", failed.join(", ")))
    }
}

/// Runs one proof in a scope of its own: whether it succeeded, and how it ended, with its
/// time and peak memory, as harnesses.txt has them.
fn measure(root: &Path, dir: &Path, harness: &str, memory: &str) -> Result<(bool, String), String> {
    // Built beforehand, so that its time and memory include no compiling: Kani builds the
    // crate again for each proof, and the dependencies for the first.
    let mut build = cargo(root, &["kani"]);
    build.args(proof(harness)).arg("--only-codegen");
    status(capped(build, memory))?;
    let name = format!("kani-{}.log", harness.replace("::", "."));
    let path = dir.join(&name);
    let log = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let err = log.try_clone().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cmd = Command::new(this_program());
    cmd.args(["__kani-harness", harness]).current_dir(root);
    if scopes() {
        cmd.arg("--peak");
    }
    // When the proof runs out of memory, systemd's default policy would end the whole
    // scope, this program's peak report included; with this one the kernel ends cbmc.
    // Before systemd 253 a scope takes no such policy, and the report may then be lost.
    let policy: &[&str] = if oom_policy() { &["OOMPolicy=continue"] } else { &[] };
    let mut cmd = scoped(cmd, memory, policy);
    cmd.stdout(log).stderr(err);
    record::say(&format!("$ {} > {name}", shown(&cmd)));
    let started = Instant::now();
    let exit = cmd.status().map_err(|e| e.to_string())?;
    let secs = started.elapsed().as_secs();
    let peak = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(PEAK)?.trim().parse::<u64>().ok());
    let full = peak.is_some_and(|p| size(memory).is_ok_and(|limit| p >= limit));
    let peak = peak.map_or_else(|| "unknown".to_owned(), mib);
    let outcome = if exit.success() {
        "succeeded".to_owned()
    } else {
        let full = if full { ", at the memory limit" } else { "" };
        format!("failed ({exit}{full}; see {name})")
    };
    Ok((exit.success(), format!("{outcome}, {secs} s, peak memory {peak}")))
}

fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes.div_ceil(1 << 20))
}

/// `__kani-harness <name> [--peak]`, run inside the proof's scope: the proof, then with
/// `--peak` (when this is a scope of its own) the most memory the scope used.
pub fn harness(root: &Path, args: &[String]) -> ExitCode {
    let Some(harness) = args.first() else { return ExitCode::FAILURE };
    let mut cmd = cargo(root, &["kani"]);
    cmd.args(proof(harness));
    eprintln!("$ {}", shown(&cmd));
    let status = cmd.status();
    if args.iter().any(|a| a == "--peak")
        && let Some(peak) = cgroup("memory.peak")
    {
        eprintln!("{PEAK} {peak}");
    }
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{harness}: {e}");
            ExitCode::FAILURE
        }
    }
}
