use std::path::Path;

use crate::run::{cargo, status};

pub fn run(root: &Path) -> Result<(), String> {
    // Locked: cargo vet would otherwise update supply-chain/imports.lock after it passes.
    status(cargo(root, &["vet", "--locked"]))
}
