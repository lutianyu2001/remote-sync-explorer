//! The license texts this program carries, which `--licenses` prints: the project's own,
//! then the notices of the third-party code compiled into it, which a release build embeds
//! (build.rs, `third_party_licenses`). Copied into the project as `src/licenses.rs`, and
//! declared as a module of the binary; only the lines marked "Per project" change.

use std::io::{self, IsTerminal as _, Write as _};
use std::process::{Command, Stdio};

// Per project: the project's license files, relative to this file, in the order they are
// printed (LGPL: COPYING, then COPYING.LESSER).
const OWN: &[&str] = &[include_str!("../LICENSE")];

/// Empty unless the release build set `THIRD_PARTY_LICENSES`.
const THIRD_PARTY: &str = include_str!(concat!(env!("OUT_DIR"), "/third-party-licenses.txt"));

// Per project: where a development build's reader finds the texts.
const ELSEWHERE: &str = "This is a development build, which carries no third-party license \
texts: a release build prints them here, and each release archive holds them in \
third-party/. They are the licenses of the dependencies in Cargo.lock and librse/go.sum.";

/// Shows the texts: in a pager when standard output is a terminal, as `git log` and `man`
/// do, else written out as they are. The third-party texts come last and unchanged, so
/// that the release job can compare the end of the output with the file it embedded. A
/// closed pipe (`--licenses | head`, or quitting the pager early) is not an error.
pub fn print() -> io::Result<()> {
    let mut text = String::new();
    for own in OWN {
        text.push_str(own);
        text.push('\n');
    }
    if THIRD_PARTY.is_empty() {
        text.push_str(ELSEWHERE);
        text.push('\n');
    } else {
        text.push_str(THIRD_PARTY);
    }
    if io::stdout().is_terminal()
        && let Some(paged) = page(&text)
    {
        return paged;
    }
    let mut out = io::stdout().lock();
    quiet_on_closed_pipe(out.write_all(text.as_bytes()).and_then(|()| out.flush()))
}

/// Shows `text` in a pager, chosen as git chooses one: `PAGER` run by the shell, else
/// `less`, which gets `LESS=FRX` unless `LESS` is set (quit at once when the text fits on
/// one screen, keep colours, leave the text on the screen). None when there is none to
/// run: `PAGER` empty or `cat`, or no `less` (Windows, as a rule).
fn page(text: &str) -> Option<io::Result<()>> {
    let mut pager = match std::env::var("PAGER") {
        Ok(p) if p.trim().is_empty() || p.trim() == "cat" => return None,
        Ok(p) if cfg!(unix) => {
            let mut shell = Command::new("sh");
            shell.arg("-c").arg(p);
            shell
        }
        Ok(p) => Command::new(p),
        Err(_) => Command::new("less"),
    };
    if std::env::var_os("LESS").is_none() {
        pager.env("LESS", "FRX");
    }
    let mut child = pager.stdin(Stdio::piped()).spawn().ok()?;
    // Dropped at the end of the statement, which closes the pipe: the pager sees the end.
    let written = child.stdin.take().map_or(Ok(()), |mut stdin| stdin.write_all(text.as_bytes()));
    let waited = child.wait().map(|_| ());
    Some(quiet_on_closed_pipe(written).and(waited))
}

fn quiet_on_closed_pipe(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}
