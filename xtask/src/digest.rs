//! What inputs.txt holds of each file a task reads: a hash of what can change a run. Rust
//! files are hashed as their tokens, so that a change to their comments or layout changes
//! none; doc comments are tokens (`#[doc]` attributes), and stay, since derive macros such
//! as clap's and displaydoc's make the program's output of them. TOML files are hashed as
//! their values, so that a change to their comments, layout or key order changes none. The
//! rest, and a file that does not parse, are hashed as git hashes them.
//!
//! A file that code reads as data (with `include_str!`, or a test's sample) is hashed as it
//! is, whatever its kind.
//!
//! The version of the workspace's own packages is left out of each Cargo.toml and Cargo.lock:
//! a release changes it and nothing else, and no task finds anything else for it, unless the
//! code reads its version (`OWN_VERSION_READ`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// The first line of inputs.txt, which says how its hashes were made: with another first
/// line, none of them compare.
pub const FORMAT: &str = if OWN_VERSION_READ {
    "# Rust as tokens, TOML as values, the rest as git blobs (1)"
} else {
    "# Rust as tokens, TOML as values, the rest as git blobs, own versions left out (1)"
};

/// Per project: true when the code reads its own version (`env!("CARGO_PKG_VERSION")`,
/// clap's `version` attribute and the like), so that a new version can change what a task
/// finds; Cargo.toml and Cargo.lock are then hashed with it. Here true: `rse --version`
/// prints it (src/main.rs).
const OWN_VERSION_READ: bool = true;

/// The text that stands for a file's contents; none when they are hashed as they are.
fn normal(path: &str, bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let own = !OWN_VERSION_READ && is_cargo_file(path);
    let text = if own { Cow::Owned(without_own_versions(path, text)) } else { Cow::Borrowed(text) };
    match Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or_default() {
        // Outside a procedural macro, proc-macro2 reads the text itself, leaving out the
        // comments and the space between tokens.
        "rs" => text.parse::<proc_macro2::TokenStream>().ok().map(|t| t.to_string()),
        "toml" => toml::to_string(&sorted(toml::from_str(&text).ok()?)).ok(),
        _ if own => Some(text.into_owned()),
        _ => None,
    }
}

/// A TOML value with the keys of every table in order: with the `preserve_order` feature,
/// which any crate in the workspace may turn on, a table keeps them as they were written.
fn sorted(value: toml::Value) -> toml::Value {
    match value {
        toml::Value::Table(table) => {
            let keys: BTreeMap<String, toml::Value> =
                table.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            toml::Value::Table(keys.into_iter().collect())
        }
        toml::Value::Array(items) => toml::Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Whether `path` is a Cargo.toml or a Cargo.lock.
fn is_cargo_file(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|n| n == "Cargo.toml" || n == "Cargo.lock")
}

/// A Cargo.toml or Cargo.lock with the version of the workspace's own packages blanked: the
/// `version` of `[package]` and `[workspace.package]`, and of each `[[package]]` of the lock
/// without a `source`, which are the workspace's and path dependencies'. Read line by line, as
/// Cargo writes them, so that it needs no TOML parser; whatever else is kept as it is.
fn without_own_versions(path: &str, text: &str) -> String {
    let lock = path.ends_with("Cargo.lock");
    let mut out = String::with_capacity(text.len());
    let mut table: Vec<&str> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') && !table.is_empty() {
            push_table(&mut out, &table, lock);
            table.clear();
        }
        table.push(line);
    }
    push_table(&mut out, &table, lock);
    out
}

/// Adds a table's lines to `out`, its version blanked if it is one of the workspace's own.
fn push_table(out: &mut String, lines: &[&str], lock: bool) {
    let key = |line: &str, name: &str| {
        line.trim_start().strip_prefix(name).is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let header = lines.first().and_then(|l| l.split('#').next()).unwrap_or_default().trim();
    let own = if lock {
        header == "[[package]]" && !lines.iter().any(|l| key(l, "source"))
    } else {
        header == "[package]" || header == "[workspace.package]"
    };
    let mut blanked = false;
    for line in lines {
        if own && !blanked && key(line, "version") {
            out.push_str("version = \"\"\n");
            blanked = true;
        } else {
            out.push_str(line);
        }
    }
}

/// The files given with their git hashes (as `git ls-files -s` has them), with the hash of
/// what stands for each instead, as inputs.txt has them; those in `data` are hashed as they
/// are.
pub fn hashes(
    root: &Path,
    files: Vec<(String, String)>,
    data: &[&str],
) -> Result<Vec<(String, String)>, String> {
    let wanted: Vec<usize> = (0..files.len())
        .filter(|&i| !data.contains(&files[i].0.as_str()))
        .filter(|&i| {
            let path = Path::new(&files[i].0);
            path.extension().is_some_and(|e| e == "rs" || e == "toml") || is_cargo_file(&files[i].0)
        })
        .collect();
    let blobs = contents(root, wanted.iter().map(|&i| files[i].1.as_str()))?;
    let normals: Vec<(usize, String)> = wanted
        .into_iter()
        .zip(blobs)
        .filter_map(|(i, bytes)| Some((i, normal(&files[i].0, &bytes)?)))
        .collect();
    let new = hash(root, normals.iter().map(|(_, text)| text.as_str()))?;
    let mut files = files;
    for ((i, _), hash) in normals.into_iter().zip(new) {
        files[i].1 = hash;
    }
    Ok(files)
}

/// The contents of git's objects, in order.
fn contents<'a>(
    root: &Path,
    hashes: impl Iterator<Item = &'a str>,
) -> Result<Vec<Vec<u8>>, String> {
    let input: String = hashes.flat_map(|h| [h, "\n"]).collect();
    let out = piped(root, &["cat-file", "--batch"], input.into_bytes())?;
    // `<hash> blob <size>\n<contents>\n` for each.
    let mut rest = out.as_slice();
    let mut blobs = Vec::new();
    while let Some(end) = rest.iter().position(|&b| b == b'\n') {
        let header = String::from_utf8_lossy(&rest[..end]);
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("git cat-file: {header}"))?;
        let blob = rest.get(end + 1..end + 1 + size).ok_or("git cat-file: cut short")?;
        blobs.push(blob.to_vec());
        rest = rest.get(end + 2 + size..).unwrap_or_default();
    }
    Ok(blobs)
}

/// The git hashes of texts, in order.
fn hash<'a>(root: &Path, texts: impl Iterator<Item = &'a str>) -> Result<Vec<String>, String> {
    // Under target/, not the shared temporary directory, where another user could put a
    // link at these names beforehand.
    let dir = root.join("target").join("audit").join(format!("hashes-{}", std::process::id()));
    let written = (|| {
        fs::create_dir_all(&dir)?;
        let mut paths = String::new();
        for (i, text) in texts.enumerate() {
            let path = dir.join(i.to_string());
            fs::write(&path, text)?;
            paths.push_str(&path.to_string_lossy());
            paths.push('\n');
        }
        Ok::<_, std::io::Error>(paths)
    })();
    let out = written.map_err(|e| format!("{}: {e}", dir.display())).and_then(|paths| {
        piped(root, &["hash-object", "--no-filters", "--stdin-paths"], paths.into_bytes())
    });
    let _ = fs::remove_dir_all(&dir);
    Ok(String::from_utf8_lossy(&out?).lines().map(str::to_owned).collect())
}

/// What `git <args>` prints, given `input`.
fn piped(root: &Path, args: &[&str], input: Vec<u8>) -> Result<Vec<u8>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("git {}: {e}", args.join(" "));
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| failed(&e))?;
    let mut stdin = child.stdin.take().ok_or_else(|| failed(&"no input"))?;
    // Written beside the reading, so that neither pipe fills while the other waits.
    let writer = thread::spawn(move || stdin.write_all(&input));
    let mut out = Vec::new();
    let read = child.stdout.take().map(|mut s| s.read_to_end(&mut out));
    let status = child.wait().map_err(|e| failed(&e))?;
    let wrote = writer.join().map_err(|_| failed(&"the writer panicked"))?;
    match (read, wrote) {
        (Some(Ok(_)), Ok(())) if status.success() => Ok(out),
        _ => Err(failed(&status)),
    }
}

#[cfg(test)]
mod tests {
    use super::{OWN_VERSION_READ, normal, without_own_versions};

    fn same(path: &str, a: &str, b: &str) -> bool {
        let (a, b) = (normal(path, a.as_bytes()), normal(path, b.as_bytes()));
        assert!(a.is_some() && b.is_some(), "{path}: {a:?} {b:?}");
        a == b
    }

    #[test]
    fn rust_is_its_tokens() {
        assert!(same(
            "a.rs",
            "fn a() -> u8 { 1 } // one",
            "fn a()\n    -> u8 {\n    /* one */ 1\n}\n"
        ));
        assert!(!same("a.rs", "fn a() -> u8 { 1 }", "fn a() -> u8 { 2 }"));
        assert!(!same("a.rs", "/// One.\nfn a() {}", "/// Two.\nfn a() {}"));
        assert!(!same("a.rs", "//! One.\nfn a() {}", "//! Two.\nfn a() {}"));
        // What looks like a comment, or space, inside a literal is part of it.
        assert!(!same("a.rs", r#"const S: &str = "// a";"#, r#"const S: &str = "// b";"#));
        assert!(!same("a.rs", r#"const S: &str = "a b";"#, r#"const S: &str = "a  b";"#));
        assert!(!same(
            "a.rs",
            "const S: &str = r#\"/* a */\"#;",
            "const S: &str = r#\"/* b */\"#;"
        ));
        assert!(!same(
            "a.rs",
            "fn a<'a>(_: &'a u8) -> char { '/' }",
            "fn a<'a>(_: &'a u8) -> char { '*' }"
        ));
        assert!(!same("a.rs", "a += b;", "a + = b;"));
        assert_eq!(normal("a.rs", b"fn a() { \"open"), None);
    }

    #[test]
    fn toml_is_its_values() {
        assert!(same("a.toml", "a = 1 # one\nb = \"x\"\n", "b = 'x'\n\n a = 1\n"));
        assert!(!same("a.toml", "a = 1", "a = 2"));
        assert!(!same("a.toml", "a = [1, 2]", "a = [2, 1]"));
        assert_eq!(normal("a.toml", b"a = "), None);
        assert_eq!(normal("a.md", b"a"), None);
    }

    #[test]
    fn a_release_changes_no_hash() {
        let manifest = |v: &str| {
            format!(
                "[package]\nname = \"a\"\nversion = \"{v}\"\n\n[dependencies]\nb = {{ version = \"1\" }}\nc = {{ path = \"c\" }}\n"
            )
        };
        let lock = |v: &str, b: &str| {
            format!(
                "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"{v}\"\ndependencies = [\n \"b\",\n]\n\n[[package]]\nname = \"b\"\nversion = \"{b}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
            )
        };
        let blank = |path: &str, text: &str| without_own_versions(path, text);
        assert_eq!(
            blank("Cargo.toml", &manifest("0.1.0")),
            blank("Cargo.toml", &manifest("0.2.0"))
        );
        assert_eq!(
            blank("Cargo.lock", &lock("0.1.0", "1.0.0")),
            blank("Cargo.lock", &lock("0.2.0", "1.0.0"))
        );
        // A dependency's version still counts, as does the lock's own format.
        assert_ne!(
            blank("Cargo.lock", &lock("0.1.0", "1.0.0")),
            blank("Cargo.lock", &lock("0.1.0", "1.0.1"))
        );
        assert!(blank("Cargo.lock", &lock("0.1.0", "1.0.0")).starts_with("version = 4\n"));
        assert!(blank("Cargo.toml", &manifest("0.1.0")).contains("b = { version = \"1\" }"));
        let workspace = "[workspace.package]\nversion = \"1.0.0\"\n";
        assert_eq!(blank("Cargo.toml", workspace), "[workspace.package]\nversion = \"\"\n");
        let inherited = "[package]\nversion.workspace = true\n";
        assert_eq!(blank("Cargo.toml", inherited), inherited);
        // Both reach normal, and the rest of the text stays: a Cargo.toml as TOML, a Cargo.lock
        // with its own versions left out, or as it is where the code reads its version.
        assert!(normal("Cargo.toml", manifest("0.2.0").as_bytes()).is_some());
        let lock_read = normal("fuzz/Cargo.lock", lock("0.2.0", "1.0.0").as_bytes());
        assert_eq!(lock_read.is_some(), !OWN_VERSION_READ);
        assert_eq!(normal("a.md", b"version = \"1\""), None);
    }
}
