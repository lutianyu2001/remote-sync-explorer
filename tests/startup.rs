//! Start-up: encrypted rclone configs and the `rse` binary's behaviour before the
//! interface opens. `tests/fixtures/encrypted-rclone.conf` holds one memory remote,
//! `encremote`, encrypted with the password `sekrit`.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

use rse::rclone::{Backend, Embedded, api};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encrypted-rclone.conf")
}

/// A copy, because rclone may rewrite the file it was given.
fn fixture_copy(dir: &tempfile::TempDir) -> String {
    let p = dir.path().join("rclone.conf");
    std::fs::copy(fixture(), &p).unwrap();
    p.to_str().unwrap().to_owned()
}

#[test]
fn encrypted_config_needs_unlocking() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Embedded::new();
    api::set_config_path(&backend, &fixture_copy(&dir)).unwrap();
    api::disable_ask_password(&backend).unwrap();

    api::unlock_config(&backend, "wrong").unwrap();
    let err = api::listremotes(&backend).unwrap_err();
    assert!(err.message.contains("decrypt"), "{err:?}");

    api::unlock_config(&backend, "sekrit").unwrap();
    assert_eq!(api::listremotes(&backend).unwrap(), ["encremote"]);
    assert_eq!(api::list(&backend, "encremote:", "").unwrap().len(), 0);
    backend.finalize();
}

fn rse(args: &[&str], envs: &[(&str, &str)]) -> (bool, String, String) {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rse"));
    cmd.args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env_remove("RCLONE_CONFIG_PASS")
        // English unless a test says otherwise.
        .env_remove("LANGUAGE")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "C.UTF-8")
        .stdin(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn version_names_both_versions() {
    let (ok, out, _) = rse(&["--version"], &[]);
    assert!(ok);
    assert!(out.starts_with("rse "), "{out}");
    assert!(out.contains(&format!("rclone {}", env!("RSE_RCLONE_VERSION"))), "{out}");
}

#[test]
fn locked_config_is_reported_before_the_interface_starts() {
    let dir = tempfile::tempdir().unwrap();
    let conf = fixture_copy(&dir);
    let (ok, _, err) = rse(&["--rclone-config", &conf], &[]);
    assert!(!ok);
    assert!(err.contains("--ask-password or set RCLONE_CONFIG_PASS"), "{err}");
}

#[test]
fn env_password_unlocks_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let conf = fixture_copy(&dir);
    // Without a terminal, start-up gets past the config and fails at the terminal.
    let (ok, _, err) = rse(&["--rclone-config", &conf], &[("RCLONE_CONFIG_PASS", "sekrit")]);
    assert!(!ok);
    assert!(!err.contains("rclone config"), "{err}");
    assert!(err.contains("terminal"), "{err}");
}

#[test]
fn bad_config_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[ui]\nmouse = \"maybe\"\n").unwrap();
    let (ok, _, err) = rse(&["--config", cfg.to_str().unwrap()], &[]);
    assert!(!ok);
    assert!(err.contains("config.toml"), "{err}");

    std::fs::write(&cfg, "[keys]\ncopy = [\"m\"]\n").unwrap();
    let (ok, _, err) = rse(&["--config", cfg.to_str().unwrap()], &[]);
    assert!(!ok);
    assert!(err.contains("bound to both"), "{err}");
}

#[test]
fn rcd_flag_without_the_feature_is_explained() {
    if cfg!(feature = "rcd") {
        return;
    }
    let (ok, _, err) = rse(&["--rcd", "http://localhost:5572"], &[]);
    assert!(!ok);
    assert!(err.contains("without the `rcd` feature"), "{err}");
}

#[test]
fn command_line_help_and_errors_follow_the_language() {
    let (ok, out, _) = rse(&["--help"], &[]);
    assert!(ok);
    assert!(out.contains("\nUsage: rse [OPTIONS] [LEFT] [RIGHT]\n"), "{out}");
    assert!(out.contains("\n  -h, --help                     Print help\n"), "{out}");
    let (ok, _, err) = rse(&["--bogus"], &[]);
    assert!(!ok);
    assert!(err.starts_with("error: unexpected argument '--bogus' found\n"), "{err}");
    assert!(err.ends_with("For more information, try '--help'.\n"), "{err}");

    let chinese = [("LANG", "zh_CN.UTF-8")];
    let (ok, out, _) = rse(&["--help"], &chinese);
    assert!(ok);
    assert!(out.contains("\n用法：rse [选项] [左侧位置] [右侧位置]\n"), "{out}");
    // The descriptions line up although the value names are wide characters.
    assert!(out.contains("\n      --rc-user <用户名>        --rcd 的用户名\n"), "{out}");
    assert!(out.contains("\n  -h, --help                    显示帮助\n"), "{out}");
    let (ok, _, err) = rse(&["--bogus"], &chinese);
    assert!(!ok);
    assert!(err.starts_with("错误：无法识别的参数 '--bogus'\n"), "{err}");
    assert!(err.contains("提示：如果 '--bogus' 是要打开的位置，请写成 '-- --bogus'"), "{err}");
    let (ok, _, err) = rse(&["--conf"], &chinese);
    assert!(!ok);
    assert!(err.contains("提示：有相近的参数：'--config'"), "{err}");
}

#[test]
fn language_in_the_config_file_applies_from_the_command_line_on() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[ui]\nlanguage = \"zh-CN\"\n").unwrap();
    let (ok, out, _) = rse(&["--config", cfg.to_str().unwrap(), "--help"], &[]);
    assert!(ok);
    assert!(out.contains("用法："), "{out}");

    std::fs::write(&cfg, "[ui]\nlanguage = \"zh-CN\"\n[keys]\ncopy = [\"m\"]\n").unwrap();
    let (ok, _, err) = rse(&[&format!("--config={}", cfg.display())], &[]);
    assert!(!ok);
    assert!(err.starts_with("rse: 配置文件有误：[keys]：`m` 同时绑定给了"), "{err}");
}

#[test]
fn forget_password_deletes_a_remembered_one() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    let file = ssh.join("rse-passwords");
    std::fs::write(&file, "# mine\nme@box 3T9PgUymd9w-jZk2CoHnCElf\nroot@nas:2222 x\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let env = [("HOME", home.path().to_str().unwrap())];

    let (ok, out, err) = rse(&["--forget-password", "me@box"], &env);
    assert!(ok, "{err}");
    assert!(out.contains("Forgot the password for me@box"), "{out}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# mine\nroot@nas:2222 x\n");

    let (ok, _, err) = rse(&["--forget-password", "me@box"], &env);
    assert!(!ok);
    assert!(err.contains("no password is remembered for me@box"), "{err}");
}
