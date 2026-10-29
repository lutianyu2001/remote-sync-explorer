use std::path::Path;
use std::process::Command;

use crate::run::{cargo, status};

pub fn run(root: &Path) -> Result<(), String> {
    // Every member: from the root package alone, xtask, which nothing depends on, would
    // be left out. Locked, as everywhere: cargo deny would rewrite a stale lock file and
    // pass, leaving the tree dirty for the tasks after it.
    status(cargo(root, &["deny", "--locked", "--workspace", "check"]))?;
    if !crate::tasks::TASKS.contains(&"fuzz") {
        return Ok(());
    }
    // fuzz/ is a workspace of its own, outside cargo vet: at least no known
    // vulnerability and no foreign source.
    status(fuzz_workspace(root))
}

/// cargo deny over the fuzz workspace: its advisories and sources.
pub fn fuzz_workspace(root: &Path) -> Command {
    cargo(
        root,
        &[
            "deny",
            "--locked",
            "--manifest-path",
            "fuzz/Cargo.toml",
            "check",
            "advisories",
            "sources",
        ],
    )
}
