use std::fs;
use std::path::Path;

use crate::record;
use crate::run::{capped, cargo, shown, status};
use crate::tasks::limit;

pub fn run(root: &Path, dir: &Path) -> Result<(), String> {
    // geiger reads the dependencies' sources, which a build without vendor/ has fetched
    // only for the host's targets.
    status(cargo(root, &["fetch", "--locked"]))?;
    let path = dir.join("geiger.txt");
    let file = fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // geiger cleans before it builds; in target/debug that would take everyone's build.
    let mut scan = cargo(root, &["geiger", "--locked", "--offline", "--output-format", "Ascii"]);
    scan.env("CARGO_TARGET_DIR", root.join("target/geiger"));
    let mut cmd = capped(scan, &limit("geiger"));
    record::say(&format!("$ {} > geiger.txt", shown(&cmd)));
    // geiger exits non-zero when files it was shown are not Rust (READMEs, included
    // text), so the table itself tells whether it ran.
    let _ = record::exec(&mut cmd, Some(file));
    let table = fs::read_to_string(&path).unwrap_or_default();
    if table.contains("Metric output format") {
        Ok(())
    } else {
        Err("cargo geiger wrote no table".to_owned())
    }
}
