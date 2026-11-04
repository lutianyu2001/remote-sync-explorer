//! The security audit (see SECURITY.md): `cargo xtask <task>`.
//!
//! The tools and the nightly toolchain they need are pinned in pins.rs, so that every
//! machine and CI run the same versions; `cargo xtask setup` installs them. Each task
//! checks them first. Heavy tasks run one at a time (several only when asked for and the
//! memory allows), each in a systemd scope with a memory limit: cbmc (Kani's solver) and
//! fuzzing can take tens of gigabytes and freeze a machine without swap, so where there is
//! no such scope they do not run, unless `RUST_XTASK_NO_SCOPE=1` says the machine is one
//! of their own (as in CI). The latest run of each task, and the last that succeeded when
//! it is not that one, are kept in `audit/` and committed with the code (see record.rs).

use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use run::{scope, size};
use tasks::{KNOWN, TASKS, fuzz, memory, pins};

mod badges;
mod digest;
mod lock;
mod outdated;
mod pins;
mod record;
mod run;
mod setup;
mod tasks;

pub const USAGE: &str = "usage: cargo xtask <task> [args]

  setup [task...]   install the pinned tools and toolchain, for all tasks or the ones named
  deny              cargo deny over the workspace: sources, licenses, bans, advisories;
                    the fuzz workspace's advisories and sources
  vet               cargo vet: every dependency is audited
  geiger            unsafe code in the dependencies, into geiger.txt
  miri              the tests of the pure modules under Miri
  coverage          branch coverage of the tests: a table per file into summary.txt, and
                    HTML in target/llvm-cov/html
  kani [proof...]   every Kani proof, one at a time; each one's result, time and peak
                    memory into harnesses.txt. Naming proofs needs --no-record
  fuzz [seconds]    every fuzz target, 3600 seconds each unless given; fewer needs
                    --no-record. A run at least as long stands for the task
  all               those above that the project runs, in this order, each whatever the
                    others did
  status            how each task last ran, when it last succeeded, and whether it needs
                    a run
  needed <task> [args]
                    why the task, run with args, needs a run; exits 0 when it does, 1
                    when it does not, 2 on an error
  outdated [--vulns] pinned versions with a release a month newer, or a known
                    vulnerability (only that with --vulns); asks which to update, and
                    exits 2 when something is due and it cannot ask. Covers the actions
                    the workflows use too, pinned to commits
  badges <dir> --deny <outcome>
                    the README's badges, as shields.io endpoint files, into dir: whether
                    each task's latest result stands, deny's from the outcome given, and
                    whether a pinned version is due

A task's latest run is kept in audit/<task>/, to be committed: its output (run.log), when
and on which commit it ran and how it ended (run.txt), the files it read (inputs.txt), and
the files named above. When it failed, the last run that succeeded is in
audit/<task>/last_succeeded/. A run needs a working tree without uncommitted changes. Each
task ends with a line `xtask: <task>: succeeded`, `failed: ...`, `skipped: ...` (with
--if-needed) or `refused: ...` (a check before the run did not pass, and nothing ran), and
exits 0 when it succeeded or was skipped.

  --no-record       run on any working tree, into target/audit/<task>/ instead
  --if-needed       skip the task when its latest run stands: it succeeded, with the same
                    tools and arguments, on the same files it reads, and for deny, within
                    a day

Each heavy task runs in a systemd scope limited to RUST_XTASK_MEMORY (unless set, 4G; each
Kani proof has a scope of its own). The fuzz targets run together in one scope, at most
RUST_XTASK_JOBS at once (unless set, all), limited to RUST_XTASK_FUZZ_MEMORY (unless set,
1280M a target running at once). Where there is no such scope, heavy tasks are refused;
RUST_XTASK_NO_SCOPE=1 runs them without one. One task runs at a time; with
RUST_XTASK_PARALLEL=1 another may start beside it, unless one of them is kani and the
other fuzz, or their limits together come within 4G of the memory there is. Tasks look for
known vulnerabilities in the pinned tools every RUST_XTASK_VULNS_EVERY days (1) and for
newer releases every RUST_XTASK_RELEASES_EVERY days (7); 0 looks every run.";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((task, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let root = root();
    // `cargo run` keeps the caller's directory; everything below is from the root.
    if let Err(e) = env::set_current_dir(&root) {
        eprintln!("xtask: {}: {e}", root.display());
        return ExitCode::FAILURE;
    }
    let (flags, rest): (Vec<String>, Vec<String>) =
        rest.iter().cloned().partition(|a| a == "--no-record" || a == "--if-needed");
    let opts = Opts {
        record: !flags.iter().any(|f| f == "--no-record"),
        if_needed: flags.iter().any(|f| f == "--if-needed"),
    };
    let rest = rest.as_slice();
    let result = match task.as_str() {
        // Not while a task uses the tools.
        "setup" => lock::exclusive(&root).and_then(|_held| setup::setup(rest)),
        "outdated" => return outdated::main(&root, rest),
        "badges" => {
            return badges::main(&root, rest, TASKS, |t| {
                needed(&root, t, &params(t)).unwrap_or_else(|e| vec![e])
            });
        }
        // Run by this program inside a scope; they take no lock, being part of the task
        // that holds it.
        "__memory-max" => return run::memory_max(),
        "__fuzz-targets" => return fuzz::targets(&root, rest),
        "__kani-harness" => return tasks::kani::harness(&root, rest),
        "all" if !rest.is_empty() => Err(format!("all takes no arguments\n\n{USAGE}")),
        // Holding the lock between the tasks too: not having it is the one reason to stop
        // before them all.
        "all" => match lock::session(&root) {
            Ok(_held) => {
                let outcomes: Vec<Outcome> =
                    TASKS.iter().map(|t| run(&root, t, &[], opts)).collect();
                return if outcomes.iter().all(Outcome::ok) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            Err(e) => return refused("all", &e).exit(),
        },
        "status" => {
            record::status(&root, TASKS, |t| {
                needed(&root, t, &params(t)).unwrap_or_else(|e| vec![e])
            });
            Ok(())
        }
        "needed" => match rest {
            [t, args @ ..] if TASKS.contains(&t.as_str()) => {
                let wanted = plan(t, args, true).map(|p| p.params);
                return match wanted.and_then(|params| needed(&root, t, &params)) {
                    Ok(why) if why.is_empty() => {
                        println!("{t}: up to date");
                        ExitCode::from(1)
                    }
                    Ok(why) => {
                        println!("{t}: {}", why.join("; "));
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("xtask: {e}");
                        ExitCode::from(2)
                    }
                };
            }
            _ => {
                // Not 1, which would read as the result standing.
                eprintln!("xtask: usage: cargo xtask needed <task> [args]\n\n{USAGE}");
                return ExitCode::from(2);
            }
        },
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        t if TASKS.contains(&t) => {
            return match lock::session(&root) {
                Ok(_held) => run(&root, t, rest, opts),
                Err(e) => refused(t, &e),
            }
            .exit();
        }
        _ => Err(format!("unknown task {task}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace's root: the parent of this crate.
fn root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap_or(here).to_owned()
}

#[derive(Clone, Copy)]
struct Opts {
    /// Kept in `audit/`, to be committed (not `--no-record`).
    record: bool,
    /// Skipped when the latest run stands (`--if-needed`).
    if_needed: bool,
}

/// How a task ended.
enum Outcome {
    Succeeded,
    Failed(String),
    /// Its latest run stands (`--if-needed`).
    Skipped(String),
    /// A check before the run did not pass; nothing ran, and nothing was recorded.
    Refused(String),
}

impl Outcome {
    fn ok(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Skipped(_))
    }

    fn exit(&self) -> ExitCode {
        if self.ok() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Succeeded => write!(f, "succeeded"),
            Self::Failed(why) => write!(f, "failed: {why}"),
            Self::Skipped(why) => write!(f, "skipped: {why}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

fn refused(task: &str, why: &str) -> Outcome {
    let outcome = Outcome::Refused(why.to_owned());
    eprintln!("xtask: {task}: {outcome}");
    outcome
}

// Per project: the files the code reads as data (with include_str!, from build.rs, or as a
// test's sample): never documentation, whatever their names, and hashed as they are.
const READ_AS_DATA: [&str; 0] = [];

// Per project: what only Kani compiles (`#[cfg(kani)]`), which no other task can find changed.
const PROOFS: [&str; 0] = [];

/// Whether a change to `path` can change what `task` finds, besides the tools pinned for
/// it. Documentation, CI and the results themselves change nothing.
fn affects(task: &str, path: &str) -> bool {
    let under = |names: &[&str]| {
        names.iter().any(|n| path == *n || path.strip_prefix(n).is_some_and(|p| p.starts_with('/')))
    };
    if PROOFS.contains(&path) {
        return task == "kani";
    }
    // Where a project has it: what reads Kani's list of proofs, and the workspace's packages
    // before geiger and coverage build (own_target).
    if path == "xtask/src/json.rs" {
        return matches!(task, "geiger" | "coverage" | "kani");
    }
    let module = |t: &str| format!("xtask/src/tasks/{t}.rs");
    // The dependencies, the toolchain, how the tasks run and the task's own code: not the
    // other tasks' code, nor what changes no run (how runs are recorded and kept apart, and
    // the tools, whose versions each run records).
    // A task the project does not run counts as another: its module changes no run.
    let other = KNOWN.iter().any(|t| *t != task && path == module(t));
    let none =
        ["record.rs", "digest.rs", "pins.rs", "lock.rs", "setup.rs", "outdated.rs", "badges.rs"]
            .map(|f| format!("xtask/src/{f}"));
    // Every member's manifest too: licenses, dependency kinds and versions that Cargo.lock
    // does not show.
    let manifest = path == "Cargo.toml" || path.ends_with("/Cargo.toml") && !under(&["fuzz"]);
    // How code is formatted and linted, wherever it is (xtask/ has its own): no run reads it.
    let style = Path::new(path).file_name().is_some_and(|n| {
        ["rustfmt.toml", ".rustfmt.toml", "clippy.toml", ".clippy.toml"].iter().any(|s| n == *s)
    });
    let every = manifest
        || under(&["Cargo.lock", ".cargo", "rust-toolchain.toml"])
        || under(&["xtask"]) && !other && !style && !none.iter().any(|f| path == f);
    if every {
        return true;
    }
    let top = path.split('/').next().unwrap_or(path);
    let documentation = Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
        || top.starts_with("README")
        || top.starts_with("LICENSE")
        || under(&[".github", ".gitignore"])
        || style;
    // What the code reads counts for every code task, wherever it is: a sample under tests/
    // that a unit test reads too.
    let code = |also: &[&str]| {
        READ_AS_DATA.contains(&path)
            || !documentation && !under(&["xtask", "deny.toml", "supply-chain"]) && !under(also)
    };
    match task {
        "deny" => under(&["deny.toml", "fuzz/Cargo.toml", "fuzz/Cargo.lock"]),
        "vet" => under(&["supply-chain"]),
        "fuzz" => code(&["tests"]),
        // `--lib`: not the integration tests.
        "geiger" | "kani" | "miri" => code(&["tests", "fuzz"]),
        _ => code(&["fuzz"]),
    }
}

/// The files `task` reads, with the hashes of what can change a run in them (see
/// digest.rs), as inputs.txt has them.
fn inputs(root: &Path, task: &str) -> Result<String, String> {
    let read = record::files(root)?.into_iter().filter(|(path, _)| affects(task, path)).collect();
    let mut text = format!("{}\n", digest::FORMAT);
    for (path, hash) in digest::hashes(root, read, &READ_AS_DATA)? {
        let _ = writeln!(text, "{hash}\t{path}");
    }
    Ok(text)
}

/// Why `task`, run with `params`, needs a run; empty when its latest run stands.
fn needed(root: &Path, task: &str, params: &str) -> Result<Vec<String>, String> {
    Ok(record::needed(root, task, &inputs(root, task)?, &pins(task), params))
}

/// What a run of `task` is to do, from its arguments.
#[derive(Default)]
pub struct Plan {
    /// As run.txt records them: `seconds=3600` for fuzz, nothing for the others.
    pub params: String,
    /// How long each fuzz target runs.
    pub seconds: u32,
    /// The Kani proofs named; all of them when none is.
    pub proofs: Vec<String>,
    /// The fuzz targets, how many run at once, and the memory of their scope.
    pub targets: Vec<String>,
    pub jobs: usize,
    pub fuzz_memory: String,
}

/// The plan for `task` with `args`; an error when they are mistaken, or when a run with
/// them could not stand for the task and would be recorded.
fn plan(task: &str, args: &[String], record: bool) -> Result<Plan, String> {
    let mut plan = Plan::default();
    match (task, args) {
        ("fuzz", []) => plan.seconds = fuzz::SECONDS,
        ("fuzz", [s]) => {
            plan.seconds = s.parse().map_err(|_| format!("not a number of seconds: {s}"))?;
            if plan.seconds == 0 {
                // libFuzzer reads -max_total_time=0 as no limit at all.
                return Err("fuzz 0 would never end; give a number of seconds".to_owned());
            }
        }
        ("fuzz", _) => return Err("usage: cargo xtask fuzz [seconds]".to_owned()),
        ("kani", proofs) => plan.proofs = proofs.to_vec(),
        (_, []) => {}
        (_, _) => return Err(format!("{task} takes no arguments")),
    }
    if task == "fuzz" {
        plan.params = format!("seconds={}", plan.seconds);
    }
    if record && !plan.proofs.is_empty() {
        return Err(
            "a run of some of the proofs cannot stand for them all; add --no-record".to_owned()
        );
    }
    if record && task == "fuzz" && plan.seconds < fuzz::SECONDS {
        return Err(format!(
            "a run of fewer than {} seconds would take the place of a full one; \
             add --no-record",
            fuzz::SECONDS
        ));
    }
    Ok(plan)
}

/// The parameters of a run of `task` without arguments.
fn params(task: &str) -> String {
    plan(task, &[], true).map(|p| p.params).unwrap_or_default()
}

/// What `run` does before a run: skip it, or a plan and the memory it may take.
enum Ready {
    Skip(String),
    Go(Plan, u64),
}

/// The checks before a run (see USAGE): none of them starts a run or records anything.
fn prepare(root: &Path, task: &str, args: &[String], opts: Opts) -> Result<Ready, String> {
    let mut plan = plan(task, args, opts.record)?;
    if opts.if_needed {
        let why = needed(root, task, &plan.params)?;
        if why.is_empty() {
            return Ok(Ready::Skip("its latest run stands".to_owned()));
        }
        eprintln!("xtask: {task}: needs a run: {}", why.join("; "));
    }
    outdated::notice(root);
    setup::check(task)?;
    if opts.record {
        let changed = record::uncommitted(root);
        if !changed.is_empty() {
            let more = if changed.len() > 3 { " and more" } else { "" };
            return Err(format!(
                "uncommitted changes ({}{more}): the results are recorded by commit, so commit \
                 them first, or run with --no-record",
                changed[..changed.len().min(3)].join(", ")
            ));
        }
    }
    if !record::tracked(root, "Cargo.lock") {
        return Err("Cargo.lock is not tracked by git, and whether a result stands depends \
                    on it"
            .to_owned());
    }
    let memory = memory(task);
    if memory.is_some() {
        scope()?;
    }
    let mut bytes = memory.as_deref().map_or(Ok(0), size)?;
    if task == "fuzz" {
        bytes = bytes.max(fuzz::prepare(root, &mut plan)?);
    }
    Ok(Ready::Go(plan, bytes))
}

/// Runs `task` after the checks before it, keeps its output (see record.rs), and says how
/// it ended in its last line.
fn run(root: &Path, task: &str, args: &[String], opts: Opts) -> Outcome {
    let (plan, bytes) = match prepare(root, task, args, opts) {
        Ok(Ready::Go(plan, bytes)) => (plan, bytes),
        Ok(Ready::Skip(why)) => {
            let outcome = Outcome::Skipped(why);
            eprintln!("xtask: {task}: {outcome}");
            return outcome;
        }
        Err(e) => return refused(task, &e),
    };
    let started =
        inputs(root, task).and_then(|inputs| record::start(root, task, bytes, opts.record, inputs));
    let run = match started {
        Ok(run) => run,
        Err(e) => return refused(task, &e),
    };
    let dir = run.dir().to_owned();
    let result = tasks::run(root, task, &dir, &plan);
    let outcome = match result.and_then(|()| run.moved().map_or(Ok(()), Err)) {
        Ok(()) => Outcome::Succeeded,
        Err(e) => Outcome::Failed(e),
    };
    let line = format!("xtask: {task}: {outcome}");
    record::note(&line);
    if let Err(e) = run.finish(&outcome.to_string(), &plan.params, &pins(task)) {
        let outcome = Outcome::Failed(format!("its results were not kept: {e}"));
        eprintln!("xtask: {task}: {outcome}");
        return outcome;
    }
    if opts.record {
        eprintln!("xtask: {task}: output kept in audit/{task}/");
        if root.join("audit").join(task).join(record::LAST).is_dir() {
            eprintln!(
                "xtask: {task}: the last run that succeeded is in audit/{task}/{}/",
                record::LAST
            );
        }
    } else {
        eprintln!("xtask: {task}: output in target/audit/{task}/, not recorded");
    }
    eprintln!("{line}");
    outcome
}

#[cfg(test)]
mod tests {
    use super::{affects, plan};

    #[test]
    fn each_task_reads_its_own_files() {
        let reads = |path: &str| -> Vec<&str> {
            super::TASKS.iter().copied().filter(|t| affects(t, path)).collect()
        };
        let all = super::TASKS.to_vec();
        // Of the tasks named, those the project runs.
        let only = |tasks: &[&'static str]| -> Vec<&'static str> {
            tasks.iter().copied().filter(|t| super::TASKS.contains(t)).collect()
        };
        for path in ["Cargo.lock", "Cargo.toml", "crates/a/Cargo.toml", ".cargo/config.toml"] {
            assert_eq!(reads(path), all, "{path}");
        }
        for path in
            ["xtask/src/main.rs", "xtask/src/tasks/mod.rs", "xtask/src/run.rs", "xtask/src/new.rs"]
        {
            assert_eq!(reads(path), all, "{path}");
        }
        for path in [
            "xtask/src/record.rs",
            "xtask/src/digest.rs",
            "xtask/src/pins.rs",
            "xtask/src/lock.rs",
            "xtask/src/setup.rs",
            "xtask/src/outdated.rs",
            "xtask/src/badges.rs",
        ] {
            assert_eq!(reads(path), Vec::<&str>::new(), "{path}");
        }
        assert_eq!(reads("xtask/src/tasks/kani.rs"), only(&["kani"]));
        assert_eq!(reads("xtask/src/json.rs"), only(&["geiger", "coverage", "kani"]));
        assert_eq!(reads("xtask/src/tasks/fuzz.rs"), only(&["fuzz"]));
        // Per project: paths of this project, by which files each task reads.
        assert_eq!(reads("README.md"), Vec::<&str>::new());
        assert_eq!(reads("README/README_CN.md"), Vec::<&str>::new());
        assert_eq!(reads("LICENSE"), Vec::<&str>::new());
        assert_eq!(reads("xtask/rustfmt.toml"), Vec::<&str>::new());
        assert_eq!(reads(".github/workflows/verify.yml"), Vec::<&str>::new());
        assert_eq!(reads("deny.toml"), only(&["deny"]));
        assert_eq!(reads("supply-chain/audits.toml"), only(&["vet"]));
        assert_eq!(reads("fuzz/Cargo.lock"), only(&["deny", "fuzz"]));
        assert_eq!(reads("fuzz/fuzz_targets/text.rs"), only(&["fuzz"]));
        assert_eq!(reads("tests/startup.rs"), only(&["coverage"]));
        assert_eq!(reads("src/lib.rs"), only(&["geiger", "miri", "coverage", "kani", "fuzz"]));
        let code = only(&["geiger", "miri", "coverage", "kani", "fuzz"]);
        assert_eq!(reads("locales/en/main.ftl"), code);
    }

    #[test]
    fn runs_that_cannot_stand_for_the_task_are_not_recorded() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(plan("fuzz", &[], true).map(|p| p.params), Ok("seconds=3600".to_owned()));
        assert!(plan("fuzz", &args(&["600"]), true).is_err());
        let longer = plan("fuzz", &args(&["14400"]), true).map(|p| p.params);
        assert_eq!(longer, Ok("seconds=14400".to_owned()));
        assert_eq!(plan("fuzz", &args(&["600"]), false).map(|p| p.seconds), Ok(600));
        assert!(plan("fuzz", &args(&["0"]), false).is_err());
        assert!(plan("fuzz", &args(&["soon"]), false).is_err());
        assert!(plan("kani", &args(&["a::proofs::b"]), true).is_err());
        assert!(plan("kani", &args(&["a::proofs::b"]), false).is_ok());
        assert!(plan("miri", &args(&["x"]), false).is_err());
        assert_eq!(plan("miri", &[], true).map(|p| p.params), Ok(String::new()));
    }
}
