//! A private ssh-agent: an `ssh-agent` of the program's own, holding one key the program
//! has in memory only (an inline key from a configuration), for the system ssh to log
//! in with. The key never goes into a file; the agent's socket is in a directory the
//! program keeps private; only the ssh commands the program gives [`Agent::ssh_options`]
//! use it; and it ends with the program, also one that is killed.
//!
//! The agent runs `cat` as its command, reading a pipe from here: when the pipe closes,
//! which it does when the program ends however it ends, `cat` ends, and the agent, whose
//! parent it was, ends within ten seconds (OpenSSH's check for a parent gone). The key is
//! handed to `ssh-add -` on its standard input.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// How long the agent may take to make its socket.
const START_TIME: Duration = Duration::from_secs(10);

/// An ssh-agent at work. Dropping it ends it.
pub struct Agent {
    /// `cat`, which the agent runs; the agent ends once it has.
    child: Child,
    stdin: Option<ChildStdin>,
    socket: PathBuf,
    public: PathBuf,
}

impl Agent {
    /// Starts an agent listening on `socket`, a path in a directory only this user can
    /// use, where nothing is yet. Its key's public half is kept at `socket` plus `.pub`.
    ///
    /// # Errors
    /// Fails when the path is one ssh would read otherwise than as it is, when there is
    /// no `ssh-agent`, or when it does not start listening.
    pub fn start(socket: PathBuf) -> io::Result<Self> {
        let mut public = socket.clone().into_os_string();
        public.push(".pub");
        let public = PathBuf::from(public);
        if !plain_path(&socket) || !plain_path(&public) {
            return Err(io::Error::other("the path of the agent's socket is no plain path"));
        }
        let mut child = Command::new("ssh-agent")
            .arg("-a")
            .arg(&socket)
            .arg("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // A process group of its own: the terminal's signals stay away from it.
            .process_group(0)
            .spawn()?;
        let stdin = child.stdin.take();
        let mut agent = Self { child, stdin, socket, public };
        let deadline = Instant::now() + START_TIME;
        loop {
            if fs::symlink_metadata(&agent.socket).is_ok_and(|m| m.file_type().is_socket()) {
                return Ok(agent);
            }
            if agent.child.try_wait()?.is_some() || Instant::now() >= deadline {
                // Dropping it removes what it left.
                agent.stdin = None;
                return Err(io::Error::other("ssh-agent did not start"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The agent's socket.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `ssh-add -`, for this agent: run with the private key on its standard input, and
    /// with what answers its question for a passphrase if the key has one (`Enter
    /// passphrase for (stdin): `, then `Bad passphrase, try again for (stdin): `).
    pub fn add_command(&self) -> Command {
        let mut cmd = Command::new("ssh-add");
        cmd.arg("-").env("SSH_AUTH_SOCK", &self.socket).stdout(Stdio::null()).stderr(Stdio::null());
        cmd
    }

    /// Keeps the public half of the keys the agent holds where [`Self::ssh_options`]
    /// names it, so that ssh offers those keys and no other.
    ///
    /// # Errors
    /// Fails when the agent holds no key or the file cannot be written.
    pub fn keep_public_key(&self) -> io::Result<()> {
        let out = Command::new("ssh-add")
            .arg("-L")
            .env("SSH_AUTH_SOCK", &self.socket)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if !out.status.success() || out.stdout.is_empty() {
            return Err(io::Error::other("the agent holds no key"));
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&self.public)?;
        io::Write::write_all(&mut file, &out.stdout)
    }

    /// The ssh options that log in with the agent's keys alone: its socket, and the public
    /// half of its keys, which ssh offers from the agent, as the only identity.
    pub fn ssh_options(&self) -> Vec<String> {
        vec![
            "-o".to_owned(),
            format!("IdentityAgent={}", self.socket.display()),
            "-i".to_owned(),
            self.public.display().to_string(),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
        ]
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // `cat` ends, and with it the agent; its socket goes at once, so nothing reaches
        // it meanwhile.
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.public);
    }
}

/// Whether ssh reads `path`, as the value of `IdentityAgent` or `IdentityFile`, as it is:
/// absolute, with none of the `%` and `${` it expands, nor the quote and white space it
/// splits at.
fn plain_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else { return false };
    text.starts_with('/')
        && !text.contains(['%', '$', '"', '\'', '\\'])
        && !text.contains(char::is_whitespace)
}

/// The text of a key kept in rclone's `key_pem`, which holds it as the inside of a Go
/// string literal: line breaks as `\n`. `None` when it is not one.
pub fn pem_text(kept: &str) -> Option<String> {
    let mut out = String::with_capacity(kept.len());
    let mut chars = kept.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '\\' => '\\',
            '"' => '"',
            _ => return None,
        });
    }
    out.contains("PRIVATE KEY").then_some(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Whether `program` is on the path. Not by running it: `ssh-keygen -h` waits for
    /// its input.
    fn have(program: &str) -> bool {
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
    }

    /// A key pair made for the test, with `passphrase`: the private key's text. `None`
    /// where ssh-keygen cannot run, such as for a user the system has no entry for (a
    /// container run with a bare uid), which it refuses.
    fn key(dir: &Path, passphrase: &str) -> Option<String> {
        let file = dir.join("key");
        let made = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-C", "rse test", "-N", passphrase, "-f"])
            .arg(&file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        if !made.status.success() {
            eprintln!("skipped: ssh-keygen: {}", String::from_utf8_lossy(&made.stderr).trim());
            return None;
        }
        let text = fs::read_to_string(&file).unwrap();
        fs::remove_file(&file).unwrap();
        fs::remove_file(dir.join("key.pub")).unwrap();
        Some(text)
    }

    #[test]
    fn a_key_goes_in_from_memory_and_everything_goes_at_the_end() {
        if !have("ssh-agent") || !have("ssh-keygen") {
            eprintln!("skipped: no ssh-agent or ssh-keygen");
            return;
        }
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let Some(text) = key(dir.path(), "") else { return };
        let agent = Agent::start(dir.path().join("agent")).unwrap();
        let mut add = agent.add_command();
        let mut child = add.stdin(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        agent.keep_public_key().unwrap();
        let public = fs::read_to_string(dir.path().join("agent.pub")).unwrap();
        assert!(public.starts_with("ssh-ed25519 ") && public.contains("rse test"), "{public}");
        assert!(!public.contains("PRIVATE"));
        let options = agent.ssh_options();
        assert!(options.contains(&format!("IdentityAgent={}", agent.socket().display())));
        assert!(options.contains(&"IdentitiesOnly=yes".to_owned()));
        // No file holds the private key.
        let names: Vec<_> =
            fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");

        let socket = agent.socket().to_owned();
        drop(agent);
        assert!(fs::symlink_metadata(&socket).is_err());
        assert!(fs::symlink_metadata(dir.path().join("agent.pub")).is_err());
    }

    #[test]
    fn an_agent_with_no_key_or_on_a_path_ssh_reads_otherwise_is_an_error() {
        if !have("ssh-agent") {
            eprintln!("skipped: no ssh-agent");
            return;
        }
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let agent = Agent::start(dir.path().join("agent")).unwrap();
        assert!(agent.keep_public_key().is_err());
        for bad in ["a b", "a%h", "a${HOME}", "a\"b"] {
            assert!(Agent::start(dir.path().join(bad)).is_err(), "{bad:?}");
        }
        assert!(Agent::start(PathBuf::from("relative")).is_err());
    }

    #[test]
    fn keys_are_read_as_rclone_keeps_them() {
        let kept = r"-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn\\Nza\n-----END OPENSSH PRIVATE KEY-----\n";
        assert_eq!(
            pem_text(kept).unwrap(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn\\Nza\n-----END OPENSSH PRIVATE KEY-----\n"
        );
        // As it is, too.
        let plain = "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n-----END OPENSSH PRIVATE KEY-----\n";
        assert_eq!(pem_text(plain).unwrap(), plain);
        assert_eq!(pem_text(r"-----BEGIN PRIVATE KEY-----\q"), None);
        assert_eq!(pem_text(r"-----BEGIN PRIVATE KEY-----\"), None);
        assert_eq!(pem_text("no key"), None);
    }
}
