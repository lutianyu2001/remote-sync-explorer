use std::path::Path;
use std::process::Command;

use crate::pins::NIGHTLY;
use crate::run::{capped, nightly, status};
use crate::tasks::{limit, own_target};

// Per project: the tests that call into the C library, and what they are built with.
/// What the tests are built with: `--lib` for unit tests, or the integration tests
/// (`-p <crate> --tests`), as for Miri. Here the integration tests, each of which calls into
/// librclone (the default features); no unit test does.
pub const BUILD: &[&str] = &[
    "--test",
    "addresses",
    "--test",
    "cancel",
    "--test",
    "concurrency",
    "--test",
    "rclone_ops",
    "--test",
    "resume",
    "--test",
    "rsync",
    "--test",
    "sftp_ssh",
    "--test",
    "startup",
];
/// The tests run, as test-name filters: the modules end in `::`, since a filter matches any
/// part of a name. Here every test of BUILD's.
const FILTER: &[&str] = &[""];

/// The hosts `AddressSanitizer` runs on here: Rust supports it on more, but these are the ones
/// its runtime is known to work on without building the standard library.
const HOSTS: &[&str] = &["x86_64-unknown-linux-", "aarch64-unknown-linux-"];

/// The tests of FILTER under `AddressSanitizer`. Only the code built for the target is
/// instrumented (with `--target`, build scripts and procedural macros are built without it),
/// and the standard library is not rebuilt with it (no `-Zbuild-std`, which would fetch the
/// standard library's dependencies, and a build offline from vendor/ cannot): reads and
/// writes through the C library's string and memory functions are caught all the same, as
/// the runtime intercepts them.
pub fn run(root: &Path) -> Result<(), String> {
    // Filters that match nothing run nothing, and pass.
    if FILTER.is_empty() {
        return Err("no test to run: list them in xtask/src/tasks/asan.rs".to_owned());
    }
    let host = host(root)?;
    if !HOSTS.iter().any(|h| host.starts_with(h)) {
        return Err(format!(
            "AddressSanitizer runs only on Linux, on x86_64 or aarch64; this host is {host}"
        ));
    }
    own_target(root, "asan")?;
    let mut test = nightly(root, &["test", "--locked", "--target", &host]);
    test.args(BUILD).arg("--").args(FILTER);
    test.env("CARGO_TARGET_DIR", root.join("target").join("asan"))
        .env("RUSTFLAGS", "-Zsanitizer=address")
        .env("RUSTDOCFLAGS", "-Zsanitizer=address")
        .env("ASAN_OPTIONS", "detect_leaks=1");
    status(capped(test, &limit("asan")))
}

/// The host's target triple, as the pinned nightly's rustc gives it.
fn host(root: &Path) -> Result<String, String> {
    let shown = format!("rustc +{NIGHTLY} -vV");
    let out = Command::new("rustc")
        .arg(format!("+{NIGHTLY}"))
        .arg("-vV")
        .current_dir(root)
        .output()
        .map_err(|e| format!("{shown}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{shown}: {}", out.status));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_owned())
        .ok_or_else(|| format!("{shown}: no host"))
}
