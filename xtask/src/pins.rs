//! The pinned versions, so that every machine and CI run the same ones; `cargo xtask setup`
//! installs them, and `cargo xtask outdated` writes newer ones back here. This file is no
//! task's input (see `affects`): a new version of a tool makes only the tasks that use it
//! need a run, through the tools each run records.

/// The nightly toolchain for fuzzing, Miri, `AddressSanitizer` and coverage. Kani brings its own.
pub const NIGHTLY: &str = "nightly-2026-10-28";

/// The stable toolchain the plugins below are built with: new enough for each (cargo-geiger
/// 0.14.0 needs 1.97), whatever rust-toolchain.toml pins, and not a nightly, on which some
/// of their dependencies turn on features that newer compilers refuse. Raised by hand when
/// a new version of a plugin needs a newer one.
pub const STABLE: &str = "1.99.1";

/// The cargo plugins: crate, version, and the cargo subcommand it adds.
pub const TOOLS: [(&str, &str, &str); 6] = [
    ("cargo-deny", "0.20.2", "deny"),
    ("cargo-vet", "0.10.3", "vet"),
    ("cargo-geiger", "0.14.0", "geiger"),
    ("cargo-fuzz", "0.13.2", "fuzz"),
    ("cargo-llvm-cov", "0.9.1", "llvm-cov"),
    ("kani-verifier", "0.68.0", "kani"),
];
