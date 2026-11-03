//! `cargo xtask badges <dir> --deny <outcome>`: the README's badges, as files for shields.io's
//! endpoint badge, one per task, one for them all (`audit`) and one for what is pinned
//! (`pins`). A task's badge says whether its latest result stands for the code checked out,
//! as `cargo xtask status` does; deny's says how the check just run ended (`--deny`, the
//! outcome of its step in the workflow), as its result lapses daily by design.
//!
//! The advisories workflow writes them from a job that cannot push, and a job that runs none
//! of this code checks them and pushes them to the `badges` branch: the messages and colors
//! below are the only ones that job lets through, so a change here goes there too.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::{outdated, record};

/// How a badge reads: in order, the worst of them all is the `audit` badge's.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum State {
    UpToDate,
    NeverRun,
    OutOfDate,
    Failed,
}

impl State {
    fn badge(self) -> (&'static str, &'static str) {
        match self {
            Self::UpToDate => ("up to date", "brightgreen"),
            Self::NeverRun => ("never run", "lightgrey"),
            Self::OutOfDate => ("out of date", "yellow"),
            Self::Failed => ("failed", "red"),
        }
    }
}

/// The badge of all the tasks together.
fn overall(states: &[State]) -> (&'static str, &'static str) {
    match states.iter().max() {
        Some(State::UpToDate) => State::UpToDate.badge(),
        Some(State::Failed) => State::Failed.badge(),
        _ => State::OutOfDate.badge(),
    }
}

/// The badge of what is pinned, from how many updates are due and how many are urgent.
fn pins(found: Result<(usize, usize), String>) -> (String, &'static str) {
    match found {
        Ok((0, _)) => ("up to date".to_owned(), "brightgreen"),
        Ok((_, urgent)) if urgent > 0 => (format!("{urgent} urgent"), "red"),
        Ok((due, _)) => (format!("{due} due"), "yellow"),
        Err(e) => {
            eprintln!("xtask: badges: pins: {e}");
            ("check failed".to_owned(), "lightgrey")
        }
    }
}

/// A shields.io endpoint file.
fn endpoint(label: &str, message: &str, color: &str) -> String {
    format!(r#"{{"schemaVersion":1,"label":"{label}","message":"{message}","color":"{color}"}}"#)
        + "\n"
}

pub fn main(
    root: &Path,
    args: &[String],
    tasks: &[&str],
    needed: impl Fn(&str) -> Vec<String>,
) -> ExitCode {
    let (dir, deny) = match args {
        [dir, flag, outcome] if flag == "--deny" => (root.join(dir), outcome.as_str()),
        _ => {
            eprintln!("xtask: usage: cargo xtask badges <dir> --deny <success|failure|...>");
            return ExitCode::FAILURE;
        }
    };
    let mut badges = Vec::new();
    let mut states = Vec::new();
    for task in tasks {
        let state = if *task == "deny" {
            if deny == "success" { State::UpToDate } else { State::Failed }
        } else {
            match record::latest_succeeded(root, task) {
                None => State::NeverRun,
                Some(false) => State::Failed,
                Some(true) if needed(task).is_empty() => State::UpToDate,
                Some(true) => State::OutOfDate,
            }
        };
        let (message, color) = match (*task, state) {
            ("deny", State::UpToDate) => ("passing", "brightgreen"),
            ("deny", _) => ("failing", "red"),
            _ => state.badge(),
        };
        states.push(state);
        badges.push(((*task).to_owned(), message.to_owned(), color));
    }
    let (message, color) = overall(&states);
    badges.push(("audit".to_owned(), message.to_owned(), color));
    let (message, color) = pins(outdated::count(root));
    badges.push(("pins".to_owned(), message, color));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("xtask: {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    for (label, message, color) in badges {
        let path = dir.join(format!("{label}.json"));
        if let Err(e) = fs::write(&path, endpoint(&label, &message, color)) {
            eprintln!("xtask: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{label}: {message}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_the_tasks_read_as_the_worst_of_them() {
        use State::*;
        assert_eq!(overall(&[UpToDate, UpToDate]), ("up to date", "brightgreen"));
        assert_eq!(overall(&[UpToDate, NeverRun]), ("out of date", "yellow"));
        assert_eq!(overall(&[OutOfDate, UpToDate]), ("out of date", "yellow"));
        assert_eq!(overall(&[OutOfDate, Failed, NeverRun]), ("failed", "red"));
    }

    #[test]
    fn pins_say_how_many_are_due() {
        assert_eq!(pins(Ok((0, 0))), ("up to date".to_owned(), "brightgreen"));
        assert_eq!(pins(Ok((3, 0))), ("3 due".to_owned(), "yellow"));
        assert_eq!(pins(Ok((3, 1))), ("1 urgent".to_owned(), "red"));
        assert_eq!(pins(Err("offline".to_owned())), ("check failed".to_owned(), "lightgrey"));
    }

    #[test]
    fn badges_are_shields_endpoint_files() {
        assert_eq!(
            endpoint("miri", "up to date", "brightgreen"),
            "{\"schemaVersion\":1,\"label\":\"miri\",\"message\":\"up to date\",\"color\":\"brightgreen\"}\n"
        );
    }
}
