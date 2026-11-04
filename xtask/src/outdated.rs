//! `cargo xtask outdated`: whether the pinned tools, the nightly toolchain and the actions
//! the workflows use have fallen behind, and updating them if the user agrees.
//!
//! Only releases count, not pre-releases or yanked ones, and only once the newest was
//! released at least `MONTH` days after the pinned one, so that a tool released daily
//! does not ask every day; and only when there is a version to update to: released at
//! least `WAIT` days ago, not yanked, plain `x.y.z` and without a known vulnerability. A
//! known vulnerability in the pinned version makes it due at once, even with nothing to
//! update to yet. The nightly toolchain is due once its date is `MONTH` days old.
//!
//! Actions are pinned to a commit, with the release it is after it, as in
//! `uses: owner/repo@<sha> # vX.Y.Z`, so that a tag moved by whoever controls the action's repository cannot change
//! what runs. Their releases are GitHub's; an action not pinned so is due at once, and so is
//! one whose tag no longer names the commit pinned, which nothing updates by itself: someone
//! has to find out why first.
//!
//! Other tasks look by themselves: for vulnerabilities daily, for releases weekly
//! (`RUST_XTASK_VULNS_EVERY`, `RUST_XTASK_RELEASES_EVERY`, in days; 0 is every run).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use serde_json::Value;

use crate::pins::{NIGHTLY, TOOLS};

const MONTH: i64 = 30;
/// How old a release must be to be updated to: a release yanked, or a crate taken over,
/// is mostly found within days.
const WAIT: i64 = 7;
/// How old a nightly must be to be suggested: regressions in a nightly (a broken Miri,
/// a compiler crash) are mostly fixed within days.
const SETTLED: i64 = 7;
/// How often other tasks look, in days, unless the environment says otherwise.
const RELEASES_EVERY: (&str, u64) = ("RUST_XTASK_RELEASES_EVERY", 7);
const VULNS_EVERY: (&str, u64) = ("RUST_XTASK_VULNS_EVERY", 1);
/// crates.io asks for one that says who is calling.
// Per project: its name and address, and no e-mail.
const USER_AGENT: &str = "rse-xtask (+https://github.com/lutianyu2001/remote-sync-explorer)";
/// OSV's names for where the tools and the actions come from.
const CRATES: &str = "crates.io";
const ACTIONS: &str = "GitHub Actions";
/// Where the workflows are.
const WORKFLOWS: &str = ".github/workflows";

/// What a pinned version is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Tool,
    Nightly,
    Action,
}

/// An update that is due.
struct Due {
    kind: Kind,
    /// The crate, `nightly`, or the action's `owner/repo`.
    what: String,
    from: String,
    /// None for a known vulnerability with no version to update to yet.
    to: Option<String>,
    /// For an action, the commit of `to`.
    commit: Option<String>,
    why: String,
    /// A known vulnerability, or an action's tag moved off the commit pinned.
    urgent: bool,
}

impl std::fmt::Display for Due {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let to = self.to.as_deref().unwrap_or("nothing to update to yet");
        write!(f, "{}: {} -> {to} ({})", self.what, self.from, self.why)
    }
}

/// `text` from a server or an error, safe to print: without what would act on the terminal or
/// reorder the line.
fn printable(text: &str) -> String {
    let harmful = |c: char| {
        c.is_control()
            || matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}')
            || matches!(c, '\u{2066}'..='\u{2069}')
    };
    text.chars().map(|c| if harmful(c) { '?' } else { c }).collect()
}

/// `text` as a JSON string.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The advisories in an OSV reply. A reply of another shape is an error: read as no
/// vulnerabilities, it would let a vulnerable tool through.
fn advisories(reply: &Value) -> Result<&[Value], String> {
    if reply.as_object().is_none() {
        return Err("the OSV reply is not an object".to_owned());
    }
    match reply["vulns"].as_array() {
        Some(vulns) => Ok(vulns),
        None if reply.as_object().is_some_and(|fields| fields.contains_key("vulns")) => {
            Err("the OSV reply's vulns is not a list".to_owned())
        }
        None => Ok(&[]),
    }
}

/// The ids in an OSV reply.
fn advisory_ids(reply: &Value) -> Result<Vec<String>, String> {
    let vulns = advisories(reply)?;
    if vulns.is_empty() && reply["next_page_token"].as_str().is_some_and(|t| !t.is_empty()) {
        return Err("the OSV reply has more pages and nothing on this one".to_owned());
    }
    vulns
        .iter()
        .map(|v| {
            let id = v["id"].as_str().ok_or("an OSV advisory has no id")?;
            Ok(printable(id))
        })
        .collect()
}

/// `cargo xtask outdated`. Exits with 2 when updates are due and it cannot ask, so that
/// CI shows it, and when a pinned version has a known vulnerability and nothing to update
/// to yet, whether it asked or not.
/// With `--vulns`, only known vulnerabilities and what is not pinned as it should be, for a
/// daily check.
pub fn main(root: &Path, args: &[String]) -> ExitCode {
    let releases = match args {
        [] => true,
        [flag] if flag == "--vulns" => false,
        _ => {
            eprintln!("xtask: usage: cargo xtask outdated [--vulns]");
            return ExitCode::FAILURE;
        }
    };
    let (due, waiting) = match due(root, Duration::from_secs(30), releases) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("xtask: {}", printable(&e));
            return ExitCode::FAILURE;
        }
    };
    let _ = remember_check(root, &due, releases);
    for w in &waiting {
        println!("{}", printable(w));
    }
    if due.is_empty() {
        println!(
            "{}",
            if releases {
                "No pinned version is due: no release to update to a month newer, no known vulnerability."
            } else {
                "No known vulnerability in the pinned versions."
            }
        );
        return ExitCode::SUCCESS;
    }
    for d in &due {
        println!("{}", printable(&d.to_string()));
    }
    let stuck = due.iter().any(|d| d.to.is_none());
    if !io::stdin().is_terminal() {
        return ExitCode::from(2);
    }
    // Not while a task uses the tools; setup, a process of its own, takes the lock again.
    let applied = match crate::lock::exclusive(root) {
        Ok(_held) => update(root, &due),
        Err(e) => Err(e),
    };
    match applied {
        Ok((0, _)) => {}
        Ok((_, tools)) => {
            if tools {
                println!("Pinned in xtask/src/pins.rs. Installing them:");
                // A new run, built from the edited source.
                let installed =
                    Command::new("cargo").args(["xtask", "setup"]).current_dir(root).status();
                if !installed.is_ok_and(|s| s.success()) {
                    return ExitCode::FAILURE;
                }
            }
            println!(
                "Now commit what changed in xtask/src/pins.rs and {WORKFLOWS}/, run `cargo xtask all \
                 --if-needed`, fix what the new versions find, and commit the results."
            );
        }
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::FAILURE;
        }
    }
    if stuck { ExitCode::from(2) } else { ExitCode::SUCCESS }
}

/// Asks about each update there is a version for, and writes each one agreed to at once,
/// so that a later one failing keeps the earlier; how many it wrote, and whether any was of
/// a tool or the nightly.
fn update(root: &Path, due: &[Due]) -> Result<(usize, bool), String> {
    let (mut written, mut tools) = (0, false);
    for d in due {
        let Some(to) = &d.to else { continue };
        if !ask(&format!("Update {} {} -> {to}?", d.what, d.from)) {
            continue;
        }
        let to = if d.kind == Kind::Nightly {
            match settle(to) {
                Ok(to) => to,
                Err(e) => {
                    eprintln!("xtask: {}", printable(&e));
                    continue;
                }
            }
        } else {
            to.clone()
        };
        apply(root, d, &to)?;
        written += 1;
        tools |= d.kind != Kind::Action;
    }
    Ok((written, tools))
}

/// For other tasks: say in one line what is due, looking for vulnerabilities and for
/// releases each as often as set. Silent when the network is not there, and counted as a
/// check all the same, so that an offline machine does not wait on the network at every
/// task; the daily advisories workflow still looks.
pub fn notice(root: &Path) {
    let dir = root.join("target").join("audit");
    let releases = stale(&dir.join("releases-checked"), RELEASES_EVERY);
    if !releases && !stale(&dir.join("vulns-checked"), VULNS_EVERY) {
        return;
    }
    let due = due(root, Duration::from_secs(10), releases).map(|f| f.0).unwrap_or_default();
    let _ = remember_check(root, &due, releases);
    if !due.is_empty() {
        let names: Vec<&str> = due.iter().map(|d| d.what.as_str()).collect();
        eprintln!(
            "xtask: newer versions are due for {}; `cargo xtask outdated` shows them and can update",
            names.join(", ")
        );
    }
}

/// For the badges: how many updates are due, and how many of them are urgent.
pub fn count(root: &Path) -> Result<(usize, usize), String> {
    let (due, _) = due(root, Duration::from_secs(30), true)?;
    Ok((due.len(), due.iter().filter(|d| d.urgent).count()))
}

/// Whether the check `stamp` records is older than its interval.
fn stale(stamp: &Path, (var, days): (&str, u64)) -> bool {
    let days: u64 = std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(days);
    let every = Duration::from_secs(days.saturating_mul(86_400));
    !fs::metadata(stamp)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < every))
}

/// Records the check and what it found, in `target/audit/`.
fn remember_check(root: &Path, due: &[Due], releases: bool) -> io::Result<()> {
    let dir = root.join("target").join("audit");
    fs::create_dir_all(&dir)?;
    let mut text = String::new();
    for d in due {
        let _ = writeln!(text, "{d}");
    }
    fs::write(dir.join("vulns-checked"), &text)?;
    if releases {
        fs::write(dir.join("releases-checked"), &text)?;
    }
    Ok(())
}

/// What is due: known vulnerabilities and actions not pinned as they should be, and with
/// `releases` also newer releases; and newer releases with nothing to update to yet.
fn due(root: &Path, timeout: Duration, releases: bool) -> Result<(Vec<Due>, Vec<String>), String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
    let (mut due, mut waiting) = (Vec::new(), Vec::new());
    let mut sort = |found: Found| match found {
        Found::Current => {}
        Found::Due(d) => due.push(d),
        Found::Waiting(why) => waiting.push(why),
    };
    for (krate, pinned, _) in TOOLS {
        sort(tool(&agent, krate, pinned, releases)?);
    }
    let mut seen = BTreeSet::new();
    for (_, text) in workflows(root)? {
        for uses in text.lines().filter_map(parse_uses) {
            let repo = repository(&uses.action)?;
            if seen.insert((repo.clone(), uses.reference.clone(), uses.version.clone())) {
                sort(action(&agent, &repo, &uses, releases)?);
            }
        }
    }
    if let Some(d) = nightly(today()).filter(|_| releases) {
        due.push(d);
    }
    Ok((due, waiting))
}

/// What became of one pinned version.
enum Found {
    Current,
    Due(Due),
    /// A release newer by a month, with nothing to update to yet: why.
    Waiting(String),
}

fn tool(agent: &ureq::Agent, krate: &str, pinned: &str, releases: bool) -> Result<Found, String> {
    let vulns = vulnerabilities(agent, krate, pinned)?;
    if vulns.is_empty() && !releases {
        return Ok(Found::Current);
    }
    let versions = versions(agent, krate)?;
    judge(agent, (Kind::Tool, krate), pinned, &versions, &vulns, releases)
}

/// Whether `name` at `pinned` is due, from its releases (number, day, yanked) and the known
/// vulnerabilities of `pinned`.
fn judge(
    agent: &ureq::Agent,
    (kind, name): (Kind, &str),
    pinned: &str,
    versions: &[(String, i64, bool)],
    vulns: &[String],
    releases: bool,
) -> Result<Found, String> {
    let Some(pinned_day) = versions.iter().find(|v| v.0 == pinned).map(|v| v.1) else {
        return Err(format!("{name} {pinned} is not among its releases"));
    };
    let newest = versions
        .iter()
        .filter(|(num, _, yanked)| !yanked && !num.contains(['-', '+']))
        .filter(|(num, _, _)| semver(num) > semver(pinned))
        .max_by_key(|(num, _, _)| semver(num));
    let late = newest.filter(|(_, day, _)| releases && day - pinned_day >= MONTH);
    if vulns.is_empty() && late.is_none() {
        return Ok(Found::Current);
    }
    let to = candidate(agent, kind, name, pinned, versions, today())?;
    let due = |why: String, urgent: bool| {
        Found::Due(Due {
            kind,
            what: name.to_owned(),
            from: pinned.to_owned(),
            to: to.clone(),
            commit: None,
            why,
            urgent,
        })
    };
    if !vulns.is_empty() {
        return Ok(due(format!("known vulnerability: {}", vulns.join(", ")), true));
    }
    let Some((num, day, _)) = late else { return Ok(Found::Current) };
    let why = format!("{num} released {} days after {pinned}", day - pinned_day);
    Ok(match to {
        Some(_) => due(why, false),
        None => Found::Waiting(format!(
            "{name}: {why}, but nothing to update to yet: no newer release is {WAIT} days old, \
             unyanked, plain x.y.z and without a known vulnerability"
        )),
    })
}

/// The version to update `name` to: the newest released at least `WAIT` days ago, not
/// yanked, plain `x.y.z` (it is written into the source) and without a known
/// vulnerability.
fn candidate(
    agent: &ureq::Agent,
    kind: Kind,
    name: &str,
    pinned: &str,
    versions: &[(String, i64, bool)],
    today: i64,
) -> Result<Option<String>, String> {
    let mut fit: Vec<&String> = versions
        .iter()
        .filter(|(num, day, yanked)| {
            !yanked && today - day >= WAIT && plain(num) && semver(num) > semver(pinned)
        })
        .map(|(num, _, _)| num)
        .collect();
    fit.sort_by_key(|num| std::cmp::Reverse(semver(num)));
    for num in fit {
        let vulns = match kind {
            Kind::Action => action_vulnerabilities(agent, name, num)?,
            _ => vulnerabilities(agent, name, num)?,
        };
        if vulns.is_empty() {
            return Ok(Some(num.clone()));
        }
    }
    Ok(None)
}

/// Whether `version` is three numbers and nothing else.
fn plain(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn nightly(today: i64) -> Option<Due> {
    let date = NIGHTLY.strip_prefix("nightly-")?;
    let age = today - day(date)?;
    // setup fails if this one lacks a component, and then an earlier date will do.
    let to = civil(today - SETTLED);
    (age >= MONTH).then(|| Due {
        kind: Kind::Nightly,
        what: "nightly".to_owned(),
        from: NIGHTLY.to_owned(),
        to: Some(format!("nightly-{to}")),
        commit: None,
        why: format!("{age} days old"),
        urgent: false,
    })
}

/// Installs the nightly `to`, or else the latest of the `SETTLED` days before it that has
/// every component the tasks need; which one it installed.
fn settle(to: &str) -> Result<String, String> {
    let first = to.strip_prefix("nightly-").and_then(day).ok_or(format!("not a nightly: {to}"))?;
    let components = crate::setup::nightly_needs();
    for back in 0..=SETTLED {
        let name = format!("nightly-{}", civil(first - back));
        if crate::setup::install_toolchain(&name, &components).status().is_ok_and(|s| s.success()) {
            return Ok(name);
        }
    }
    Err(format!(
        "no nightly from {to} back {SETTLED} days could be installed with {}; the pinned one stays",
        components.into_iter().collect::<Vec<_>>().join(", ")
    ))
}

/// Every version of `krate` on crates.io: number, day of release, yanked.
fn versions(agent: &ureq::Agent, krate: &str) -> Result<Vec<(String, i64, bool)>, String> {
    let base = format!("https://crates.io/api/v1/crates/{krate}/versions");
    let mut url = format!("{base}?per_page=100");
    let mut out = Vec::new();
    for _ in 0..20 {
        let page = get_json(agent, &url)?;
        for v in page["versions"].as_array().into_iter().flatten() {
            let (Some(num), Some(created)) = (v["num"].as_str(), v["created_at"].as_str()) else {
                continue;
            };
            let Some(day) = day(created) else { continue };
            out.push((num.to_owned(), day, v["yanked"].as_bool().unwrap_or(false)));
        }
        match page["meta"]["next_page"].as_str() {
            Some(next) => url = format!("{base}{next}"),
            None => break,
        }
    }
    Ok(out)
}

/// The known vulnerabilities of `krate` at `version`, from OSV, which carries the `RustSec` advisories.
fn vulnerabilities(agent: &ureq::Agent, krate: &str, version: &str) -> Result<Vec<String>, String> {
    let query = format!(
        r#"{{"package":{{"name":{},"ecosystem":{}}},"version":{}}}"#,
        quoted(krate),
        quoted(CRATES),
        quoted(version)
    );
    advisory_ids(&post_json(agent, "https://api.osv.dev/v1/query", &query)?)
}

/// A `uses:` of a workflow, as written there.
#[derive(Debug, PartialEq, Eq)]
struct Uses {
    /// What comes before the action, kept when the line is written again.
    head: String,
    /// `owner/repo`, or a path in it.
    action: String,
    /// What comes after `@`: a commit, a tag or a branch.
    reference: String,
    /// The release in a `# vX.Y.Z` after it, without the `v`.
    version: Option<String>,
}

/// The `uses:` on `line`, if it has one of an action from a repository (not `./` nor
/// `docker://`).
fn parse_uses(line: &str) -> Option<Uses> {
    let at = line.find("uses:")?;
    let before = &line[..at];
    if !before.trim_start().trim_start_matches('-').trim().is_empty() {
        return None;
    }
    let rest = &line[at + "uses:".len()..];
    let (value, comment) = rest.split_once('#').map_or((rest, None), |(v, c)| (v, Some(c)));
    let value = value.trim().trim_matches(['"', '\'']);
    if value.starts_with("./") || value.starts_with("docker://") {
        return None;
    }
    let (action, reference) = value.rsplit_once('@')?;
    let version = comment
        .and_then(|c| c.split_whitespace().next())
        .and_then(|v| v.strip_prefix('v'))
        .filter(|v| plain(v))
        .map(str::to_owned);
    Some(Uses {
        head: format!("{before}uses: "),
        action: action.to_owned(),
        reference: reference.to_owned(),
        version,
    })
}

/// The `owner/repo` of an action, checked to be no more than a name: it goes into URLs.
fn repository(action: &str) -> Result<String, String> {
    let name = |p: &str| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    let parts: Vec<&str> = action.split('/').collect();
    match parts.as_slice() {
        [owner, repo, ..] if name(owner) && name(repo) && !repo.starts_with('.') => {
            Ok(format!("{owner}/{repo}"))
        }
        _ => Err(format!("not an action's name: {}", printable(action))),
    }
}

/// Whether `reference` is a full commit hash.
fn is_commit(reference: &str) -> bool {
    reference.len() == 40
        && reference.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The workflow files, and their text.
fn workflows(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let dir = root.join(WORKFLOWS);
    let Ok(entries) = fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            fs::read_to_string(&p)
                .map(|t| (p.clone(), t))
                .map_err(|e| format!("{}: {e}", p.display()))
        })
        .collect()
}

fn action(agent: &ureq::Agent, repo: &str, uses: &Uses, releases: bool) -> Result<Found, String> {
    let due = |from: String, to: Option<String>, why: String, urgent: bool| Due {
        kind: Kind::Action,
        what: repo.to_owned(),
        from,
        to,
        commit: None,
        why,
        urgent,
    };
    let mut found = if is_commit(&uses.reference) {
        let Some(version) = &uses.version else {
            return Err(format!(
                "{repo}@{}: pinned with no `# vX.Y.Z` after it to say which release",
                uses.reference
            ));
        };
        let tagged = commit_of(agent, repo, version)?;
        if tagged != uses.reference {
            let why = format!(
                "tag v{version} is now commit {tagged}, not the pinned {}: find out why before \
                 updating",
                uses.reference
            );
            return Ok(Found::Due(due(version.clone(), None, why, true)));
        }
        let vulns = action_vulnerabilities(agent, repo, version)?;
        if vulns.is_empty() && !releases {
            return Ok(Found::Current);
        }
        let versions = action_releases(agent, repo)?;
        judge(agent, (Kind::Action, repo), version, &versions, &vulns, releases)?
    } else {
        let versions = action_releases(agent, repo)?;
        let to = candidate(agent, Kind::Action, repo, "0.0.0", &versions, today())?;
        let why = "not pinned to a commit".to_owned();
        Found::Due(due(uses.reference.clone(), to, why, false))
    };
    if let Found::Due(d) = &mut found
        && let Some(to) = &d.to
    {
        d.commit = Some(commit_of(agent, repo, to)?);
    }
    Ok(found)
}

/// The releases of an action on GitHub with a tag `vX.Y.Z`, newest first: number, day of
/// release, and false (none is yanked).
fn action_releases(agent: &ureq::Agent, repo: &str) -> Result<Vec<(String, i64, bool)>, String> {
    let reply =
        github(agent, &format!("https://api.github.com/repos/{repo}/releases?per_page=100"))?;
    let list = reply.as_array().ok_or(format!("{repo}: the list of releases is not a list"))?;
    Ok(list
        .iter()
        .filter(|r| r["draft"].as_bool() == Some(false) && r["prerelease"].as_bool() == Some(false))
        .filter_map(|r| {
            let num = r["tag_name"].as_str()?.strip_prefix('v').filter(|v| plain(v))?;
            Some((num.to_owned(), day(r["published_at"].as_str()?)?, false))
        })
        .collect())
}

/// The commit that the tag `v<version>` of an action names.
fn commit_of(agent: &ureq::Agent, repo: &str, version: &str) -> Result<String, String> {
    let reply = github(agent, &format!("https://api.github.com/repos/{repo}/commits/v{version}"))?;
    match reply["sha"].as_str() {
        Some(sha) if is_commit(sha) => Ok(sha.to_owned()),
        _ => Err(format!("{repo}: no commit for v{version}")),
    }
}

/// The known vulnerabilities of an action at `version`, from OSV, which carries GitHub's
/// advisories. OSV does not order an action's versions, so a query by version finds nothing:
/// every advisory of the action is read, and its ranges compared here.
fn action_vulnerabilities(
    agent: &ureq::Agent,
    repo: &str,
    version: &str,
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut token = String::new();
    for _ in 0..20 {
        let page = if token.is_empty() {
            String::new()
        } else {
            format!(r#","page_token":{}"#, quoted(&token))
        };
        let query = format!(
            r#"{{"package":{{"name":{},"ecosystem":{}}}{page}}}"#,
            quoted(repo),
            quoted(ACTIONS)
        );
        let reply = post_json(agent, "https://api.osv.dev/v1/query", &query)?;
        for advisory in advisories(&reply)? {
            if affects_version(advisory, repo, version)? {
                ids.push(printable(advisory["id"].as_str().ok_or("an OSV advisory has no id")?));
            }
        }
        match reply["next_page_token"].as_str() {
            Some(next) if !next.is_empty() => next.clone_into(&mut token),
            _ => return Ok(ids),
        }
    }
    Err(format!("{repo}: OSV kept giving more pages"))
}

/// Whether an OSV advisory covers `repo` at `version`, by its ranges of plain versions. A
/// range of another kind is an error rather than a guess.
fn affects_version(advisory: &Value, repo: &str, version: &str) -> Result<bool, String> {
    if advisory["withdrawn"].as_str().is_some() {
        return Ok(false);
    }
    let id = advisory["id"].as_str().unwrap_or("?");
    let wanted = semver(version);
    for affected in advisory["affected"].as_array().into_iter().flatten() {
        let package = &affected["package"];
        let ours = package["ecosystem"].as_str() == Some(ACTIONS)
            && package["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(repo));
        if !ours {
            continue;
        }
        if affected["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v.as_str().is_some_and(|v| semver(v.trim_start_matches('v')) == wanted))
        {
            return Ok(true);
        }
        for range in affected["ranges"].as_array().into_iter().flatten() {
            if !matches!(range["type"].as_str(), Some("ECOSYSTEM" | "SEMVER")) {
                return Err(format!(
                    "{}: a range OSV gives that is not of versions",
                    printable(id)
                ));
            }
            let mut inside = false;
            for event in range["events"].as_array().into_iter().flatten() {
                let at = |key: &str| event[key].as_str().map(|v| semver(v.trim_start_matches('v')));
                if let Some(start) = at("introduced") {
                    inside = wanted >= start;
                } else if let Some(end) = at("fixed") {
                    if inside && wanted < end {
                        return Ok(true);
                    }
                    inside = false;
                } else if let Some(end) = at("last_affected") {
                    if inside && wanted <= end {
                        return Ok(true);
                    }
                    inside = false;
                } else if event["limit"].as_str().is_none() {
                    return Err(format!("{}: an event of a range not known here", printable(id)));
                }
            }
            if inside {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// Per project: ureq, from the dependencies; without an HTTP crate there, the three below are
// replaced by reference/variants/curl-agent.rs.
fn get_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut resp = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

fn post_json(agent: &ureq::Agent, url: &str, body: &str) -> Result<Value, String> {
    let mut resp = agent
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
        .send(body)
        .map_err(|e| format!("{url}: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

/// GitHub's API at `url`, with the token in `GITHUB_TOKEN` if there is one (in CI, where
/// runners share addresses and the limit without one, 60 requests an hour, is soon reached).
fn github(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut request = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = github_token()? {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let mut resp = request.call().map_err(|e| format!("{url}: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

fn ask(question: &str) -> bool {
    print!("{} [y/N] ", printable(question));
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

/// Whether `t` is a bearer token as RFC 6750 writes one (b64token): letters, digits and
/// `-._~+/`, then `=` alone. GitHub's tokens differ by kind (the one Actions gives a job was
/// refused by a check for letters, digits and `_`), so the check is the header's grammar, not
/// a guess at their shape: what passes it cannot end the header or the line of curl's
/// configuration it goes into.
fn is_token(t: &str) -> bool {
    let body = t.trim_end_matches('=');
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
}

/// `GITHUB_TOKEN`, if set, checked to be a token: it goes into a header.
fn github_token() -> Result<Option<String>, String> {
    match std::env::var("GITHUB_TOKEN") {
        Ok(t) if t.is_empty() => Ok(None),
        Ok(t) if is_token(&t) => Ok(Some(t)),
        Ok(_) => Err("GITHUB_TOKEN is not a token".to_owned()),
        Err(_) => Ok(None),
    }
}

/// Writes `to` for the update `d`: into the constants of `xtask/src/pins.rs`, or for an
/// action, into every `uses:` of it in the workflows.
fn apply(root: &Path, d: &Due, to: &str) -> Result<(), String> {
    if d.kind == Kind::Action {
        let commit = d.commit.as_deref().ok_or(format!("{}: no commit for {to}", d.what))?;
        return apply_action(root, d, to, commit);
    }
    let path = root.join("xtask").join("src").join("pins.rs");
    let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (old, new) = if d.kind == Kind::Nightly {
        (
            format!("const NIGHTLY: &str = \"{}\";", d.from),
            format!("const NIGHTLY: &str = \"{to}\";"),
        )
    } else {
        (format!("(\"{}\", \"{}\",", d.what, d.from), format!("(\"{}\", \"{to}\",", d.what))
    };
    if source.matches(&old).count() != 1 {
        return Err(format!("cannot find `{old}` once in {}", path.display()));
    }
    fs::write(&path, source.replace(&old, &new)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes `uses: <action>@<commit> # v<to>` for each `uses:` of the action `d` names at the
/// version it is updated from.
fn apply_action(root: &Path, d: &Due, to: &str, commit: &str) -> Result<(), String> {
    let mut written = 0;
    for (path, text) in workflows(root)? {
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        for line in text.split_inclusive('\n') {
            let end = &line[line.trim_end_matches(['\r', '\n']).len()..];
            match parse_uses(line.trim_end_matches(['\r', '\n'])) {
                Some(u)
                    if repository(&u.action).is_ok_and(|r| r == d.what)
                        && (u.reference == d.from || u.version.as_deref() == Some(&d.from)) =>
                {
                    let _ = write!(out, "{}{}@{commit} # v{to}{end}", u.head, u.action);
                    changed = true;
                }
                _ => out.push_str(line),
            }
        }
        if changed {
            fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))?;
            written += 1;
        }
    }
    if written == 0 {
        return Err(format!("no `uses: {}` at {} in {WORKFLOWS}/", d.what, d.from));
    }
    Ok(())
}

/// `1.2.3` as numbers, for ordering releases.
fn semver(version: &str) -> Vec<u64> {
    version
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Days since 1970-01-01 of a date that starts `YYYY-MM-DD`.
pub fn day(text: &str) -> Option<i64> {
    let mut parts = text.get(..10)?.split('-').map(str::parse::<i64>);
    let (y, m, d) = (parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `YYYY-MM-DD` of a day since 1970-01-01 (`civil_from_days`).
pub fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn today() -> i64 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    fn reply(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn what_comes_from_a_server_cannot_act_on_the_terminal() {
        assert_eq!(super::printable("RUSTSEC\u{1b}[2J\u{9b}\u{202e}-1 é文"), "RUSTSEC?[2J??-1 é文");
    }

    #[test]
    fn an_osv_reply_is_read_only_in_its_known_shape() {
        let ids = |text: &str| super::advisory_ids(&reply(text));
        assert_eq!(ids("{}"), Ok(Vec::new()));
        assert_eq!(ids(r#"{"vulns":[{"id":"RUSTSEC-1"}]}"#), Ok(vec!["RUSTSEC-1".to_owned()]));
        assert_eq!(ids(r#"{"vulns":[]}"#), Ok(Vec::new()));
        for bad in
            ["[]", "null", r#"{"vulns":{}}"#, r#"{"vulns":[{}]}"#, r#"{"next_page_token":"x"}"#]
        {
            assert!(ids(bad).is_err(), "{bad}");
        }
    }

    use super::*;

    #[test]
    fn tokens_are_what_a_header_can_carry() {
        assert!(is_token("ghp_0123456789abcdefABCDEF"));
        assert!(is_token("ghs_abc.def-ghi_jkl~mno+pq/rs=="));
        for t in ["", "==", "a b", "a\"b", "a\\b", "a\nb", "a=b", "a:b", "é"] {
            assert!(!is_token(t), "{t:?}");
        }
    }

    #[test]
    fn json_strings_are_quoted() {
        assert_eq!(quoted("a\"b\\c\n"), r#""a\"b\\c\u000a""#);
        assert_eq!(reply(&quoted("x\u{1}\"")).as_str(), Some("x\u{1}\""));
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(day("1970-01-01"), Some(0));
        assert_eq!(day("2026-10-28T12:00:00+00:00"), Some(20_754));
        for d in [0, 59, 60, 365, 11_016, 20_754, 30_000] {
            assert_eq!(day(&civil(d)), Some(d), "{d}");
        }
    }

    #[test]
    fn releases_order_by_number() {
        assert!(semver("0.13.10") > semver("0.13.9"));
        assert!(semver("1.0.0") > semver("0.99.99"));
        assert_eq!(semver("0.20.2-rc.1"), semver("0.20.2"));
    }

    #[test]
    fn only_plain_releases_are_updated_to() {
        assert!(plain("0.20.2"));
        assert!(!plain("0.20.2-rc.1"));
        assert!(!plain("0.20.2+meta"));
        assert!(!plain("0.20"));
        assert!(!plain("0..2"));
        assert!(!plain("0.20.2\"); evil(\""));
    }

    fn due(kind: Kind, what: &str, from: &str) -> Due {
        Due {
            kind,
            what: what.to_owned(),
            from: from.to_owned(),
            to: None,
            commit: None,
            why: String::new(),
            urgent: false,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn updates_are_written_into_the_constants() {
        let root = scratch("apply");
        let src = root.join("xtask").join("src");
        fs::create_dir_all(&src).unwrap();
        let before = "pub const NIGHTLY: &str = \"nightly-2026-01-01\";\n    (\"cargo-deny\", \"0.1.0\", \"deny\"),\n";
        fs::write(src.join("pins.rs"), before).unwrap();
        apply(&root, &due(Kind::Nightly, "nightly", "nightly-2026-01-01"), "nightly-2026-03-01")
            .unwrap();
        apply(&root, &due(Kind::Tool, "cargo-deny", "0.1.0"), "0.2.0").unwrap();
        let after = fs::read_to_string(src.join("pins.rs")).unwrap();
        assert_eq!(after, before.replace("2026-01-01", "2026-03-01").replace("0.1.0", "0.2.0"));
        assert!(apply(&root, &due(Kind::Tool, "cargo-vet", "9.9.9"), "10.0.0").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_nightly_is_due_after_a_month() {
        let pinned = day(NIGHTLY.trim_start_matches("nightly-")).unwrap_or(0);
        assert!(nightly(pinned + MONTH - 1).is_none());
        let due = nightly(pinned + MONTH).and_then(|d| d.to);
        assert_eq!(due, Some(format!("nightly-{}", civil(pinned + MONTH - SETTLED))));
    }

    const SHA: &str = "08eba0b27e820071cde6df949e0beb9ba4906955";

    #[test]
    fn uses_lines_are_read_as_written() {
        let pinned = parse_uses(&format!("      - uses: actions/checkout@{SHA} # v4.2.2")).unwrap();
        assert_eq!(pinned.head, "      - uses: ");
        assert_eq!((pinned.action.as_str(), pinned.reference.as_str()), ("actions/checkout", SHA));
        assert_eq!(pinned.version.as_deref(), Some("4.2.2"));
        let tag = parse_uses("        uses: \"actions/cache/save@v4\"").unwrap();
        assert_eq!(
            (tag.action.as_str(), tag.reference.as_str(), tag.version),
            ("actions/cache/save", "v4", None)
        );
        assert_eq!(repository(&tag.action), Ok("actions/cache".to_owned()));
        for not in [
            "      - uses: ./local",
            "  uses: docker://alpine:3",
            "  run: echo uses: a/b@c",
            "  # x",
        ] {
            assert_eq!(parse_uses(not), None, "{not}");
        }
        assert!(parse_uses(&format!("  - uses: a/b@{SHA} # 4.2.2")).unwrap().version.is_none());
        assert!(repository("a").is_err());
        assert!(repository("a/../b").is_err());
        assert!(repository("a/b?x=1").is_err());
        assert!(is_commit(SHA) && !is_commit("v4") && !is_commit(&SHA.to_uppercase()));
    }

    #[test]
    fn actions_are_updated_where_the_workflows_use_them() {
        let root = scratch("actions");
        let dir = root.join(WORKFLOWS);
        fs::create_dir_all(&dir).unwrap();
        let before = format!(
            "steps:\n  - uses: actions/checkout@{SHA} # v4.2.2\n    with:\n      x: 1\n  - uses: actions/checkout/sub@v4\r\n  - uses: actions/cache@v4\n"
        );
        fs::write(dir.join("a.yml"), &before).unwrap();
        fs::write(dir.join("b.txt"), &before).unwrap();
        let new = "1111111111111111111111111111111111111111";
        let mut d = due(Kind::Action, "actions/checkout", "4.2.2");
        d.commit = Some(new.to_owned());
        apply(&root, &d, "5.0.0").unwrap();
        let mut d = due(Kind::Action, "actions/checkout", "v4");
        d.commit = Some(new.to_owned());
        apply(&root, &d, "5.0.0").unwrap();
        let after = fs::read_to_string(dir.join("a.yml")).unwrap();
        assert_eq!(
            after,
            format!(
                "steps:\n  - uses: actions/checkout@{new} # v5.0.0\n    with:\n      x: 1\n  - uses: actions/checkout/sub@{new} # v5.0.0\r\n  - uses: actions/cache@v4\n"
            )
        );
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), before, "not a workflow");
        assert!(apply(&root, &d, "6.0.0").is_err(), "nothing at v4 any more");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_actions_advisory_covers_its_ranges() {
        let advisory = |ranges: &str| {
            reply(&format!(
                r#"{{"id":"GHSA-1","affected":[{{"package":{{"name":"Tj-Actions/changed-files","ecosystem":"GitHub Actions"}},"ranges":[{ranges}]}}]}}"#
            ))
        };
        let hit = |a: &Value, v: &str| affects_version(a, "tj-actions/changed-files", v);
        let fixed =
            advisory(r#"{"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"46.0.1"}]}"#);
        assert_eq!(hit(&fixed, "45.0.7"), Ok(true));
        assert_eq!(hit(&fixed, "46.0.1"), Ok(false));
        let last = advisory(
            r#"{"type":"ECOSYSTEM","events":[{"introduced":"2.0.0"},{"last_affected":"2.1.0"}]}"#,
        );
        assert_eq!(hit(&last, "1.9.9"), Ok(false));
        assert_eq!(hit(&last, "2.1.0"), Ok(true));
        assert_eq!(hit(&last, "2.1.1"), Ok(false));
        let open = advisory(r#"{"type":"ECOSYSTEM","events":[{"introduced":"3.0.0"}]}"#);
        assert_eq!(hit(&open, "9.0.0"), Ok(true));
        assert!(hit(&advisory(r#"{"type":"GIT","events":[]}"#), "1.0.0").is_err());
        let other = reply(
            r#"{"id":"GHSA-2","affected":[{"package":{"name":"a/b","ecosystem":"GitHub Actions"},"ranges":[{"type":"ECOSYSTEM","events":[{"introduced":"0"}]}]}]}"#,
        );
        assert_eq!(hit(&other, "1.0.0"), Ok(false));
        let gone = reply(r#"{"id":"GHSA-3","withdrawn":"2026-01-01T00:00:00Z","affected":[]}"#);
        assert_eq!(hit(&gone, "1.0.0"), Ok(false));
    }
}
