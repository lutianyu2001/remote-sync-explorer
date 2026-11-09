use std::env;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitCode};

use crate::Plan;
use crate::run::{capped, nightly, shown, size, status, this_program};
use crate::tasks::{deny, limit};

/// How long `cargo xtask fuzz` runs each target, unless told otherwise.
pub const SECONDS: u32 = 3600;
/// The memory of one fuzz target, as libFuzzer's `-rss_limit_mb` (its default), and what
/// the scope allows each target running at once: some more, so that libFuzzer's own limit
/// acts first and tells which input took it. A target's memory grows over hours without
/// any input keeping it: layeredcfg's, whose inputs make many small allocations of every
/// size, went from 620M to 1G in two hours (20 million runs) with 48M of it in use, the
/// rest the sanitizer's and libFuzzer's own. One allocation, though, is held to
/// `-malloc_limit_mb`: an input that asks for that much at once is a finding.
const RSS_MB: u64 = 2048;
const SCOPE_MB: u64 = 2304;
const MALLOC_MB: u64 = 1024;

/// The checks before a run: the fuzz workspace's dependencies, the targets, how many run
/// at once and the memory of their scope, into `plan`; the most memory the run may take.
pub fn prepare(root: &Path, plan: &mut Plan) -> Result<u64, String> {
    // libfuzzer-sys compiles C++: its dependencies are checked before they are built,
    // and fuzz/Cargo.lock found stale before cargo fuzz build, which has no --locked,
    // rewrites it.
    status(deny::fuzz_workspace(root))
        .map_err(|e| format!("the fuzz workspace's dependencies: {e}"))?;
    plan.targets = list(root)?;
    plan.jobs = jobs()?.min(plan.targets.len());
    plan.fuzz_memory = memory(plan.jobs)?;
    size(&plan.fuzz_memory)
}

/// The fuzz targets, from fuzz/Cargo.toml alone, without building anything.
fn list(root: &Path) -> Result<Vec<String>, String> {
    let list =
        nightly(root, &["fuzz", "list"]).output().map_err(|e| format!("cargo fuzz list: {e}"))?;
    if !list.status.success() {
        return Err(format!(
            "cargo fuzz list: {}: {}",
            list.status,
            String::from_utf8_lossy(&list.stderr).trim()
        ));
    }
    let targets: Vec<String> =
        String::from_utf8_lossy(&list.stdout).split_whitespace().map(str::to_owned).collect();
    if targets.is_empty() {
        return Err("cargo fuzz list found no target".to_owned());
    }
    Ok(targets)
}

/// How many fuzz targets run at once (`RUST_XTASK_JOBS`), all of them unless set.
fn jobs() -> Result<usize, String> {
    match env::var("RUST_XTASK_JOBS") {
        Err(_) => Ok(usize::MAX),
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("RUST_XTASK_JOBS={v}: not a number of targets")),
    }
}

/// The memory limit of the scope the fuzz targets run in, `jobs` at a time.
fn memory(jobs: usize) -> Result<String, String> {
    let least = u64::try_from(jobs).unwrap_or(u64::MAX).saturating_mul(SCOPE_MB);
    let Ok(memory) = env::var("RUST_XTASK_FUZZ_MEMORY") else {
        return Ok(format!("{least}M"));
    };
    if size(&memory)? < least.saturating_mul(1 << 20) {
        // The scope would end them all before libFuzzer's own limit ends one.
        return Err(format!(
            "RUST_XTASK_FUZZ_MEMORY={memory} is less than the {least}M that {jobs} targets \
             at once need ({SCOPE_MB}M each)"
        ));
    }
    Ok(memory)
}

pub fn run(root: &Path, dir: &Path, plan: &Plan) -> Result<(), String> {
    // Built first in a scope of its own: compiling libfuzzer-sys's C++ takes as much
    // memory however many targets there are.
    status(capped(nightly(root, &["fuzz", "build"]), &limit("fuzz")))?;
    // One scope for all the targets, so that the limit holds for them together.
    let mut cmd = Command::new(this_program());
    cmd.arg("__fuzz-targets").arg(dir).arg(plan.seconds.to_string()).arg(plan.jobs.to_string());
    cmd.args(&plan.targets).current_dir(root);
    status(capped(cmd, &plan.fuzz_memory))
        .map_err(|_| "fuzz targets failed; see run.log and fuzz-<target>.log".to_owned())
}

/// `__fuzz-targets <dir> <seconds> <jobs> <target>...`, run inside the scope: the targets,
/// `jobs` at a time, each for the whole time.
pub fn targets(root: &Path, args: &[String]) -> ExitCode {
    let [dir, secs, jobs, targets @ ..] = args else { return ExitCode::FAILURE };
    let Some(jobs) = jobs.parse::<usize>().ok().filter(|&n| n > 0) else {
        return ExitCode::FAILURE;
    };
    let mut failed = Vec::new();
    for batch in targets.chunks(jobs) {
        let mut running: Vec<(&String, Child)> = Vec::new();
        for target in batch {
            let log = Path::new(dir).join(format!("fuzz-{target}.log"));
            let spawned = fs::File::create(&log).and_then(|out| {
                let err = out.try_clone()?;
                let mut cmd = nightly(root, &["fuzz", "run", target, "--"]);
                cmd.arg(format!("-max_total_time={secs}"));
                cmd.arg(format!("-rss_limit_mb={RSS_MB}"));
                cmd.arg(format!("-malloc_limit_mb={MALLOC_MB}"));
                cmd.arg("-timeout=10").stdout(out).stderr(err);
                eprintln!("$ {} > fuzz-{target}.log", shown(&cmd));
                cmd.spawn()
            });
            match spawned {
                Ok(child) => running.push((target, child)),
                Err(e) => failed.push(format!("{target} ({e})")),
            }
        }
        for (target, mut child) in running {
            if !child.wait().is_ok_and(|s| s.success()) {
                failed
                    .push(format!("{target} (see fuzz-{target}.log and fuzz/artifacts/{target}/)"));
            }
        }
    }
    if failed.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("xtask: fuzz targets failed: {}", failed.join(", "));
    ExitCode::FAILURE
}
