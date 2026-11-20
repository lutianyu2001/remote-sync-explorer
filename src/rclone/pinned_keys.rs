//! A remote's pinned host keys (`host_keys`, `pin_host_key`) for the system ssh: a
//! `known_hosts` file of rse's own holding them alone, and the ssh options that check the
//! server against it and nothing else.
//!
//! As rclone has it (v1.75.2): the keys are pinned only when the remote has no
//! `known_hosts_file`, which takes precedence. With `host_keys` alone, the server must
//! show one of them, and only their algorithms are negotiated; nothing is asked. With
//! `pin_host_key`, a key the server shows for an algorithm with none pinned is accepted on
//! first use and added to `host_keys`; rse asks the user first, as for any server not
//! seen before. At most [`MAX_KEYS`] are kept.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use super::obscure::base64_url_decode;

/// The most keys `host_keys` holds, as rclone caps it.
pub const MAX_KEYS: usize = 16;

/// The pinned keys of one remote, in the file ssh checks them against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// The file, in the private socket directory.
    pub file: PathBuf,
    /// The keys, as `host_keys` lists them: `algo base64`.
    pub keys: BTreeSet<String>,
    /// Whether a new key may be accepted on first use (`pin_host_key`).
    pub on_first_use: bool,
    /// The remote in rclone.conf whose `host_keys` a key accepted on first use goes to;
    /// `None` for one given as a connection string, which has nowhere to keep it.
    pub persist_to: Option<String>,
}

/// One `algo base64` entry of `host_keys`, checked as rclone checks it: two fields, the
/// key in standard base64, and of the type its algorithm names, which is the first string
/// of the key itself. `None` for anything else.
pub fn entry(text: &str) -> Option<String> {
    let mut fields = text.split_whitespace();
    let (algo, key) = (fields.next()?, fields.next()?);
    if fields.next().is_some()
        || !algo.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.@_".contains(&b))
    {
        return None;
    }
    let url = key.trim_end_matches('=').replace('+', "-").replace('/', "_");
    let blob = base64_url_decode(&url)?;
    let len = usize::try_from(u32::from_be_bytes(blob.get(..4)?.try_into().ok()?)).ok()?;
    let kind = blob.get(4..4 + len)?;
    // A certificate cannot be pinned, as rclone has it.
    (kind == algo.as_bytes() && !algo.contains("-cert-")).then(|| format!("{algo} {key}"))
}

/// The keys of rclone's `host_keys` (entries apart by commas), or why they cannot be
/// used. An empty value pins none.
pub fn parse(value: &str) -> Result<BTreeSet<String>, String> {
    let mut keys = BTreeSet::new();
    for (i, text) in value.split(',').map(str::trim).filter(|t| !t.is_empty()).enumerate() {
        let key = entry(text).ok_or_else(|| format!("host_keys entry {} is not a key", i + 1))?;
        keys.insert(key);
    }
    if keys.len() > MAX_KEYS {
        return Err(format!("host_keys has more than {MAX_KEYS} entries"));
    }
    Ok(keys)
}

/// `host_keys` as rclone writes it: the entries in order, apart by commas.
pub fn format(keys: &BTreeSet<String>) -> String {
    keys.iter().map(String::as_str).collect::<Vec<_>>().join(",")
}

impl Pins {
    /// Writes `keys` to `file` for ssh, each for any name of the server, since the file
    /// is this remote's alone.
    ///
    /// # Errors
    /// Fails when the file cannot be written; one already there is never written through.
    pub fn new(
        file: PathBuf,
        keys: BTreeSet<String>,
        on_first_use: bool,
        persist_to: Option<String>,
    ) -> Result<Self, String> {
        let plain = file.to_str().is_some_and(|f| {
            f.starts_with('/')
                && !f.contains(['%', '$', '"', '\'', '\\'])
                && !f.contains(char::is_whitespace)
        });
        if !plain {
            return Err("the path of the pinned keys' file is no plain path".to_owned());
        }
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let text = keys.iter().fold(String::new(), |mut text, key| {
            text.push_str("* ");
            text.push_str(key);
            text.push('\n');
            text
        });
        out.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(Self { file, keys, on_first_use, persist_to })
    }

    /// The ssh options that check the server against these keys alone: never another
    /// `known_hosts` file, never a key learned after logging in, and a question only for a
    /// key on first use. With `host_keys` alone, only their algorithms are negotiated,
    /// unless `algorithms_set` (the remote names its own).
    pub fn ssh_options(&self, algorithms_set: bool) -> Vec<String> {
        let strict = if self.on_first_use { "ask" } else { "yes" };
        let mut options = vec![
            format!("UserKnownHostsFile={}", self.file.display()),
            "GlobalKnownHostsFile=/dev/null".to_owned(),
            format!("StrictHostKeyChecking={strict}"),
            "UpdateHostKeys=no".to_owned(),
            "CheckHostIP=no".to_owned(),
            "HashKnownHosts=no".to_owned(),
        ];
        if !self.on_first_use && !algorithms_set && !self.keys.is_empty() {
            options.push(format!("HostKeyAlgorithms={}", algorithms(&self.keys).join(",")));
        }
        options.into_iter().flat_map(|o| ["-o".to_owned(), o]).collect()
    }

    /// The keys ssh added to the file since, accepted on first use, which are now pinned
    /// too; at most as many as `host_keys` may hold.
    pub fn accepted(&mut self) -> Vec<String> {
        let text = fs::read_to_string(&self.file).unwrap_or_default();
        let mut new = Vec::new();
        for line in text.lines() {
            // `name algo key`, maybe with a marker or a comment: the key is after its algo.
            let fields: Vec<&str> = line.split_whitespace().collect();
            let found = fields.windows(2).find_map(|w| entry(&format!("{} {}", w[0], w[1])));
            if let Some(key) = found
                && self.keys.len() < MAX_KEYS
                && self.keys.insert(key.clone())
            {
                new.push(key);
            }
        }
        new
    }
}

/// The host key algorithms to negotiate for `keys`: their own, with RSA's signatures.
fn algorithms(keys: &BTreeSet<String>) -> Vec<String> {
    let mut algos = BTreeSet::new();
    for key in keys {
        let algo = key.split_whitespace().next().unwrap_or_default();
        if algo == "ssh-rsa" {
            algos.insert("rsa-sha2-512".to_owned());
            algos.insert("rsa-sha2-256".to_owned());
        }
        algos.insert(algo.to_owned());
    }
    algos.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real public keys, as `ssh-keygen` makes them.
    const ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID8QM4m8ICcfBlOyA91d2viKpsYAZ3pgnTEMx/AAAWxQ";
    const OTHER_ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIeEtDDE2xCdS0dECVGCZZ3U8YvRYrUOVF56TT5d0oo/";

    #[test]
    fn entries_are_checked_as_rclone_checks_them() {
        assert_eq!(entry(ED25519).as_deref(), Some(ED25519));
        assert_eq!(entry(&format!("  {ED25519}  ")).as_deref(), Some(ED25519));
        // The stated algorithm is not the key's.
        let wrong = ED25519.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert_eq!(entry(&wrong), None);
        for bad in ["ssh-ed25519", "ssh-ed25519 !!!", "a b c", "", "x,y AAAA"] {
            assert_eq!(entry(bad), None, "{bad:?}");
        }
        let both = format!("{OTHER_ED25519}, {ED25519},");
        let keys = parse(&both).unwrap();
        assert_eq!(format(&keys), format!("{ED25519},{OTHER_ED25519}"));
        assert!(parse("").unwrap().is_empty());
        assert!(parse("ssh-ed25519 nope").unwrap_err().contains("entry 1"));
    }

    #[test]
    fn the_file_holds_the_keys_for_any_name_and_takes_what_ssh_adds() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let file = dir.path().join("known-1");
        let keys = parse(ED25519).unwrap();
        let mut pins = Pins::new(file.clone(), keys, true, None).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("* {ED25519}\n"));
        // Never written through, nor twice.
        assert!(Pins::new(file.clone(), BTreeSet::new(), false, None).is_err());
        // As ssh adds a key accepted on first use: under the name it used, maybe hashed.
        let mut text = fs::read_to_string(&file).unwrap();
        for line in [&format!("[127.0.0.1]:2222 {OTHER_ED25519}"), &format!("|1|x|y= {ED25519}")] {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(&file, text).unwrap();
        assert_eq!(pins.accepted(), [OTHER_ED25519]);
        assert_eq!(pins.accepted(), Vec::<String>::new(), "once");
        assert_eq!(pins.keys.len(), 2);
    }

    #[test]
    fn options_ask_only_on_first_use_and_narrow_the_algorithms_otherwise() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let rsa = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQAB";
        let keys = parse(&format!("{ED25519},{rsa}")).unwrap();
        let pinned = Pins::new(dir.path().join("known-1"), keys.clone(), false, None).unwrap();
        let options = pinned.ssh_options(false);
        assert!(options.contains(&"StrictHostKeyChecking=yes".to_owned()));
        assert!(options.contains(
            &"HostKeyAlgorithms=rsa-sha2-256,rsa-sha2-512,ssh-ed25519,ssh-rsa".to_owned()
        ));
        assert!(options.contains(&format!("UserKnownHostsFile={}", pinned.file.display())));
        assert!(!pinned.ssh_options(true).iter().any(|o| o.starts_with("HostKeyAlgorithms")));
        let tofu = Pins::new(dir.path().join("known-2"), keys, true, None).unwrap();
        let options = tofu.ssh_options(false);
        assert!(options.contains(&"StrictHostKeyChecking=ask".to_owned()));
        assert!(!options.iter().any(|o| o.starts_with("HostKeyAlgorithms")));
        assert!(Pins::new(dir.path().join("a b"), BTreeSet::new(), false, None).is_err());
    }
}
