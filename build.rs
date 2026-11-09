//! Builds librclone as a static C archive from the Go module in `librse/`: rclone at a
//! pinned version, with rse's own rc calls added (see `librse/main.go`).
//!
//! Environment:
//! - `RSE_RCLONE_VERSION` overrides the pinned rclone version: Go fetches it, in a copy
//!   of `librse/go.mod` in `OUT_DIR`.
//! - `RSE_LIBRCLONE_DIR` points at a directory that already holds `librclone.a` and
//!   `librclone.h`; the Go build is skipped entirely.
//! - `GO` names the Go binary (default `go` from `PATH`).
//! - `CC_<target>` or `TARGET_CC` names the C compiler cgo uses for the target. Cross
//!   builds default to `clang -arch <arch>` for macOS and `<arch>-linux-gnu-gcc` for
//!   Linux (`<arch>-linux-musl-gcc` for musl); native builds keep Go's default.
//!
//! `GOOS` and `GOARCH` always follow cargo's target, so `cargo build --target ...`
//! cross-compiles librclone too. For macOS, `MACOSX_DEPLOYMENT_TARGET` (set in
//! `.cargo/config.toml`) is passed to cgo's clang as `-mmacosx-version-min`.

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};

/// The rclone version this release of remote-sync-explorer is built and tested against.
/// `librse/go.mod` requires the same.
const RCLONE_VERSION: &str = "v1.75.2";

/// Bump when the go build flags change, so cached libraries are rebuilt.
const STAMP_REV: u32 = 4;

/// The Go module librclone is built from, in the package.
const LIBRSE: &str = "librse";

/// The declarations `src/rclone/ffi.rs` writes by hand. `librclone.h` must contain
/// exactly these, otherwise the build fails.
const EXPECTED_DECLS: [&str; 4] = [
    "extern void RcloneInitialize(void);",
    "extern void RcloneFinalize(void);",
    "extern struct RcloneRPCResult RcloneRPC(char* method, char* input);",
    "extern void RcloneFreeString(char* str);",
];
const EXPECTED_STRUCT: [&str; 2] = ["char* Output;", "int Status;"];
/// The declaration `src/i18n.rs` writes by hand for glibc targets, as libintl.h has it.
const EXPECTED_LIBINTL: &str =
    "extern char *bind_textdomain_codeset (const char *__domainname, const char *__codeset)";
/// The same declarations as Rust has them, in each file that writes them: each C one above
/// is checked against its header, and these against the files, so that neither side can
/// change alone. They are compared as tokens with the attributes before them, comments and
/// doc comments aside, and each file must have exactly these `extern` blocks, of any ABI,
/// and these structs of the names given: a copy in disabled code, or a block of its own,
/// fails the build too; one in a comment or a string is no declaration.
const RUST_DECLS: [(&str, &[&str]); 2] = [
    (
        "src/rclone/ffi.rs",
        &[
            "#[repr(C)] struct RpcReturn { output: *mut c_char, status: c_int, }",
            r#"#[allow(unsafe_code)]
            unsafe extern "C" {
                fn RcloneInitialize();
                fn RcloneFinalize();
                fn RcloneRPC(method: *mut c_char, input: *mut c_char) -> RpcReturn;
                fn RcloneFreeString(s: *mut c_char);
            }"#,
        ],
    ),
    (
        "src/i18n.rs",
        &[r#"unsafe extern "C" {
            fn bind_textdomain_codeset(
                domain: *const libc::c_char,
                codeset: *const libc::c_char,
            ) -> *mut libc::c_char;
        }"#],
    ),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    check_rust_decls();
    third_party_licenses();
    // A directory: cargo looks at every file in it.
    println!("cargo:rerun-if-changed={LIBRSE}");
    println!("cargo:rerun-if-env-changed=RSE_RCLONE_VERSION");
    println!("cargo:rerun-if-env-changed=RSE_LIBRCLONE_DIR");
    println!("cargo:rerun-if-env-changed=GO");
    println!("cargo:rerun-if-env-changed=CGO_CFLAGS");

    let version = env::var("RSE_RCLONE_VERSION").unwrap_or_else(|_| RCLONE_VERSION.to_owned());
    println!("cargo:rustc-env=RSE_RCLONE_VERSION={version}");

    if env_or_panic("CARGO_CFG_TARGET_OS") == "linux"
        && env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "gnu")
    {
        check_libintl(target_cc("linux", "").as_deref().unwrap_or("cc"));
    }

    if env::var_os("CARGO_FEATURE_EMBEDDED").is_none() {
        return;
    }

    let lib_dir = if let Some(dir) = env::var_os("RSE_LIBRCLONE_DIR") {
        let dir = PathBuf::from(dir);
        println!("cargo:rerun-if-changed={}", dir.join("librclone.a").display());
        println!("cargo:rerun-if-changed={}", dir.join("librclone.h").display());
        dir
    } else {
        build_librclone(&version, &GoTarget::from_cargo())
    };

    check_header(&lib_dir.join("librclone.h"));

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=rclone");
    emit_platform_link_args();
}

/// Go's names for cargo's target, and the C compiler cgo must use for it.
struct GoTarget {
    goos: &'static str,
    goarch: &'static str,
    /// `None` keeps Go's default compiler.
    cc: Option<String>,
    /// `MACOSX_DEPLOYMENT_TARGET` for a macOS target, when set.
    macos_min: Option<String>,
}

impl GoTarget {
    fn from_cargo() -> Self {
        let os = env_or_panic("CARGO_CFG_TARGET_OS");
        let arch = env_or_panic("CARGO_CFG_TARGET_ARCH");
        let goos = match os.as_str() {
            "linux" => "linux",
            "macos" => "darwin",
            other => panic!("remote-sync-explorer does not support target OS `{other}`"),
        };
        let goarch = match arch.as_str() {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => panic!(
                "no Go architecture known for `{other}`; build librclone yourself and set \
                 RSE_LIBRCLONE_DIR"
            ),
        };
        let macos_min = if goos == "darwin" {
            println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
            let min = env::var("MACOSX_DEPLOYMENT_TARGET").ok();
            if min.is_none() {
                println!(
                    "cargo:warning=MACOSX_DEPLOYMENT_TARGET is not set (see .cargo/config.toml); \
                     cgo builds librclone for the host's macOS version"
                );
            }
            min
        } else {
            None
        };
        Self { goos, goarch, cc: target_cc(goos, goarch), macos_min }
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("GOOS", self.goos).env("GOARCH", self.goarch);
        if let Some(cc) = &self.cc {
            cmd.env("CC", cc);
        }
        if let Some(min) = &self.macos_min {
            // A flag, not just the variable: Go's build cache keys on CGO_CFLAGS but not on
            // MACOSX_DEPLOYMENT_TARGET, and would reuse objects built for another version.
            let base = env::var("CGO_CFLAGS").unwrap_or_else(|_| "-O2 -g".to_owned());
            cmd.env("CGO_CFLAGS", format!("{base} -mmacosx-version-min={min}"));
        }
    }
}

/// The C compiler for cgo: `CC_<target>` or `TARGET_CC` as the cc crate reads them, else
/// for a cross build the usual cross compiler. Go splits `CC` on spaces, so it may carry
/// flags.
fn target_cc(goos: &str, goarch: &str) -> Option<String> {
    let target = env_or_panic("TARGET");
    for name in
        [format!("CC_{target}"), format!("CC_{}", target.replace('-', "_")), "TARGET_CC".into()]
    {
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(cc) = env::var(&name) {
            return Some(cc);
        }
    }
    if env_or_panic("HOST") == target {
        return None;
    }
    Some(match goos {
        // Apple's clang builds for every macOS architecture.
        "darwin" => format!("clang -arch {}", if goarch == "amd64" { "x86_64" } else { goarch }),
        _ => format!(
            "{}-linux-{}-gcc",
            env_or_panic("CARGO_CFG_TARGET_ARCH"),
            if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") { "musl" } else { "gnu" }
        ),
    })
}

/// Builds librclone into `OUT_DIR/librclone-<version>`, reusing an earlier build of the
/// same version and sources for the same target.
fn build_librclone(version: &str, target: &GoTarget) -> PathBuf {
    let out_dir = PathBuf::from(env_or_panic("OUT_DIR"));
    let src = PathBuf::from(env_or_panic("CARGO_MANIFEST_DIR")).join(LIBRSE);
    let pinned = go_mod_rclone(&src);
    assert!(
        pinned == RCLONE_VERSION,
        "{LIBRSE}/go.mod requires rclone {pinned}, build.rs pins {RCLONE_VERSION}"
    );
    let dir = out_dir.join(format!("librclone-{version}"));
    let stamp = dir.join("stamp");
    let stamp_text = format!(
        "{version} {STAMP_REV} {}/{} cc={} macos={} src={:016x}",
        target.goos,
        target.goarch,
        target.cc.as_deref().unwrap_or("default"),
        target.macos_min.as_deref().unwrap_or("-"),
        digest_dir(&src)
    );
    if fs::read_to_string(&stamp).is_ok_and(|s| s == stamp_text)
        && dir.join("librclone.a").is_file()
        && dir.join("librclone.h").is_file()
    {
        return dir;
    }

    if dir.exists() {
        fs::remove_dir_all(&dir)
            .unwrap_or_else(|e| panic!("cannot remove stale {}: {e}", dir.display()));
    }
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));

    let go = env::var("GO").unwrap_or_else(|_| "go".to_owned());
    // The pinned version builds from librse/go.sum as it is; another one from a copy,
    // so that the package itself is never written to.
    let mut module_flags = vec!["-mod=readonly".to_owned()];
    if version != RCLONE_VERSION {
        let modfile = dir.join("go.mod");
        for name in ["go.mod", "go.sum"] {
            fs::copy(src.join(name), dir.join(name))
                .unwrap_or_else(|e| panic!("cannot copy {LIBRSE}/{name}: {e}"));
        }
        let modfile_flag = format!("-modfile={}", modfile.display());
        let module = format!("github.com/rclone/rclone@{version}");
        run(&src, target, &go, &["get", &modfile_flag, &module]);
        module_flags = vec![modfile_flag, "-mod=mod".to_owned()];
    }
    let output = dir.join("librclone.a");
    let output = output.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", dir.display()));
    let mut args = vec!["build"];
    args.extend(module_flags.iter().map(String::as_str));
    args.extend([
        "-buildmode=c-archive",
        "-trimpath",
        // A release build, like rclone's own: report v1.75.2, not v1.75.2-DEV.
        "-ldflags=-X github.com/rclone/rclone/fs.VersionSuffix=",
        "-o",
        output,
        ".",
    ]);
    run(&src, target, &go, &args);

    fs::write(&stamp, &stamp_text)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", stamp.display()));
    dir
}

/// The rclone version `go.mod` in `dir` requires.
fn go_mod_rclone(dir: &Path) -> String {
    let path = dir.join("go.mod");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    text.lines()
        .map(|l| l.trim().trim_start_matches("require").split_whitespace().collect::<Vec<_>>())
        .find_map(|w| match w.as_slice() {
            ["github.com/rclone/rclone", v, ..] => Some((*v).to_owned()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} does not require github.com/rclone/rclone", path.display()))
}

/// A digest of the names and contents of the files under `dir`, so that a change to the
/// Go sources rebuilds the library. Stable within one build of the toolchain, which is
/// all a cache in `OUT_DIR` needs.
fn digest_dir(dir: &Path) -> u64 {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.sort();
    let mut hasher = DefaultHasher::new();
    for file in files {
        file.strip_prefix(dir).unwrap_or(&file).hash(&mut hasher);
        fs::read(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()))
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Runs one command and panics with the full command line and stderr if it fails.
fn run(dir: &Path, target: &GoTarget, program: &str, args: &[&str]) {
    let command_line = format!("{program} {}", args.join(" "));
    let mut cmd = Command::new(program);
    // Never a go.work from a directory above: the module builds as it is.
    cmd.args(args).current_dir(dir).env("CGO_ENABLED", "1").env("GOWORK", "off");
    target.apply(&mut cmd);
    if env::var_os("GOTOOLCHAIN").is_none() {
        // Fail with a clear message instead of silently downloading another toolchain.
        cmd.env("GOTOOLCHAIN", "local");
    }
    let output = cmd.output().unwrap_or_else(|e| {
        panic!(
            "failed to run `{command_line}` in {}: {e}\n\
             librclone needs Go 1.26 or newer (see README); set GO to the go binary, or \
             RSE_LIBRCLONE_DIR to a prebuilt librclone",
            dir.display()
        )
    });
    assert!(
        output.status.success(),
        "`{command_line}` in {} failed with {}\n--- stderr ---\n{}",
        dir.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Checks that the generated header declares exactly the functions and struct layout
/// that `src/rclone/ffi.rs` assumes.
fn check_header(path: &Path) {
    let header =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let lines: Vec<String> = header.lines().map(normalize).collect();

    let decls: Vec<String> = lines
        .iter()
        .filter(|l| l.starts_with("extern ") && l.contains("Rclone"))
        .cloned()
        .collect();
    let expected: Vec<String> = EXPECTED_DECLS.iter().map(|d| normalize(d)).collect();
    assert!(
        decls == expected,
        "{} does not match the hand-written FFI in src/rclone/ffi.rs\n\
         expected:\n  {}\nfound:\n  {}",
        path.display(),
        expected.join("\n  "),
        decls.join("\n  ")
    );

    let Some(start) = lines.iter().position(|l| l == "struct RcloneRPCResult {") else {
        panic!("{}: struct RcloneRPCResult not found", path.display());
    };
    let fields: Vec<String> = lines[start + 1..]
        .iter()
        .take_while(|l| !l.starts_with('}'))
        .filter(|l| !l.is_empty())
        .cloned()
        .collect();
    let expected: Vec<String> = EXPECTED_STRUCT.iter().map(|d| normalize(d)).collect();
    assert!(
        fields == expected,
        "{}: struct RcloneRPCResult changed; expected {{ {} }}, found {{ {} }}",
        path.display(),
        expected.join(" "),
        fields.join(" ")
    );
}

/// Checks the declaration `src/i18n.rs` writes by hand against libintl.h, as the target's
/// C compiler `cc` (which may carry flags) reads it.
fn check_libintl(cc: &str) {
    let mut words = cc.split_whitespace();
    let program = words.next().unwrap_or("cc");
    let mut child = Command::new(program)
        .args(words)
        .args(["-E", "-P", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {cc} to read libintl.h: {e}"));
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"#include <libintl.h>\n");
    }
    let out = child.wait_with_output().unwrap_or_else(|e| panic!("{cc}: {e}"));
    assert!(out.status.success(), "{cc} could not read libintl.h");
    // Spacing aside: the preprocessor joins the lines of a declaration its own way.
    let squeeze = |s: &str| s.split_whitespace().collect::<String>();
    assert!(
        squeeze(&String::from_utf8_lossy(&out.stdout)).contains(&squeeze(EXPECTED_LIBINTL)),
        "libintl.h does not match the hand-written FFI in src/i18n.rs: expected\n  \
         {EXPECTED_LIBINTL}"
    );
}

/// Checks that the files that declare C functions by hand still declare them as
/// [`RUST_DECLS`] has them, and nothing else in an `extern "C"` block.
fn check_rust_decls() {
    let tokens = |text: &str, what: &str| {
        text.parse::<TokenStream>().unwrap_or_else(|e| panic!("{what}: cannot read: {e}"))
    };
    for (file, decls) in RUST_DECLS {
        println!("cargo:rerun-if-changed={file}");
        let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
        let mut expected: Vec<String> =
            decls.iter().map(|d| render(&tokens(d, "build.rs"))).collect();
        let structs: Vec<String> = expected.iter().filter_map(|d| struct_name(d)).collect();
        let mut found = Vec::new();
        declarations(tokens(&text, file), &structs, &mut found);
        expected.sort();
        found.sort();
        assert!(
            found == expected,
            "{file} declares, outside comments:\n  {}\nbut build.rs expects:\n  {}\nChange both, \
             and the C declaration build.rs checks it against.",
            found.join("\n  "),
            expected.join("\n  ")
        );
    }
}

/// A declaration's tokens as text, without doc comments, spaced as proc-macro2 prints them.
fn render(tokens: &TokenStream) -> String {
    undocumented(tokens.clone()).to_string()
}

/// `tokens` without `#[doc = …]` attributes (what `///` comments are as tokens), at any depth.
fn undocumented(tokens: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut kept = Vec::new();
    let mut i = 0;
    while i < trees.len() {
        // `#[doc = …]`, or `#![doc = …]` for `//!`.
        let bang = matches!(trees.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!');
        let at = if bang { i + 2 } else { i + 1 };
        if let (TokenTree::Punct(p), Some(TokenTree::Group(g))) = (&trees[i], trees.get(at))
            && p.as_char() == '#'
            && g.delimiter() == Delimiter::Bracket
            && matches!(g.stream().into_iter().next(), Some(TokenTree::Ident(id)) if id == "doc")
        {
            i = at + 1;
            continue;
        }
        kept.push(match &trees[i] {
            TokenTree::Group(g) => {
                let mut inner = Group::new(g.delimiter(), undocumented(g.stream()));
                inner.set_span(g.span());
                TokenTree::Group(inner)
            }
            other => other.clone(),
        });
        i += 1;
    }
    kept.into_iter().collect()
}

/// The name of the struct `decl` declares, if it is one.
fn struct_name(decl: &str) -> Option<String> {
    let trees: Vec<TokenTree> = decl.parse::<TokenStream>().ok()?.into_iter().collect();
    trees.windows(2).find_map(|w| match w {
        [TokenTree::Ident(kw), TokenTree::Ident(name)] if kw == "struct" => Some(name.to_string()),
        _ => None,
    })
}

/// Adds to `found`, rendered, every `extern` block in `tokens`, whatever its ABI (with
/// `unsafe` and the attributes before it), and every struct named in `structs` (with its
/// visibility and the attributes before it), at any depth: in a function's body, a module,
/// or code a `cfg` leaves out, so that a copy there counts as one more. What the check
/// cannot see: what a name resolves to (a `use … as` renaming a type), and what a macro
/// expands to.
fn declarations(tokens: TokenStream, structs: &[String], found: &mut Vec<String>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let ident =
        |i: usize, name: &str| matches!(trees.get(i), Some(TokenTree::Ident(id)) if id == name);
    // Where the item that has its keyword at `i` starts: `before` steps back over the
    // tokens that may come right before that keyword, then over the attributes.
    let start = |i: usize, before: &dyn Fn(usize) -> usize| {
        let mut start = before(i);
        while start >= 2
            && matches!(&trees[start - 2], TokenTree::Punct(p) if p.as_char() == '#')
            && matches!(&trees[start - 1], TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket)
        {
            start -= 2;
        }
        start
    };
    let unsafe_before = |i: usize| if i > 0 && ident(i - 1, "unsafe") { i - 1 } else { i };
    // `pub`, or `pub(crate)` and the like.
    let visibility_before = |i: usize| {
        let restricted = i >= 2
            && ident(i - 2, "pub")
            && matches!(&trees[i - 1], TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis);
        if restricted {
            i - 2
        } else if i > 0 && ident(i - 1, "pub") {
            i - 1
        } else {
            i
        }
    };
    let brace =
        |t: &TokenTree| matches!(t, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace);
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Ident(id) if id == "extern" => {
                // `extern "<ABI>" {`, or `extern {`; not `extern crate`, nor `extern "C" fn`.
                let body = match trees.get(i + 1) {
                    Some(TokenTree::Literal(_)) => i + 2,
                    _ => i + 1,
                };
                if trees.get(body).is_some_and(brace) {
                    let from = start(i, &unsafe_before);
                    found.push(render(&trees[from..=body].iter().cloned().collect()));
                }
            }
            TokenTree::Ident(id) if id == "struct" => {
                if let Some(TokenTree::Ident(name)) = trees.get(i + 1)
                    && structs.iter().any(|s| name == s)
                {
                    let end = trees[i..]
                        .iter()
                        .position(|t| {
                            brace(t) || matches!(t, TokenTree::Punct(p) if p.as_char() == ';')
                        })
                        .map_or(trees.len() - 1, |n| i + n);
                    let from = start(i, &visibility_before);
                    found.push(render(&trees[from..=end].iter().cloned().collect()));
                }
            }
            TokenTree::Group(g) => declarations(g.stream(), structs, found),
            _ => {}
        }
    }
}

fn normalize(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// System libraries the Go runtime and rclone need when linked into a C program.
fn emit_platform_link_args() {
    match env_or_panic("CARGO_CFG_TARGET_OS").as_str() {
        "linux" => {
            for lib in ["pthread", "dl", "m", "resolv"] {
                println!("cargo:rustc-link-lib=dylib={lib}");
            }
        }
        "macos" => {
            for framework in ["CoreFoundation", "Security", "IOKit", "SystemConfiguration"] {
                println!("cargo:rustc-link-lib=framework={framework}");
            }
            println!("cargo:rustc-link-lib=dylib=resolv");
        }
        other => panic!("remote-sync-explorer does not support target OS `{other}`"),
    }
}

fn env_or_panic(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// Puts the third-party license texts into `OUT_DIR/third-party-licenses.txt`, for
/// `include_str!`: the file `THIRD_PARTY_LICENSES` names, or nothing.
fn third_party_licenses() {
    use std::path::{Path, PathBuf};
    use std::{env, fs};

    println!("cargo:rerun-if-env-changed=THIRD_PARTY_LICENSES");
    let out_dir = env::var_os("OUT_DIR").unwrap_or_else(|| panic!("cargo sets OUT_DIR"));
    let out = PathBuf::from(out_dir).join("third-party-licenses.txt");
    let texts = match env::var_os("THIRD_PARTY_LICENSES") {
        Some(path) => {
            let path = Path::new(&path);
            println!("cargo:rerun-if-changed={}", path.display());
            let texts = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("THIRD_PARTY_LICENSES: {}: {e}", path.display()));
            assert!(!texts.trim().is_empty(), "THIRD_PARTY_LICENSES: {} is empty", path.display());
            texts
        }
        None => String::new(),
    };
    fs::write(&out, texts).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
