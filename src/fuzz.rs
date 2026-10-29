//! Entry points for the fuzz targets in `fuzz/` into parsers that are not public. Built
//! only by `cargo fuzz`, which sets `--cfg fuzzing`.

use std::collections::BTreeMap;

use crate::rclone::{obscure, ssh_passwords, system_ssh};

pub use crate::ui::panes::wrap;

pub fn remote_colon(input: &str) -> Option<usize> {
    crate::location::remote_colon(input)
}

pub fn split_params(name: &str) -> Vec<(String, String)> {
    system_ssh::split_params(name)
}

pub fn conn_quote(value: &str) -> String {
    system_ssh::conn_quote(value)
}

pub fn ssh_dest(params: &BTreeMap<String, String>, askpass: bool) -> Result<Vec<String>, String> {
    system_ssh::ssh_dest(params, askpass)
}

/// The options every transfer's ssh command carries.
pub const TRANSFER_ONLY: [&str; 12] = system_ssh::TRANSFER_ONLY;

pub fn ssh_command(control: &[String], dest: &[String]) -> String {
    system_ssh::ssh_command(control, dest)
}

pub fn ssh_fs_name(name: &str, params: &BTreeMap<String, String>, cmd: &str) -> String {
    system_ssh::ssh_fs_name(name, params, cmd)
}

pub fn openssh_version(banner: &str) -> Option<(u32, u32)> {
    system_ssh::openssh_version(banner)
}

pub fn base64_url_encode(data: &[u8]) -> String {
    obscure::base64_url_encode(data)
}

pub fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    obscure::base64_url_decode(s)
}

pub fn reveal(obscured: &str) -> Result<String, String> {
    obscure::reveal(obscured)
}

pub fn password_entry(line: &str) -> Option<(&str, &str)> {
    ssh_passwords::entry(line)
}
