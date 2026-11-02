use std::path::Path;

use crate::run::{capped, nightly, status};
use crate::tasks::limit;

// Per project: the modules, the features and the skipped tests.
/// The modules whose tests Miri runs: those that start no process, open no socket and
/// wait on no worker thread. The names end in `::`, since test filters match parts of
/// names, and an interface test whose name merely contains one would time out.
const MODULES: &[&str] = &[
    "rclone::obscure::",
    "rclone::ssh_passwords::",
    "config::",
    "jobs::",
    "event::",
    "keymap::",
    "location::",
    "endpoint::",
    "ui::text::",
    "ui::panes::tests::",
    "hitmap::",
    "state::",
    "engine::",
];
/// What the tests are built with: here without the default features, which bring in
/// librclone.
const BUILD: &[&str] = &["--no-default-features", "--lib"];
/// Tests too slow under Miri: the exhaustive base64 test.
const SKIP: &[&str] = &["base64_round_trips_every_short_input"];

pub fn run(root: &Path) -> Result<(), String> {
    // Filters that match nothing run nothing, and pass.
    if MODULES.is_empty() {
        return Err("no module to test: list them in xtask/src/tasks/miri.rs".to_owned());
    }
    let mut miri = nightly(root, &["miri", "test", "--locked"]);
    miri.args(BUILD).arg("--").args(MODULES);
    for test in SKIP {
        miri.args(["--skip", test]);
    }
    // Tests read and write temporary files and /dev/urandom.
    miri.env("MIRIFLAGS", "-Zmiri-disable-isolation");
    status(capped(miri, &limit("miri")))
}
