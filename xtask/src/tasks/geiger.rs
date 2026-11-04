use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::record;
use crate::run::{capped, cargo, shown, status};
use crate::tasks::{limit, own_target};

/// Where the copy of the work tree that geiger scans is made.
const COPY: &str = "target/geiger-src";

pub fn run(root: &Path, dir: &Path) -> Result<(), String> {
    // geiger reads the dependencies' sources, which a build without vendor/ has fetched
    // only for the host's targets.
    status(cargo(root, &["fetch", "--locked"]))?;
    own_target(root, "geiger")?;
    let copy = copy(root)?;
    let path = dir.join("geiger.txt");
    let file = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // geiger cleans before it builds; in target/debug that would take everyone's build.
    let mut scan = cargo(&copy, &["geiger", "--locked", "--offline", "--output-format", "Ascii"]);
    scan.env("CARGO_TARGET_DIR", root.join("target/geiger"));
    // It colours the table as cargo's own output is coloured, and CI sets that to always.
    scan.env("CARGO_TERM_COLOR", "never");
    let mut cmd = capped(scan, &limit("geiger"));
    record::say(&format!("$ {} > geiger.txt", shown(&cmd)));
    let ended = record::exec(&mut cmd, Some(file)).map_err(|e| format!("cargo geiger: {e}"))?;
    let table = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // geiger exits non-zero when files it was shown are not Rust (READMEs, included
    // text), so the table itself tells whether it ran; a signal says it did not finish.
    if ended.code().is_none() {
        return Err(format!("cargo geiger did not finish: {ended}"));
    }
    if !complete(&table) {
        return Err(format!("cargo geiger wrote no whole table: {ended}"));
    }
    if !ended.success() {
        record::say(&format!("cargo geiger: {ended}, with a whole table"));
    }
    Ok(())
}

/// A copy of the work tree in [`COPY`], what git ignores in it (vendor/) linked rather
/// than copied. geiger counts every `.rs` file under a package's directory as that
/// package's, and follows no link (cargo-geiger 0.14.0, `src/scan/find.rs`): scanning
/// the root package where vendor/ is a directory inside it would count every
/// dependency's code a second time, as the root package's, and the totals twice. cargo
/// follows the link, so the build is the same.
fn copy(root: &Path) -> Result<PathBuf, String> {
    let to = root.join(COPY);
    match fs::remove_dir_all(&to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", to.display()));
        }
        _ => {}
    }
    let files = git(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
    for name in &files {
        // The results, which no build reads.
        if name.starts_with("audit") {
            continue;
        }
        let from = root.join(name);
        // Gone from the work tree, though git still has it.
        let Ok(meta) = fs::symlink_metadata(&from) else { continue };
        let made = parent(&to.join(name)).and_then(|()| {
            if meta.is_symlink() {
                symlink(fs::read_link(&from)?, to.join(name))
            } else if meta.is_file() {
                fs::copy(&from, to.join(name)).map(drop)
            } else {
                Ok(())
            }
        });
        made.map_err(|e| format!("{}: {e}", to.join(name).display()))?;
    }
    let ignored = git(
        root,
        &["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"],
    )?;
    for name in &ignored {
        // The builds, this copy among them.
        if name.starts_with("target") {
            continue;
        }
        let made = parent(&to.join(name)).and_then(|()| symlink(root.join(name), to.join(name)));
        made.map_err(|e| format!("{}: {e}", to.join(name).display()))?;
    }
    Ok(to)
}

/// Creates the directory `path` goes in.
fn parent(path: &Path) -> io::Result<()> {
    path.parent().map_or(Ok(()), fs::create_dir_all)
}

/// The paths `git args` lists, NUL-separated, without a trailing `/`.
fn git(root: &Path, args: &[&str]) -> Result<Vec<PathBuf>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), out.status));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| PathBuf::from(OsStr::from_bytes(name.strip_suffix(b"/").unwrap_or(name))))
        .collect())
}

/// Whether `table` has its legend and ends in the row of totals.
fn complete(table: &str) -> bool {
    let last = table.lines().rfind(|line| !line.trim().is_empty());
    table.contains("Metric output format")
        && last.is_some_and(|line| line.starts_with(|c: char| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::complete;

    #[test]
    fn a_table_is_whole_with_its_legend_and_totals() {
        assert!(complete("Metric output format: x/y\n\n0/0  0/0 :) a 1.0\n\n1/2  3/4\n\n"));
        assert!(!complete("Metric output format: x/y\n\nFunctions  Expressions\n"));
        assert!(!complete("1/2  3/4\n"));
        assert!(!complete(""));
    }
}
