use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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
    let members = members(root)?;
    let copy = copy(root)?;
    for (package, manifest) in &members {
        // A table for each member when there are several.
        let name = match members.len() {
            1 => "geiger.txt".to_owned(),
            _ => format!("geiger-{package}.txt"),
        };
        scan(root, &copy, dir, &name, package, manifest)?;
    }
    Ok(())
}

/// The workspace's default members, each scanned on its own: the root package where there
/// is one, the members a virtual manifest lists otherwise, which cargo-geiger 0.14.0 does
/// not take (nor `-p`). Each one's package name, and its manifest relative to the root;
/// xtask, which is not shipped, is no default member.
fn members(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let out = cargo(root, &["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata: {}", out.status));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let ids: Vec<&str> = meta["workspace_default_members"]
        .as_array()
        .ok_or("cargo metadata: no default members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let base = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut members = Vec::new();
    for package in meta["packages"].as_array().ok_or("cargo metadata: no packages")? {
        let field = |k: &str| package[k].as_str().ok_or(format!("cargo metadata: a package's {k}"));
        if !ids.contains(&field("id")?) {
            continue;
        }
        let manifest = Path::new(field("manifest_path")?);
        let relative = manifest.strip_prefix(&base).map_err(|_| {
            format!("cargo metadata: {} is outside the workspace", manifest.display())
        })?;
        members.push((field("name")?.to_owned(), relative.to_path_buf()));
    }
    if members.is_empty() {
        return Err("cargo metadata: no default member to scan".to_owned());
    }
    members.sort();
    Ok(members)
}

/// Scans `package` of the copy, whose manifest is `manifest`, into the table `name` in `dir`.
fn scan(
    root: &Path,
    copy: &Path,
    dir: &Path,
    name: &str,
    package: &str,
    manifest: &Path,
) -> Result<(), String> {
    let path = dir.join(name);
    let file = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // geiger cleans before it builds; in target/debug that would take everyone's build.
    let mut scan = cargo(copy, &["geiger", "--locked", "--offline", "--output-format", "Ascii"]);
    scan.arg("--manifest-path").arg(copy.join(manifest));
    scan.env("CARGO_TARGET_DIR", root.join("target/geiger"));
    // It colours the table as cargo's own output is coloured, and CI sets that to always.
    scan.env("CARGO_TERM_COLOR", "never");
    let mut cmd = capped(scan, &limit("geiger"));
    record::say(&format!("$ {} > {name}", shown(&cmd)));
    let ended = record::exec(&mut cmd, Some(file)).map_err(|e| format!("cargo geiger: {e}"))?;
    let table = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // geiger exits non-zero when files it was shown are not Rust (READMEs, included
    // text), so the table itself tells whether it ran; a signal says it did not finish.
    if ended.code().is_none() {
        return Err(format!("cargo geiger did not finish on {package}: {ended}"));
    }
    if !complete(&table) {
        return Err(format!("cargo geiger wrote no whole table of {package}: {ended}"));
    }
    if !ended.success() {
        record::say(&format!("cargo geiger: {ended}, with a whole table of {package}"));
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
    let mut linked: Vec<&PathBuf> = Vec::new();
    for name in &ignored {
        // The builds, this copy among them.
        if name.starts_with("target") {
            continue;
        }
        // git 2.43 lists a directory with a .gitignore of its own (pytest's `*`), then its
        // files as well: they are there through the directory's link, and a link made for
        // one of them would go through it, into the work tree.
        if linked.iter().any(|dir| name.starts_with(dir)) {
            continue;
        }
        let made = parent(&to.join(name)).and_then(|()| symlink(root.join(name), to.join(name)));
        made.map_err(|e| format!("{}: {e}", to.join(name).display()))?;
        linked.push(name);
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
