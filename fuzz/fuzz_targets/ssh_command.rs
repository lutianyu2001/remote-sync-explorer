//! sftp options from rclone.conf or a connection string, turned into an ssh command for
//! rclone: no value becomes an ssh option or another argument, and the fs name sent to
//! rclone carries exactly the intended options.
#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use rse::fuzz::{TRANSFER_ONLY, conn_quote, split_params, ssh_command, ssh_dest, ssh_fs_name};

/// Options the system ssh takes over from rclone (`SSH_TAKES_OVER`).
const TAKEN: [&str; 10] = [
    "host",
    "user",
    "port",
    "pass",
    "key_file",
    "key_file_pass",
    "pubkey_file",
    "known_hosts_file",
    "ask_password",
    "key_use_agent",
];

fuzz_target!(|input: (bool, Vec<(String, String)>)| {
    let (askpass, pairs) = input;
    // Option names as rclone has them; values are anything.
    let params: BTreeMap<String, String> = pairs
        .into_iter()
        .filter(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .collect();
    let mut name = ":sftp".to_owned();
    for (k, v) in &params {
        name.push_str(&format!(",{k}={}", conn_quote(v)));
        // Quoting reads back as the value.
        let one = split_params(&format!("x,{k}={}", conn_quote(v)));
        assert_eq!(one, [("x".to_owned(), String::new()), (k.clone(), v.clone())]);
    }
    assert_eq!(split_params(&name)[1..], params.clone().into_iter().collect::<Vec<_>>()[..]);

    let Ok(dest) = ssh_dest(&params, askpass) else { return };
    let last = dest.last().expect("a destination");
    assert!(!last.starts_with('-'), "{last} would be an ssh option");
    let control = vec!["-o".to_owned(), "ControlPath=/tmp/rse-1000/%C".to_owned()];
    let cmd = ssh_command(&control, &dest);
    let mut argv = vec!["ssh", "-o", "BatchMode=yes"];
    argv.extend(TRANSFER_ONLY);
    argv.extend(["-o", "ControlMaster=no"]);
    argv.extend(control.iter().map(String::as_str));
    argv.extend(dest.iter().map(String::as_str));
    assert_eq!(
        go_csv_record(&cmd).as_deref(),
        Ok(&argv.iter().map(|a| a.to_string()).collect::<Vec<_>>()[..]),
        "{cmd}"
    );

    let fs = ssh_fs_name(&name, &params, &cmd);
    let parts = split_params(&fs);
    assert_eq!(parts[0].0, ":sftp");
    let ssh: Vec<_> = parts.iter().filter(|(k, _)| k == "ssh").collect();
    assert_eq!(ssh.len(), 1, "{fs}");
    assert_eq!(ssh[0].1, cmd);
    for (k, v) in &parts[1..] {
        if k == "ssh" {
            continue;
        }
        if TAKEN.contains(&k.as_str()) {
            assert!(v.is_empty() || v == "false", "{k}={v} left in {fs}");
        } else {
            assert_eq!(params.get(k), Some(v), "{k} changed in {fs}");
        }
    }
});

/// The first record of `s` as Go's encoding/csv reads it with `Comma = ' '`, which is how
/// rclone reads a space-separated list option (`fs.SpaceSepList`).
fn go_csv_record(s: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        let mut field = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    None => return Err("unterminated quote".into()),
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        field.push('"');
                    }
                    Some('"') => break,
                    Some('\r') if chars.peek() == Some(&'\n') => {}
                    Some(c) => field.push(c),
                }
            }
            match chars.next() {
                None | Some('\n') => {
                    fields.push(field);
                    return Ok(fields);
                }
                Some(' ') => fields.push(field),
                Some(c) => return Err(format!("{c:?} after a quoted field")),
            }
        } else {
            loop {
                match chars.next() {
                    None | Some('\n') => {
                        fields.push(field);
                        return Ok(fields);
                    }
                    Some('\r') if chars.peek() == Some(&'\n') => {}
                    Some(' ') => break,
                    Some('"') => return Err("bare quote".into()),
                    Some(c) => field.push(c),
                }
            }
            fields.push(field);
        }
    }
}
