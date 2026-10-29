//! `cargo xtask outdated`: whether the pinned tools and nightly toolchain have fallen
//! behind, and updating them if the user agrees.
//!
//! Only releases count, not pre-releases or yanked ones, and only once the newest was
//! released at least `MONTH` days after the pinned one, so that a tool released daily
//! does not ask every day; and only when there is a version to update to: released at
//! least `WAIT` days ago, not yanked, plain `x.y.z` and without a known vulnerability. A
//! known vulnerability in the pinned version makes it due at once, even with nothing to
//! update to yet. The nightly toolchain is due once its date is `MONTH` days old.
//!
//! Other tasks look by themselves: for vulnerabilities daily, for releases weekly
//! (`RUST_XTASK_VULNS_EVERY`, `RUST_XTASK_RELEASES_EVERY`, in days; 0 is every run).

use std::fmt::Write as _;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use serde_json::{Value, json};

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
const USER_AGENT: &str = "rext-xtask (+https://github.com/lutianyu2001/rclone-explorer-tui)";

/// An update that is due.
struct Due {
    /// The crate, or `nightly`.
    what: String,
    from: String,
    /// None for a known vulnerability with no version to update to yet.
    to: Option<String>,
    why: String,
}

impl std::fmt::Display for Due {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let to = self.to.as_deref().unwrap_or("nothing to update to yet");
        write!(f, "{}: {} -> {to} ({})", self.what, self.from, self.why)
    }
}

/// `cargo xtask outdated`. Exits with 2 when updates are due and it cannot ask, so that
/// CI shows it, and when a pinned version has a known vulnerability and nothing to update
/// to yet, whether it asked or not.
/// With `--vulns`, only known vulnerabilities, for a daily check.
pub fn main(root: &Path, args: &[String]) -> ExitCode {
    let releases = match args {
        [] => true,
        [flag] if flag == "--vulns" => false,
        _ => {
            eprintln!("xtask: usage: cargo xtask outdated [--vulns]");
            return ExitCode::FAILURE;
        }
    };
    let (due, waiting) = match due(Duration::from_secs(30), releases) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = remember_check(root, &due, releases);
    for w in &waiting {
        println!("{w}");
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
        println!("{d}");
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
        Ok(0) => {}
        Ok(_) => {
            println!("Pinned in xtask/src/pins.rs. Installing them:");
            // A new run, built from the edited source.
            let installed =
                Command::new("cargo").args(["xtask", "setup"]).current_dir(root).status();
            if !installed.is_ok_and(|s| s.success()) {
                return ExitCode::FAILURE;
            }
            println!(
                "Now commit xtask/src/pins.rs, run `cargo xtask all --if-needed`, fix what the \
                 new versions find, and commit the results."
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
/// so that a later one failing keeps the earlier; how many it wrote.
fn update(root: &Path, due: &[Due]) -> Result<usize, String> {
    let mut written = 0;
    for d in due {
        let Some(to) = &d.to else { continue };
        if !ask(&format!("Update {} {} -> {to}?", d.what, d.from)) {
            continue;
        }
        let to = if d.what == "nightly" {
            match settle(to) {
                Ok(to) => to,
                Err(e) => {
                    eprintln!("xtask: {e}");
                    continue;
                }
            }
        } else {
            to.clone()
        };
        apply(root, d, &to)?;
        written += 1;
    }
    Ok(written)
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
    let due = due(Duration::from_secs(10), releases).map(|f| f.0).unwrap_or_default();
    let _ = remember_check(root, &due, releases);
    if !due.is_empty() {
        let names: Vec<&str> = due.iter().map(|d| d.what.as_str()).collect();
        eprintln!(
            "xtask: newer versions are due for {}; `cargo xtask outdated` shows them and can update",
            names.join(", ")
        );
    }
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

/// What is due: known vulnerabilities, and with `releases` also newer releases; and
/// newer releases with nothing to update to yet.
fn due(timeout: Duration, releases: bool) -> Result<(Vec<Due>, Vec<String>), String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
    let (mut due, mut waiting) = (Vec::new(), Vec::new());
    for (krate, pinned, _) in TOOLS {
        match tool(&agent, krate, pinned, releases)? {
            Found::Current => {}
            Found::Due(d) => due.push(d),
            Found::Waiting(why) => waiting.push(why),
        }
    }
    if let Some(d) = nightly(today()).filter(|_| releases) {
        due.push(d);
    }
    Ok((due, waiting))
}

/// What became of one pinned tool.
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
    let Some(pinned_day) = versions.iter().find(|v| v.0 == pinned).map(|v| v.1) else {
        return Err(format!("{krate} {pinned} is not on crates.io"));
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
    let to = candidate(agent, krate, pinned, &versions, today())?;
    let due = |why: String| {
        Found::Due(Due { what: krate.to_owned(), from: pinned.to_owned(), to: to.clone(), why })
    };
    if !vulns.is_empty() {
        return Ok(due(format!("known vulnerability: {}", vulns.join(", "))));
    }
    let Some((num, day, _)) = late else { return Ok(Found::Current) };
    let why = format!("{num} released {} days after {pinned}", day - pinned_day);
    Ok(match to {
        Some(_) => due(why),
        None => Found::Waiting(format!(
            "{krate}: {why}, but nothing to update to yet: no newer release is {WAIT} days old, \
             unyanked, plain x.y.z and without a known vulnerability"
        )),
    })
}

/// The version to update `krate` to: the newest released at least `WAIT` days ago, not
/// yanked, plain `x.y.z` (it is written into the source) and without a known
/// vulnerability.
fn candidate(
    agent: &ureq::Agent,
    krate: &str,
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
        if vulnerabilities(agent, krate, num)?.is_empty() {
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
        what: "nightly".to_owned(),
        from: NIGHTLY.to_owned(),
        to: Some(format!("nightly-{to}")),
        why: format!("{age} days old"),
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
    let query = json!({"package": {"name": krate, "ecosystem": "crates.io"}, "version": version});
    let url = "https://api.osv.dev/v1/query";
    let mut resp = agent
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
        .send(query.to_string())
        .map_err(|e| format!("{url}: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    let reply: Value = serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))?;
    Ok(reply["vulns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["id"].as_str().map(str::to_owned))
        .collect())
}

fn get_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut resp = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

/// Writes `to` for the update `d` into the constants of `xtask/src/pins.rs`.
fn apply(root: &Path, d: &Due, to: &str) -> Result<(), String> {
    let path = root.join("xtask").join("src").join("pins.rs");
    let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (old, new) = if d.what == "nightly" {
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
    use super::*;

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

    #[test]
    fn updates_are_written_into_the_constants() {
        let root = std::env::temp_dir().join(format!("xtask-apply-{}", std::process::id()));
        let src = root.join("xtask").join("src");
        fs::create_dir_all(&src).unwrap();
        let before = "pub const NIGHTLY: &str = \"nightly-2026-01-01\";\n    (\"cargo-deny\", \"0.1.0\", \"deny\"),\n";
        fs::write(src.join("pins.rs"), before).unwrap();
        let due = |what: &str, from: &str| Due {
            what: what.to_owned(),
            from: from.to_owned(),
            to: None,
            why: String::new(),
        };
        apply(&root, &due("nightly", "nightly-2026-01-01"), "nightly-2026-03-01").unwrap();
        apply(&root, &due("cargo-deny", "0.1.0"), "0.2.0").unwrap();
        let after = fs::read_to_string(src.join("pins.rs")).unwrap();
        assert_eq!(after, before.replace("2026-01-01", "2026-03-01").replace("0.1.0", "0.2.0"));
        assert!(apply(&root, &due("cargo-vet", "9.9.9"), "10.0.0").is_err(), "not pinned");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_nightly_is_due_after_a_month() {
        let pinned = day(NIGHTLY.trim_start_matches("nightly-")).unwrap_or(0);
        assert!(nightly(pinned + MONTH - 1).is_none());
        let due = nightly(pinned + MONTH).and_then(|d| d.to);
        assert_eq!(due, Some(format!("nightly-{}", civil(pinned + MONTH - SETTLED))));
    }
}
