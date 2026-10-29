//! What inputs.txt holds of each file a task reads: a hash of what can change a run. Rust
//! files are hashed as their tokens, so that a change to their comments or layout changes
//! none; doc comments are tokens (`#[doc]` attributes), and stay, since derive macros such
//! as clap's and displaydoc's make the program's output of them. TOML files are hashed as
//! their values, so that a change to their comments, layout or key order changes none. The
//! rest, and a file that does not parse, are hashed as git hashes them.
//!
//! A file that code reads as data (with `include_str!`, or a test's sample) is hashed as it
//! is, whatever its kind.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// The first line of inputs.txt, which says how its hashes were made: with another first
/// line, none of them compare.
pub const FORMAT: &str = "# Rust as tokens, TOML as values, the rest as git blobs (1)";

/// The text that stands for a file's contents; none when they are hashed as they are.
fn normal(path: &str, bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    match Path::new(path).extension()?.to_str()? {
        // Outside a procedural macro, proc-macro2 reads the text itself, leaving out the
        // comments and the space between tokens.
        "rs" => text.parse::<proc_macro2::TokenStream>().ok().map(|t| t.to_string()),
        "toml" => toml::to_string(&sorted(toml::from_str(text).ok()?)).ok(),
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
        .filter(|&i| Path::new(&files[i].0).extension().is_some_and(|e| e == "rs" || e == "toml"))
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
    let dir = std::env::temp_dir().join(format!("xtask-hashes-{}", std::process::id()));
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
    use super::normal;

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
}
