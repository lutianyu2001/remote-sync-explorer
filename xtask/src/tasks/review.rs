//! `review`: that the code a reviewer has to read, because no tool can check it,
//! was read as it is now. That is the Rust with `unsafe` in it, and any C, C++ or assembly.
//! Reviews are written by hand in `audit/reviews/<name>.md`, each beginning with a header that
//! names the files it covers with their sha256:
//!
//! ```text
//! # Review of the lookup in the password database
//!
//! - file: src/ffi.rs sha256:<hex>
//! - reviewer: <who>
//! - date: <YYYY-MM-DD>
//! - commit: <the commit reviewed>
//!
//! <the findings>
//! ```
//!
//! A file is covered by a review whose sha256 for it is the file's own now, so any change to
//! the file, its comments included, wants it read again. `cargo xtask review-hash <file>...`
//! prints the header lines.

use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where the reviews are, beside the results of the tasks; never a task's own directory.
pub const REVIEWS: &str = "audit/reviews";

// Per project: files whose `unsafe` is only a word in their comments.
/// Rust files left out although `unsafe` is a word in them, while it is only in comments (or
/// strings): read as tokens, where `unsafe` or `global_asm` brings them back. Here the
/// message key `names-unsafe` (src/app.rs) and a comment of src/main.rs.
const ONLY_IN_COMMENTS: &[&str] = &["src/app.rs"];

/// What is never reviewed here: the dependencies' code, which their audits cover (see
/// supply-chain/), the fuzz targets and the audit's own code.
const OUTSIDE: &[&str] = &["vendor", "fuzz", "xtask"];

/// The extensions of C, C++ and assembly, which always want a review.
const FOREIGN: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp", "s", "S",
    "asm",
];

fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|p| p.starts_with('/'))
}

/// Whether `path` is a file that can want a review: Rust, which may have `unsafe` in it, or
/// C, C++ or assembly, outside what is never reviewed here.
pub fn can_need(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or_default();
    !OUTSIDE.iter().any(|d| under(path, d))
        && !under(path, "audit")
        && (ext == "rs" || FOREIGN.contains(&ext))
}

/// Whether Rust `text` has `unsafe` or `global_asm` in its code, not in its comments or
/// strings; text that is not Rust's tokens is taken to have it.
fn unsafe_in_code(text: &str) -> bool {
    fn any(stream: proc_macro2::TokenStream) -> bool {
        stream.into_iter().any(|tree| match tree {
            proc_macro2::TokenTree::Ident(ident) => ident == "unsafe" || ident == "global_asm",
            proc_macro2::TokenTree::Group(group) => any(group.stream()),
            _ => false,
        })
    }
    text.parse::<proc_macro2::TokenStream>().map_or(true, any)
}

/// The files that want a review now: tracked Rust files with the word `unsafe` in them, or
/// `global_asm!`, which needs no `unsafe` block, and tracked C, C++ and assembly.
fn wanted(root: &Path) -> Result<Vec<String>, String> {
    let git = |args: &[&str]| -> Result<(bool, String), String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        // git grep exits 1 when nothing matches.
        match out.status.code() {
            Some(0 | 1) => {
                Ok((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned()))
            }
            _ => Err(format!(
                "git {}: {}: {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    };
    let (_, unsafe_rust) = git(&["grep", "-lzwE", "unsafe|global_asm", "--", "*.rs"])?;
    let (listed, tracked) = git(&["ls-files", "-z"])?;
    if !listed {
        return Err("git ls-files failed".to_owned());
    }
    let mut files: Vec<String> = unsafe_rust
        .split('\0')
        .chain(tracked.split('\0').filter(|p| {
            Path::new(p).extension().and_then(|e| e.to_str()).is_some_and(|e| FOREIGN.contains(&e))
        }))
        .filter(|p| !p.is_empty() && can_need(p) && root.join(p).is_file())
        .filter(|p| {
            !ONLY_IN_COMMENTS.contains(p)
                || fs::read_to_string(root.join(p)).map_or(true, |text| unsafe_in_code(&text))
        })
        .map(str::to_owned)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// A review's header.
#[derive(Debug, PartialEq, Eq)]
struct Review {
    /// The files it covers, with the sha256 it records for each.
    files: Vec<(String, String)>,
}

/// The header of a review: after a title and blank lines, the lines `- <key>: <value>` up to
/// the first that is not one. It names at least one file, and its reviewer, date and commit.
fn header(text: &str) -> Result<Review, String> {
    let lines = text
        .lines()
        .map(str::trim_end)
        .skip_while(|l| l.is_empty() || l.starts_with('#'))
        .take_while(|l| l.starts_with("- "));
    let mut files = Vec::new();
    let mut keys = Vec::new();
    for line in lines {
        let Some((key, value)) = line[2..].split_once(':') else {
            return Err(format!("not a `- <key>: <value>` line: {line}"));
        };
        let (key, value) = (key.trim(), value.trim());
        if key == "file" {
            let entry = value
                .rsplit_once(" sha256:")
                .map(|(path, hash)| (path.trim(), hash.trim()))
                .filter(|(path, hash)| {
                    !path.is_empty()
                        && hash.len() == 64
                        && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                });
            let Some((path, hash)) = entry else {
                return Err(format!(
                    "not `- file: <path> sha256:<64 lowercase hex digits>`: {line}"
                ));
            };
            files.push((path.to_owned(), hash.to_owned()));
        } else if !value.is_empty() {
            keys.push(key.to_owned());
        }
    }
    if files.is_empty() {
        return Err("its header names no file (`- file: <path> sha256:<hex>`)".to_owned());
    }
    let missing: Vec<&str> = ["reviewer", "date", "commit"]
        .into_iter()
        .filter(|k| !keys.iter().any(|h| h == k))
        .collect();
    if !missing.is_empty() {
        return Err(format!("its header has no {}", missing.join(", no ")));
    }
    Ok(Review { files })
}

/// The reviews read: each one's path, and its header or why it could not be read.
type Reviews = Vec<(String, Result<Review, String>)>;

/// The reviews in audit/reviews/.
fn reviews(root: &Path) -> Result<Reviews, String> {
    let dir = root.join(REVIEWS);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{REVIEWS}: {e}")),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{REVIEWS}: {e}"))?.path();
        if path.extension().is_none_or(|e| e != "md") || !path.is_file() {
            continue;
        }
        let name = format!("{REVIEWS}/{}", path.file_name().unwrap_or_default().to_string_lossy());
        let review = fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| header(&t));
        found.push((name, review));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// What a file that wants a review has of one.
#[derive(Debug, PartialEq, Eq)]
enum State {
    /// Covered by this review.
    Reviewed(String),
    /// Reviews of it as it was, none as it is.
    Stale(Vec<String>),
    None,
}

fn state(path: &str, hash: &str, reviews: &Reviews) -> State {
    let mut stale = Vec::new();
    for (name, review) in reviews {
        let Ok(review) = review else { continue };
        for (file, recorded) in &review.files {
            if file == path && recorded == hash {
                return State::Reviewed(name.clone());
            }
            if file == path {
                stale.push(name.clone());
            }
        }
    }
    if stale.is_empty() { State::None } else { State::Stale(stale) }
}

/// The line of a review's header for `path`, a file of the repository.
fn line(path: &str, hash: &str) -> String {
    format!("- file: {path} sha256:{hash}")
}

fn sha256_of(root: &Path, path: &str) -> Result<String, String> {
    fs::read(root.join(path)).map(|b| hex(&sha256(&b))).map_err(|e| format!("{path}: {e}"))
}

/// Runs the check, writing what each file has of a review into `dir/review.txt`.
pub fn run(root: &Path, dir: &Path) -> Result<(), String> {
    let wanted = wanted(root)?;
    let reviews = reviews(root)?;
    let mut report = String::new();
    let mut problems = Vec::new();
    let mut lines = Vec::new();
    for path in &wanted {
        let hash = sha256_of(root, path)?;
        let shown = match state(path, &hash, &reviews) {
            State::Reviewed(by) => format!("reviewed in {by}"),
            State::Stale(by) => {
                lines.push(line(path, &hash));
                let shown = format!("changed since its review in {}", by.join(", "));
                problems.push(format!("{path}: {shown}"));
                shown
            }
            State::None => {
                lines.push(line(path, &hash));
                problems.push(format!("{path}: no review"));
                "no review".to_owned()
            }
        };
        let _ = writeln!(report, "{path}\tsha256:{hash}\t{shown}");
    }
    for (name, review) in &reviews {
        match review {
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                let _ = writeln!(report, "{name}\tnot read: {e}");
            }
            Ok(review) => {
                for (file, _) in &review.files {
                    if !wanted.contains(file) {
                        let _ = writeln!(report, "{name}\t{file} wants no review now");
                    }
                }
            }
        }
    }
    if wanted.is_empty() {
        report.push_str("no file wants a review\n");
    }
    let path = dir.join("review.txt");
    fs::write(&path, &report).map_err(|e| format!("{}: {e}", path.display()))?;
    if problems.is_empty() {
        crate::record::say(&format!(
            "each file that wants a review is reviewed as it is ({})",
            wanted.len()
        ));
        return Ok(());
    }
    let mut said = problems.join("\n");
    if !lines.is_empty() {
        let _ = write!(
            said,
            "\n\nOnce the code is reviewed, put these lines in the header of a review in \
             {REVIEWS}/ (a new one, {REVIEWS}/<name>.md, also has `- reviewer: `, `- date: ` \
             and `- commit: ` lines, then the findings):\n{}",
            lines.join("\n")
        );
    }
    crate::record::say(&said);
    let count = match problems.len() {
        1 => "a problem".to_owned(),
        n => format!("{n} problems"),
    };
    Err(format!("{count} with the reviews (see above)"))
}

/// `cargo xtask review-hash <file>...`: the header line of each file, given from `caller`
/// (the directory cargo xtask was started in).
pub fn hash_lines(root: &Path, caller: &Path, files: &[String]) -> ExitCode {
    if files.is_empty() {
        eprintln!("xtask: usage: cargo xtask review-hash <file>...");
        return ExitCode::FAILURE;
    }
    let mut ok = true;
    for file in files {
        match relative(root, &caller.join(file)).and_then(|p| Ok((sha256_of(root, &p)?, p))) {
            Ok((hash, path)) => println!("{}", line(&path, &hash)),
            Err(e) => {
                eprintln!("xtask: review-hash: {e}");
                ok = false;
            }
        }
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// `path` relative to the repository, with `/` between its parts.
fn relative(root: &Path, path: &Path) -> Result<String, String> {
    let full = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let inside: PathBuf = full
        .strip_prefix(&base)
        .map_err(|_| format!("{}: not in the repository", path.display()))?
        .to_owned();
    if !full.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    let parts: Vec<String> = inside
        .components()
        .map(|c| match c {
            Component::Normal(p) => Ok(p.to_string_lossy().into_owned()),
            _ => Err(format!("{}: not a path in the repository", path.display())),
        })
        .collect::<Result<_, _>>()?;
    Ok(parts.join("/"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256's round constants (FIPS 180-4, 4.2.2).
#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4,
    0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe,
    0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f,
    0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da, 0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
    0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc,
    0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
    0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070, 0x19a4_c116,
    0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256 (FIPS 180-4), which no dependency of the project's provides.
fn sha256(data: &[u8]) -> [u8; 32] {
    #[rustfmt::skip]
    let mut state: [u32; 8] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
        0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        compress(&mut state, block);
    }
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    out
}

/// One block of SHA-256 into its state.
#[allow(clippy::many_single_char_names)] // FIPS 180-4's names
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, w) in K.iter().zip(w) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(w);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (x, y) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *x = x.wrapping_add(y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_the_standard_one() {
        let of = |data: &[u8]| hex(&sha256(data));
        assert_eq!(of(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(of(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Lengths about a block's end, where the padding takes another block.
        assert_eq!(
            of(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            of(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn a_header_names_files_and_who_reviewed_which_commit() {
        let text = format!(
            "# Review\n\n- file: a/b.rs sha256:{HASH}\n- file: c d.c sha256:{HASH}\n\
             - reviewer: someone\n- date: 2026-11-08\n- commit: abc\n\n- file: e.rs sha256:{HASH}\n"
        );
        let files =
            vec![("a/b.rs".to_owned(), HASH.to_owned()), ("c d.c".to_owned(), HASH.to_owned())];
        assert_eq!(header(&text), Ok(Review { files }));
        let without = |key: &str| {
            text.lines()
                .filter(|l| !l.starts_with(&format!("- {key}:")))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(header(&without("reviewer")).is_err());
        assert!(header(&without("commit")).is_err());
        assert!(header(&without("file")).is_err());
        assert!(header(&text.replace(HASH, "abc")).is_err());
        assert!(header(&text.replace(HASH, &HASH.to_uppercase())).is_err());
        assert!(header("").is_err());
    }

    #[test]
    fn a_file_is_reviewed_only_as_it_is() {
        let other = "0".repeat(64);
        let review = |files: &[(&str, &str)]| {
            Ok(Review {
                files: files.iter().map(|(p, h)| ((*p).to_owned(), (*h).to_owned())).collect(),
            })
        };
        let reviews = vec![
            ("old.md".to_owned(), review(&[("a.rs", &other)])),
            ("bad.md".to_owned(), Err("no header".to_owned())),
            ("new.md".to_owned(), review(&[("b.rs", HASH), ("a.rs", HASH)])),
        ];
        assert_eq!(state("a.rs", HASH, &reviews), State::Reviewed("new.md".to_owned()));
        assert_eq!(
            state("a.rs", "1".repeat(64).as_str(), &reviews),
            State::Stale(vec!["old.md".to_owned(), "new.md".to_owned()])
        );
        assert_eq!(state("c.rs", HASH, &reviews), State::None);
    }

    #[test]
    fn unsafe_in_comments_and_strings_is_no_unsafe() {
        let comments = "//! Code that calls `unsafe` C.\n#![deny(unsafe_code)]\n/// unsafe\nfn a() { \"unsafe\"; }\n";
        assert!(!unsafe_in_code(comments));
        assert!(unsafe_in_code("fn a() { unsafe { b() } }"));
        assert!(unsafe_in_code("core::arch::global_asm!(\"nop\");"));
        assert!(unsafe_in_code("fn a() { \"unclosed }"));
    }

    #[test]
    fn what_can_want_a_review() {
        for path in [
            "crates/a/src/lib.rs",
            "src/x.c",
            "include/x.h",
            "a.cpp",
            "a.cxx",
            "include/a.hh",
            "include/a.ipp",
            "b.hpp",
            "c.cc",
            "d.s",
            "e.S",
            "f.asm",
        ] {
            assert!(can_need(path), "{path}");
        }
        for path in [
            "vendor/libc/src/lib.rs",
            "fuzz/fuzz_targets/a.rs",
            "xtask/src/main.rs",
            "audit/reviews/a.md",
            "README.md",
            "crates/a/Cargo.toml",
            "a.py",
        ] {
            assert!(!can_need(path), "{path}");
        }
        // Still review's inputs, so that `unsafe` coming into their code is caught.
        for path in ONLY_IN_COMMENTS {
            assert!(can_need(path), "{path}");
        }
    }

    /// The reviews are never where a task's results go, which a run replaces whole.
    #[test]
    fn the_reviews_are_no_tasks_results() {
        for task in super::super::KNOWN {
            let results = format!("audit/{task}");
            assert!(results != REVIEWS && !REVIEWS.starts_with(&format!("{results}/")), "{task}");
        }
    }
}
