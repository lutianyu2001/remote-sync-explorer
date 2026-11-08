//! Worker threads. The UI thread never calls rclone; it sends requests here and receives
//! the results as events.
//!
//! - Browse workers (a small pool) run listings and other short calls. A slow remote only
//!   occupies one worker, and listings nobody waits for any more are skipped.
//! - One job worker starts transfers, as rsync runs where rsync carries them out (see
//!   [`Engines`]) and as async rclone jobs otherwise, polls them (`job/status` and
//!   `core/stats` for rclone), and stops them on request.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use serde_json::{Value, json};

use crate::engine::Engines;
use crate::event::{BrowseOp, Entry, Event, JobId, JobSnapshot, ReqId};
use crate::jobs::{Engine, OpKind, Transfer};
use crate::location::{Location, endpoint_key};
use crate::rclone::api::{self, Stats};
use crate::rclone::{Backend, RpcError};
use crate::rsync::Running;

enum BrowseReq {
    List { generation: u64, loc: Location },
    Probe { req: ReqId, loc: Location },
    Op { req: ReqId, op: BrowseOp },
    Remotes,
    Obscure { req: ReqId, clear: String },
    Engine { a: Location, b: Location },
    Shell { req: ReqId, loc: Location },
}

enum JobCmd {
    Submit { id: JobId, transfer: Box<Transfer>, method: String, params: Value },
    Stop { id: JobId },
}

pub struct Exec {
    browse: Option<Sender<BrowseReq>>,
    jobs: Option<Sender<JobCmd>>,
    wanted: Arc<Mutex<[u64; 2]>>,
    handles: Vec<JoinHandle<()>>,
    events: Sender<Event>,
}

impl Exec {
    /// Workers that leave every transfer to rclone.
    pub fn spawn(
        backend: Arc<dyn Backend>,
        events: Sender<Event>,
        browse_threads: usize,
        poll: Duration,
    ) -> Self {
        Self::spawn_with(backend, events, browse_threads, poll, Engines::default())
    }

    pub fn spawn_with(
        backend: Arc<dyn Backend>,
        events: Sender<Event>,
        browse_threads: usize,
        poll: Duration,
        engines: Engines,
    ) -> Self {
        let engines = Arc::new(engines);
        let events_out = events.clone();
        let (browse_tx, browse_rx) = unbounded();
        let (jobs_tx, jobs_rx) = unbounded();
        let wanted = Arc::new(Mutex::new([0; 2]));
        let mut handles = Vec::new();
        for _ in 0..browse_threads.max(1) {
            let (backend, events, rx, wanted, engines) = (
                backend.clone(),
                events.clone(),
                browse_rx.clone(),
                wanted.clone(),
                engines.clone(),
            );
            handles.push(thread::spawn(move || {
                browse_worker(&*backend, &engines, &rx, &events, &wanted);
            }));
        }
        handles.push(thread::spawn(move || {
            job_worker(&*backend, &engines, &jobs_rx, &events, poll);
        }));
        Self { browse: Some(browse_tx), jobs: Some(jobs_tx), wanted, handles, events: events_out }
    }

    /// Records which listing generations the panes still wait for; others are skipped.
    pub fn set_wanted(&self, generations: [u64; 2]) {
        if let Ok(mut w) = self.wanted.lock() {
            *w = generations;
        }
    }

    pub fn list(&self, generation: u64, loc: Location) {
        self.send_browse(BrowseReq::List { generation, loc });
    }

    pub fn probe(&self, req: ReqId, loc: Location) {
        self.send_browse(BrowseReq::Probe { req, loc });
    }

    pub fn op(&self, req: ReqId, op: BrowseOp) {
        self.send_browse(BrowseReq::Op { req, op });
    }

    pub fn remotes(&self) {
        self.send_browse(BrowseReq::Remotes);
    }

    pub fn obscure(&self, req: ReqId, clear: String) {
        self.send_browse(BrowseReq::Obscure { req, clear });
    }

    /// Starts `transfer` as job `id`: with rsync when rsync carries it out, else as the
    /// rclone call `method` with `params`.
    pub fn submit(&self, id: JobId, transfer: Transfer, method: &str, params: Value) {
        if let Some(tx) = &self.jobs {
            let transfer = Box::new(transfer);
            let _ = tx.send(JobCmd::Submit { id, transfer, method: method.to_owned(), params });
        }
    }

    /// Finds out which engine copies between `a` and `b`, answered by [`Event::Engine`].
    pub fn engine(&self, a: Location, b: Location) {
        self.send_browse(BrowseReq::Engine { a, b });
    }

    /// Finds out how a terminal reaches `loc`'s server, answered by [`Event::Shell`]. May
    /// open the master connection, asking the user to log in.
    pub fn shell(&self, req: ReqId, loc: Location) {
        self.send_browse(BrowseReq::Shell { req, loc });
    }

    /// Where events for the UI thread go, for threads of its own (the terminal's).
    pub fn events(&self) -> Sender<Event> {
        self.events.clone()
    }

    pub fn stop(&self, id: JobId) {
        if let Some(tx) = &self.jobs {
            let _ = tx.send(JobCmd::Stop { id });
        }
    }

    fn send_browse(&self, req: BrowseReq) {
        if let Some(tx) = &self.browse {
            let _ = tx.send(req);
        }
    }

    /// Closes the request channels and waits for the workers to finish their current call.
    pub fn shutdown(mut self) {
        self.browse = None;
        self.jobs = None;
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }

    /// Like `shutdown`, but gives up after `limit` (a listing on a dead remote can hang
    /// much longer). Returns whether all workers finished.
    pub fn shutdown_timeout(self, limit: Duration) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            self.shutdown();
            let _ = tx.send(());
        });
        rx.recv_timeout(limit).is_ok()
    }
}

fn list_entries(backend: &dyn Backend, loc: &Location) -> Result<Vec<Entry>, RpcError> {
    Ok(api::list(backend, &loc.fs_string(), "")?.into_iter().map(Entry::from).collect())
}

fn browse_worker(
    backend: &dyn Backend,
    engines: &Engines,
    rx: &Receiver<BrowseReq>,
    events: &Sender<Event>,
    wanted: &Mutex<[u64; 2]>,
) {
    for req in rx {
        let event = match req {
            BrowseReq::List { generation, loc } => {
                let still_wanted = wanted.lock().map_or(true, |w| w.contains(&generation));
                if still_wanted {
                    Event::Listed { generation, result: list_entries(backend, &loc) }
                } else {
                    Event::ListSkipped { generation }
                }
            }
            BrowseReq::Probe { req, loc } => {
                Event::Probed { req, result: list_entries(backend, &loc) }
            }
            BrowseReq::Op { req, op } => Event::OpDone { req, result: run_op(backend, &op) },
            BrowseReq::Remotes => Event::Remotes(api::remotes(backend)),
            BrowseReq::Obscure { req, clear } => {
                Event::Obscured { req, result: api::obscure(backend, &clear) }
            }
            BrowseReq::Engine { a, b } => Event::Engine {
                endpoints: (endpoint_key(&a), endpoint_key(&b)),
                engine: engines.between(&a, &b),
            },
            BrowseReq::Shell { req, loc } => Event::Shell { req, result: engines.shell(&loc) },
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

fn run_op(backend: &dyn Backend, op: &BrowseOp) -> Result<(), RpcError> {
    match op {
        BrowseOp::Mkdir(loc) => {
            let (fs, remote) = loc.rc_pair();
            api::mkdir(backend, &fs, &remote)
        }
        BrowseOp::Rename { from, to, is_dir: false } => {
            let ((src_fs, src), (dst_fs, dst)) = (from.rc_pair(), to.rc_pair());
            api::movefile(backend, &src_fs, &src, &dst_fs, &dst)
        }
        BrowseOp::Rename { from, to, is_dir: true } => {
            api::move_dir(backend, &from.fs_string(), &to.fs_string())?;
            // sync/move leaves the emptied source directory behind.
            remove_empty_dir(backend, from)
        }
        BrowseOp::RemoveEmptyDir(dir) => remove_empty_dir(backend, dir),
    }
}

fn remove_empty_dir(backend: &dyn Backend, dir: &Location) -> Result<(), RpcError> {
    let (fs, remote) = dir.rc_pair();
    match api::rmdir(backend, &fs, &remote) {
        Err(e) if !is_not_found(&e) => Err(e),
        _ => Ok(()),
    }
}

fn is_not_found(e: &RpcError) -> bool {
    e.status == 404 || e.message.contains("not found") || e.message.contains("no such file")
}

enum Active {
    Rclone { rc_id: i64, stop_sent: bool },
    Rsync { run: Box<Running>, transfer: Transfer },
}

fn job_worker(
    backend: &dyn Backend,
    engines: &Engines,
    rx: &Receiver<JobCmd>,
    events: &Sender<Event>,
    poll: Duration,
) {
    let mut active: BTreeMap<JobId, Active> = BTreeMap::new();
    let mut next_poll = Instant::now() + poll;
    loop {
        match rx.recv_deadline(next_poll) {
            Ok(JobCmd::Submit { id, transfer, method, params }) => {
                let (run, started) = start(backend, engines, *transfer, &method, params);
                let event = match run {
                    Ok(run) => {
                        active.insert(id, run);
                        Event::JobStarted { id, engine: started.0, resumable: started.1 }
                    }
                    Err(e) => {
                        let finished = Some(Err(e));
                        Event::Jobs(vec![JobSnapshot { id, finished, stats: Stats::default() }])
                    }
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            Ok(JobCmd::Stop { id }) => match active.get_mut(&id) {
                Some(Active::Rclone { rc_id, stop_sent }) => {
                    // An error here means the job already ended; the next poll reports it.
                    let _ = api::job_stop(backend, *rc_id);
                    *stop_sent = true;
                }
                Some(Active::Rsync { run, .. }) => run.terminate(),
                None => {}
            },
            Err(RecvTimeoutError::Timeout) => {
                next_poll = Instant::now() + poll;
                if active.is_empty() {
                    continue;
                }
                let snaps = poll_jobs(backend, &mut active);
                if events.send(Event::Jobs(snaps)).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Shutting down: stop whatever is still running.
                for job in active.values_mut() {
                    match job {
                        Active::Rclone { rc_id, stop_sent: false } => {
                            let _ = api::job_stop(backend, *rc_id);
                        }
                        Active::Rsync { run, .. } => run.stop(),
                        Active::Rclone { .. } => {}
                    }
                }
                return;
            }
        }
    }
}

/// Starts `transfer`, and tells which engine runs it and whether a stop keeps the file
/// under way: rsync's partial files always do, rclone's copy where the target allows
/// it, and a delete loses nothing.
fn start(
    backend: &dyn Backend,
    engines: &Engines,
    transfer: Transfer,
    method: &str,
    mut params: Value,
) -> (Result<Active, String>, (Engine, bool)) {
    if let Some(args) = engines.rsync_args(&transfer) {
        let run = Running::spawn(args)
            .map(|run| Active::Rsync { run: Box::new(run), transfer })
            .map_err(|e| tr!("rsync-not-started", error = e.to_string()));
        return (run, (Engine::Rsync, true));
    }
    let mut resumable = true;
    let mut method = method;
    if method == api::COPY {
        let fs = |key: &str| params[key].as_str().unwrap_or_default().to_owned();
        match api::resumable(backend, &fs("srcFs"), &fs("dstFs")) {
            Ok(r) => resumable = r,
            // An rclone without rse's additions (an external rcd) copies as rclone does.
            Err(e) if api::is_unknown_method(&e) => {
                (method, params) = api::plain_copy(&params);
                resumable = false;
            }
            Err(_) => resumable = false,
        }
    }
    if method == api::COPY && engines.checksum {
        params["checksum"] = json!(true);
    }
    let run = api::submit(backend, method, params)
        .map(|rc_id| Active::Rclone { rc_id, stop_sent: false })
        .map_err(|e| e.message);
    (run, (Engine::Rclone, resumable))
}

fn poll_jobs(backend: &dyn Backend, active: &mut BTreeMap<JobId, Active>) -> Vec<JobSnapshot> {
    let mut snaps = Vec::with_capacity(active.len());
    let mut done = Vec::new();
    for (&id, job) in active.iter_mut() {
        let (stats, finished) = match job {
            Active::Rclone { rc_id, .. } => poll_rclone(backend, *rc_id),
            Active::Rsync { run, transfer } => {
                let (stats, finished) = run.poll();
                let finished = match finished {
                    Some(Ok(())) => Some(after_rsync(backend, transfer)),
                    other => other,
                };
                (stats, finished)
            }
        };
        if finished.is_some() {
            done.push(id);
        }
        snaps.push(JobSnapshot { id, finished, stats });
    }
    for id in done {
        active.remove(&id);
    }
    snaps
}

fn poll_rclone(backend: &dyn Backend, rc_id: i64) -> (Stats, Option<Result<(), String>>) {
    let group = api::job_group(rc_id);
    let stats = api::stats(backend, &group).unwrap_or_default();
    let finished = match api::job_status(backend, rc_id) {
        Ok(s) if s.finished => {
            Some(if s.success { Ok(()) } else { Err(job_error(&s.error, &stats)) })
        }
        Ok(_) => None,
        Err(e) => Some(Err(tr!("job-lost", id = rc_id, error = e.to_string()))),
    };
    if finished.is_some() {
        let _ = api::stats_delete(backend, &group);
    }
    (stats, finished)
}

/// rsync moves a directory's files but leaves its directories: they go once it is done,
/// all but the moved directory itself, which the caller removes as after an rclone move.
fn after_rsync(backend: &dyn Backend, t: &Transfer) -> Result<(), String> {
    if t.kind != OpKind::Move || !t.is_dir {
        return Ok(());
    }
    match api::rmdirs(backend, &t.src_dir.join(&t.name).fs_string(), "", true) {
        Err(e) if !is_not_found(&e) => Err(e.message),
        _ => Ok(()),
    }
}

fn job_error(status_error: &str, stats: &Stats) -> String {
    if !status_error.is_empty() {
        status_error.to_owned()
    } else if !stats.last_error.is_empty() {
        stats.last_error.clone()
    } else {
        tr!("job-failed-unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rclone::FakeBackend;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// The next event that is not a job's start.
    fn recv(rx: &Receiver<Event>) -> Event {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
                Event::JobStarted { .. } => {}
                other => return other,
            }
        }
    }

    fn copy() -> Transfer {
        Transfer {
            kind: OpKind::Copy,
            src_dir: Location::new("/", "a"),
            name: "f".into(),
            is_dir: false,
            dst_dir: Some(Location::new("nas:", "")),
            size: None,
            overwrite: false,
        }
    }

    #[test]
    fn listing_result_comes_back_as_event() {
        let b =
            Arc::new(FakeBackend::new(|_, _| Ok(json!({"list": [{"Name": "a", "IsDir": true}]}))));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b.clone(), tx, 2, Duration::from_millis(20));
        exec.set_wanted([1, 0]);
        exec.list(1, Location::new("/", "x"));
        match recv(&rx) {
            Event::Listed { generation: 1, result: Ok(entries) } => {
                assert_eq!((entries[0].name.as_str(), entries[0].is_dir), ("a", true));
            }
            other => panic!("unexpected {other:?}"),
        }
        exec.shutdown();
        assert_eq!(
            b.calls_to("operations/list"),
            vec![json!({"fs": "/x", "remote": "", "opt": {}})]
        );
    }

    #[test]
    fn unwanted_listing_is_skipped() {
        let b = Arc::new(FakeBackend::new(|_, _| Ok(json!({"list": []}))));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b.clone(), tx, 1, Duration::from_millis(20));
        exec.set_wanted([2, 0]);
        exec.list(1, Location::new("/", "old"));
        exec.list(2, Location::new("/", "new"));
        assert!(matches!(recv(&rx), Event::ListSkipped { generation: 1 }));
        assert!(matches!(recv(&rx), Event::Listed { generation: 2, .. }));
        exec.shutdown();
        assert_eq!(b.calls_to("operations/list").len(), 1);
    }

    #[test]
    fn job_is_submitted_polled_and_finished() {
        let polls = Arc::new(AtomicI64::new(0));
        let p = polls.clone();
        let b = Arc::new(FakeBackend::new(move |m, _| match m {
            "operations/copyfile" => Ok(json!({"jobid": 42})),
            "core/stats" => Ok(json!({"bytes": 50, "totalBytes": 100})),
            "job/status" => {
                let n = p.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"id": 42, "finished": n >= 1, "success": true, "error": ""}))
            }
            _ => Ok(json!({})),
        }));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b.clone(), tx, 1, Duration::from_millis(20));
        exec.submit(7, copy(), "operations/copyfile", json!({"srcFs": "/"}));
        let Event::Jobs(first) = recv(&rx) else { panic!() };
        assert_eq!((first[0].id, first[0].finished.clone(), first[0].stats.bytes), (7, None, 50));
        let Event::Jobs(second) = recv(&rx) else { panic!() };
        assert_eq!(second[0].finished, Some(Ok(())));
        exec.shutdown();
        assert_eq!(b.calls_to("core/stats")[0], json!({"group": "job/42"}));
        assert_eq!(b.calls_to("core/stats-delete"), vec![json!({"group": "job/42"})]);
    }

    /// An rclone without rse's own calls, such as an external rcd, copies as rclone does.
    #[test]
    fn without_rse_copy_rclone_copies_by_itself() {
        let b = Arc::new(FakeBackend::new(|m, _| match m {
            "rse/resumable" => Err(RpcError::new(404, "couldn't find method \"rse/resumable\"")),
            "operations/copyfile" => Ok(json!({"jobid": 4})),
            "job/status" => Ok(json!({"id": 4, "finished": true, "success": true})),
            _ => Ok(json!({})),
        }));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b.clone(), tx, 1, Duration::from_millis(20));
        let (method, params) = copy().rc_call().unwrap();
        exec.submit(2, copy(), method, params);
        let Event::Jobs(s) = recv(&rx) else { panic!() };
        assert_eq!(s[0].finished, Some(Ok(())));
        exec.shutdown();
        assert_eq!(
            b.calls_to("operations/copyfile")[0],
            json!({"srcFs": "/a", "srcRemote": "f", "dstFs": "nas:", "dstRemote": "f", "_async": true})
        );
        assert_eq!(b.calls_to("rse/copy").len(), 0);
    }

    #[test]
    fn failed_submit_is_reported_as_finished_job() {
        let b = Arc::new(FakeBackend::new(|_, _| Err(RpcError::new(500, "no such remote"))));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b, tx, 1, Duration::from_millis(20));
        exec.submit(3, copy(), "sync/copy", json!({}));
        let Event::Jobs(s) = recv(&rx) else { panic!() };
        assert_eq!(s[0].finished, Some(Err("no such remote".to_owned())));
        exec.shutdown();
    }

    #[test]
    fn stop_calls_job_stop_with_rclone_id() {
        let b = Arc::new(FakeBackend::new(|m, _| match m {
            "sync/copy" => Ok(json!({"jobid": 9})),
            "job/status" => Ok(json!({"id": 9, "finished": false, "success": false})),
            _ => Ok(json!({})),
        }));
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(b.clone(), tx, 1, Duration::from_millis(20));
        exec.submit(1, copy(), "sync/copy", json!({}));
        let _ = recv(&rx);
        exec.stop(1);
        let _ = recv(&rx);
        exec.shutdown();
        assert_eq!(b.calls_to("job/stop"), vec![json!({"jobid": 9})]);
    }
}
