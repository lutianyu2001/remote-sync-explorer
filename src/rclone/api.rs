//! Typed wrappers for the rc methods remote-sync-explorer uses.

use serde::Deserialize;
use serde_json::{Value, json};

use super::backend::{Backend, RpcError};

/// One entry of `operations/list`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ListItem {
    pub name: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub mod_time: String,
    #[serde(default)]
    pub is_dir: bool,
}

#[derive(Deserialize)]
struct ListReply {
    #[serde(default)]
    list: Vec<ListItem>,
}

/// `job/status`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JobStatus {
    pub id: i64,
    pub finished: bool,
    pub success: bool,
    #[serde(default)]
    pub error: String,
}

/// One file in `core/stats`' `transferring` list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Transferring {
    pub name: String,
    #[serde(default)]
    pub bytes: i64,
    #[serde(default)]
    pub size: i64,
}

/// `core/stats` for one group.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    #[serde(default)]
    pub bytes: i64,
    #[serde(default)]
    pub total_bytes: i64,
    #[serde(default)]
    pub speed: f64,
    #[serde(default)]
    pub eta: Option<f64>,
    #[serde(default)]
    pub transfers: i64,
    #[serde(default)]
    pub total_transfers: i64,
    #[serde(default)]
    pub errors: i64,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub transferring: Vec<Transferring>,
}

fn decode<T: for<'de> Deserialize<'de>>(method: &str, v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| {
        RpcError::new(0, tr!("rclone-unexpected-reply", method = method, error = e.to_string()))
    })
}

/// Lists one directory (not recursive).
///
/// # Errors
/// Returns rclone's error, e.g. when the directory does not exist.
pub fn list(b: &dyn Backend, fs: &str, remote: &str) -> Result<Vec<ListItem>, RpcError> {
    let v = b.rpc("operations/list", json!({ "fs": fs, "remote": remote, "opt": {} }))?;
    Ok(decode::<ListReply>("operations/list", v)?.list)
}

/// # Errors
/// Returns rclone's error.
pub fn mkdir(b: &dyn Backend, fs: &str, remote: &str) -> Result<(), RpcError> {
    b.rpc("operations/mkdir", json!({ "fs": fs, "remote": remote })).map(drop)
}

/// Moves or renames one file.
///
/// # Errors
/// Returns rclone's error.
pub fn movefile(
    b: &dyn Backend,
    src_fs: &str,
    src_remote: &str,
    dst_fs: &str,
    dst_remote: &str,
) -> Result<(), RpcError> {
    b.rpc(
        "operations/movefile",
        json!({ "srcFs": src_fs, "srcRemote": src_remote, "dstFs": dst_fs, "dstRemote": dst_remote }),
    )
    .map(drop)
}

/// Moves a whole directory (`sync/move`), synchronously.
///
/// # Errors
/// Returns rclone's error.
pub fn move_dir(b: &dyn Backend, src_fs: &str, dst_fs: &str) -> Result<(), RpcError> {
    b.rpc("sync/move", move_dir_params(src_fs, dst_fs)).map(drop)
}

pub fn move_dir_params(src_fs: &str, dst_fs: &str) -> Value {
    json!({ "srcFs": src_fs, "dstFs": dst_fs, "createEmptySrcDirs": true, "deleteEmptySrcDirs": true })
}

/// rse's own copy and move (`librse/resume`): it skips files the target already has, and
/// resumes a file a stopped or failed job left half done where the target allows it (see
/// [`resumable`]). Runs as a job, like `sync/copy`, with stats in [`job_group`].
pub const COPY: &str = "rse/copy";

/// The parameters of [`COPY`] for a file (`src_remote` in `src_fs` to `dst_remote` in
/// `dst_fs`), or for a directory with `is_dir`, where the remotes may be empty to take
/// the fs roots. A directory's empty directories are copied too; a move deletes the
/// source directories it empties, but not the source root.
pub fn copy_params(
    src_fs: &str,
    src_remote: &str,
    dst_fs: &str,
    dst_remote: &str,
    is_dir: bool,
    moving: bool,
) -> Value {
    json!({
        "srcFs": src_fs,
        "srcRemote": src_remote,
        "dstFs": dst_fs,
        "dstRemote": dst_remote,
        "isDir": is_dir,
        "move": moving,
        "createEmptySrcDirs": is_dir,
    })
}

#[derive(Deserialize)]
struct ResumableReply {
    resumable: bool,
}

/// Whether [`COPY`] from `src_fs` to `dst_fs` resumes a stopped file instead of starting
/// it over: when the target is the local disk or an sftp server reached through an
/// `ssh` command.
///
/// # Errors
/// Returns rclone's error, e.g. for an unknown remote.
pub fn resumable(b: &dyn Backend, src_fs: &str, dst_fs: &str) -> Result<bool, RpcError> {
    let v = b.rpc("rse/resumable", json!({ "srcFs": src_fs, "dstFs": dst_fs }))?;
    Ok(decode::<ResumableReply>("rse/resumable", v)?.resumable)
}

/// Removes the empty directories under `remote` in `fs`, and `remote` itself too when it
/// ends up empty and `leave_root` is false.
///
/// # Errors
/// Returns rclone's error.
pub fn rmdirs(b: &dyn Backend, fs: &str, remote: &str, leave_root: bool) -> Result<(), RpcError> {
    b.rpc("operations/rmdirs", json!({ "fs": fs, "remote": remote, "leaveRoot": leave_root }))
        .map(drop)
}

/// # Errors
/// Returns rclone's error.
pub fn rmdir(b: &dyn Backend, fs: &str, remote: &str) -> Result<(), RpcError> {
    b.rpc("operations/rmdir", json!({ "fs": fs, "remote": remote })).map(drop)
}

/// Starts `method` as an rclone job and returns its job id.
///
/// # Errors
/// Returns rclone's error when the job cannot be started.
pub fn submit(b: &dyn Backend, method: &str, mut params: Value) -> Result<i64, RpcError> {
    if let Value::Object(map) = &mut params {
        map.insert("_async".to_owned(), Value::Bool(true));
    }
    let v = b.rpc(method, params)?;
    v.get("jobid")
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::new(0, tr!("rclone-no-jobid", method = method)))
}

/// # Errors
/// Returns rclone's error, e.g. for an unknown job.
pub fn job_status(b: &dyn Backend, jobid: i64) -> Result<JobStatus, RpcError> {
    decode("job/status", b.rpc("job/status", json!({ "jobid": jobid }))?)
}

/// # Errors
/// Returns rclone's error, e.g. for an unknown job.
pub fn job_stop(b: &dyn Backend, jobid: i64) -> Result<(), RpcError> {
    b.rpc("job/stop", json!({ "jobid": jobid })).map(drop)
}

/// The stats group rclone uses for an async job.
pub fn job_group(jobid: i64) -> String {
    format!("job/{jobid}")
}

/// # Errors
/// Returns rclone's error.
pub fn stats(b: &dyn Backend, group: &str) -> Result<Stats, RpcError> {
    decode("core/stats", b.rpc("core/stats", json!({ "group": group }))?)
}

/// Drops a finished job's stats group so it does not accumulate.
///
/// # Errors
/// Returns rclone's error.
pub fn stats_delete(b: &dyn Backend, group: &str) -> Result<(), RpcError> {
    b.rpc("core/stats-delete", json!({ "group": group })).map(drop)
}

/// The names of the configured remotes, without the trailing colon, sorted.
pub fn listremotes(b: &dyn Backend) -> Result<Vec<String>, RpcError> {
    let v = b.rpc("config/listremotes", json!({}))?;
    let mut names: Vec<String> = v
        .get("remotes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

/// The configured remotes and their backend types (`sftp`, `s3`, …), sorted by name. A
/// remote whose type cannot be read, such as one defined only in the environment, gets
/// an empty type.
///
/// # Errors
/// Returns rclone's error from listing the remotes.
pub fn remotes(b: &dyn Backend) -> Result<Vec<(String, String)>, RpcError> {
    let names = listremotes(b)?;
    let dump = b.rpc("config/dump", json!({})).unwrap_or_default();
    Ok(names
        .into_iter()
        .map(|name| {
            let kind = dump.get(&name).and_then(|r| r.get("type")).and_then(Value::as_str);
            let kind = kind.unwrap_or_default().to_owned();
            (name, kind)
        })
        .collect())
}

/// rclone's `obscure`: the form rclone.conf and connection strings hold passwords in.
///
/// # Errors
/// Returns rclone's error.
pub fn obscure(b: &dyn Backend, clear: &str) -> Result<String, RpcError> {
    let v = b.rpc("core/obscure", json!({ "clear": clear }))?;
    v.get("obscured")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcError::new(0, "core/obscure returned nothing"))
}

/// rclone's own version string, e.g. `v1.75.2`.
///
/// # Errors
/// Returns rclone's error.
pub fn version(b: &dyn Backend) -> Result<String, RpcError> {
    let v = b.rpc("core/version", json!({}))?;
    Ok(v.get("version").and_then(Value::as_str).unwrap_or("unknown").to_owned())
}

/// Sets the number of parallel transfers and checkers.
///
/// # Errors
/// Returns rclone's error.
pub fn set_parallelism(b: &dyn Backend, transfers: u32, checkers: u32) -> Result<(), RpcError> {
    b.rpc("options/set", json!({ "main": { "Transfers": transfers, "Checkers": checkers } }))
        .map(drop)
}

/// Stops rclone from prompting on the terminal for a config password.
///
/// # Errors
/// Returns rclone's error.
pub fn disable_ask_password(b: &dyn Backend) -> Result<(), RpcError> {
    b.rpc("options/set", json!({ "main": { "AskPassword": false } })).map(drop)
}

/// # Errors
/// Returns rclone's error.
pub fn set_config_path(b: &dyn Backend, path: &str) -> Result<(), RpcError> {
    b.rpc("config/setpath", json!({ "path": path })).map(drop)
}

/// # Errors
/// Returns rclone's error, e.g. for a wrong password.
pub fn unlock_config(b: &dyn Backend, password: &str) -> Result<(), RpcError> {
    b.rpc("config/unlock", json!({ "configPassword": password })).map(drop)
}

/// # Errors
/// Returns rclone's error.
pub fn noop(b: &dyn Backend, input: Value) -> Result<Value, RpcError> {
    b.rpc("rc/noop", input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rclone::FakeBackend;

    #[test]
    fn list_decodes_rclone_reply() {
        let b = FakeBackend::new(|_, _| {
            Ok(json!({"list": [
                {"Path": "a/x.txt", "Name": "x.txt", "Size": 3, "MimeType": "text/plain",
                 "ModTime": "2026-10-22T02:32:00.123+08:00", "IsDir": false},
                {"Path": "a/d", "Name": "d", "Size": -1, "ModTime": "2026-10-22T02:32:00Z", "IsDir": true}
            ]}))
        });
        let items = list(&b, "/", "a").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "x.txt");
        assert_eq!(items[0].size, 3);
        assert!(items[1].is_dir);
        assert_eq!(
            b.calls_to("operations/list"),
            vec![json!({"fs": "/", "remote": "a", "opt": {}})]
        );
    }

    #[test]
    fn submit_adds_async_and_reads_jobid() {
        let b = FakeBackend::new(|_, _| Ok(json!({"jobid": 7})));
        assert_eq!(submit(&b, "sync/copy", json!({"srcFs": "a:", "dstFs": "b:"})).unwrap(), 7);
        assert_eq!(b.calls_to("sync/copy")[0]["_async"], json!(true));
    }

    #[test]
    fn copy_params_name_rse_copy_parameters() {
        let p = copy_params("/a", "x", "box:", "y", false, true);
        assert_eq!(
            p,
            json!({"srcFs": "/a", "srcRemote": "x", "dstFs": "box:", "dstRemote": "y",
                   "isDir": false, "move": true, "createEmptySrcDirs": false})
        );
        assert_eq!(copy_params("/a", "", "/b", "", true, false)["createEmptySrcDirs"], json!(true));
    }

    #[test]
    fn resumable_reads_the_reply() {
        let b = FakeBackend::new(|_, _| Ok(json!({"resumable": true})));
        assert!(resumable(&b, "/", "box:").unwrap());
        assert_eq!(b.calls_to("rse/resumable"), vec![json!({"srcFs": "/", "dstFs": "box:"})]);
    }

    #[test]
    fn stats_tolerates_null_eta() {
        let b = FakeBackend::new(|_, _| {
            Ok(json!({"bytes": 5, "totalBytes": 10, "speed": 1.5, "eta": null}))
        });
        let s = stats(&b, "job/1").unwrap();
        assert_eq!((s.bytes, s.total_bytes, s.eta), (5, 10, None));
    }
}
