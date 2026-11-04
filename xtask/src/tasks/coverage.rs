use std::fs;
use std::path::Path;

use crate::record;
use crate::run::{capped, nightly, shown, status};
use crate::tasks::{limit, own_target};

/// The tests under coverage, then the HTML for reading (in target/, not committed) and
/// the table per file into the run.
pub fn run(root: &Path, dir: &Path) -> Result<(), String> {
    // --no-report keeps what earlier runs left in target/, and report would merge it in:
    // branches only older code or removed tests reached would count as covered.
    own_target(root, "llvm-cov-target")?;
    status(nightly(root, &["llvm-cov", "clean", "--workspace"]))?;
    status(capped(
        nightly(root, &["llvm-cov", "--locked", "--branch", "--no-report"]),
        &limit("coverage"),
    ))?;
    status(nightly(root, &["llvm-cov", "report", "--html"]))?;
    let path = dir.join("summary.txt");
    let file = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cmd = nightly(root, &["llvm-cov", "report"]);
    record::say(&format!("$ {} > summary.txt", shown(&cmd)));
    match record::exec(&mut cmd, Some(file)) {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{}: {s}", shown(&cmd))),
        Err(e) => Err(format!("{}: {e}", shown(&cmd))),
    }
}
