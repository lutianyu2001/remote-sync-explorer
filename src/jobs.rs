//! The transfer queue: one job per top-level item, a concurrency limit, and the job
//! state machine (queued → checking the target → running → done / failed / cancelled).
//!
//! A job runs only while the panes show its endpoints: both for a copy or move, the one
//! it deletes in for a delete. When a pane leaves for another endpoint, the jobs that no
//! longer have theirs are paused, and they go on when the panes show them again.

use serde_json::{Value, json};

use crate::event::{JobId, JobSnapshot};
use crate::location::{Location, endpoint_key};
use crate::rclone::api::{self, Stats};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    Copy,
    Move,
    Delete,
}

impl OpKind {
    /// `copy`, for a job's description.
    pub fn verb(self) -> String {
        match self {
            Self::Copy => tr!("op-copy"),
            Self::Move => tr!("op-move"),
            Self::Delete => tr!("op-delete"),
        }
    }

    /// `Copy`, for titles and buttons.
    pub fn title(self) -> String {
        match self {
            Self::Copy => tr!("op-copy-title"),
            Self::Move => tr!("op-move-title"),
            Self::Delete => tr!("op-delete-title"),
        }
    }
}

/// One rclone call: what to do with one top-level entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub kind: OpKind,
    pub src_dir: Location,
    pub name: String,
    pub is_dir: bool,
    /// The directory the entry goes into; `None` for deletes.
    pub dst_dir: Option<Location>,
    /// Bytes, when known from the listing (files only).
    pub size: Option<i64>,
    /// The user chose to replace what the target has of this name, or the job has been
    /// writing it already, so it starts without the target being checked again.
    pub overwrite: bool,
}

/// The program that carries out a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Rclone,
    Rsync,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rclone => "rclone",
            Self::Rsync => "rsync",
        }
    }
}

impl Transfer {
    /// The rc method and its parameters (without `_async`). A copy or move without a
    /// target is an error, never a fallback to something else.
    pub fn rc_call(&self) -> Result<(&'static str, Value), String> {
        let src = self.src_dir.join(&self.name);
        let dst = self.dst_dir.as_ref().map(|d| d.join(&self.name));
        Ok(match (self.kind, self.is_dir, dst) {
            (OpKind::Copy, false, Some(_)) => ("operations/copyfile", self.file_pair()),
            (OpKind::Move, false, Some(_)) => ("operations/movefile", self.file_pair()),
            (OpKind::Copy, true, Some(dst)) => (
                "sync/copy",
                json!({ "srcFs": src.fs_string(), "dstFs": dst.fs_string(), "createEmptySrcDirs": true }),
            ),
            (OpKind::Move, true, Some(dst)) => {
                ("sync/move", api::move_dir_params(&src.fs_string(), &dst.fs_string()))
            }
            (OpKind::Delete, false, _) => {
                ("operations/deletefile", in_dir(&self.src_dir, &self.name))
            }
            (OpKind::Delete, true, _) => ("operations/purge", in_dir(&self.src_dir, &self.name)),
            (OpKind::Copy | OpKind::Move, _, None) => {
                return Err(format!(
                    "internal error: {} of {} has no target",
                    self.kind.verb(),
                    self.name
                ));
            }
        })
    }

    /// The endpoints the job needs shown to run, by [`endpoint_key`].
    pub fn endpoints(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(&self.src_dir).chain(&self.dst_dir).map(endpoint_key)
    }

    /// The directories whose listings change when this finishes.
    pub fn touched(&self) -> Vec<Location> {
        self.changed_dirs().cloned().collect()
    }

    /// Where the entry of this name changes: the source, which a move or delete takes it
    /// from, and the target, where a copy or move writes it.
    fn changed_dirs(&self) -> impl Iterator<Item = &Location> {
        let source = (self.kind != OpKind::Copy).then_some(&self.src_dir);
        source.into_iter().chain(&self.dst_dir)
    }

    /// Whether this and `other` change one entry: the same name, in any case, in the same
    /// directory.
    pub fn clashes(&self, other: &Self) -> bool {
        self.changed_dirs().any(|d| other.changed_dirs().any(|o| o == d))
            && same_name(&self.name, &other.name)
    }
}

/// Names are compared without case: on case-insensitive targets (macOS, many cloud
/// drives) `Report.pdf` replaces `report.pdf`.
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl Transfer {
    /// The directories as fs strings and the name as the remote, as rclone's command line
    /// does it (a leading `/` in a remote parameter would be dropped).
    fn file_pair(&self) -> Value {
        let dst = self.dst_dir.as_ref().unwrap_or(&self.src_dir);
        json!({
            "srcFs": self.src_dir.fs_string(),
            "srcRemote": self.name,
            "dstFs": dst.fs_string(),
            "dstRemote": self.name,
        })
    }
}

fn in_dir(dir: &Location, name: &str) -> Value {
    json!({ "fs": dir.fs_string(), "remote": name })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    /// Listing the target, which must not have an entry of the job's name yet.
    Checking,
    Running,
    /// `job/stop` sent, waiting for rclone to report the end.
    Cancelling,
    /// Stopped because the panes no longer show its endpoints: queued again once the end
    /// is reported.
    Pausing,
    Done,
    Failed(String),
    Cancelled,
    /// The target had an entry of this name, and the user chose to keep it.
    Skipped,
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled | Self::Skipped)
    }

    /// Taking one of the places `max_concurrent` allows.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Checking | Self::Running | Self::Cancelling | Self::Pausing)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub transfer: Transfer,
    pub state: JobState,
    pub stats: Stats,
    /// Counts the checks of its target, so that the answer to one that was cancelled,
    /// and maybe retried since, is told apart.
    pub check: u64,
    /// What runs it, once it has started.
    pub engine: Option<Engine>,
    /// Whether a stop keeps what it has transferred of the file under way, so that it
    /// goes on from there; false until the engine says so.
    pub resumable: bool,
}

impl Job {
    /// Done fraction in 0..=1, if known.
    pub fn fraction(&self) -> Option<f64> {
        if self.state == JobState::Done {
            return Some(1.0);
        }
        #[allow(clippy::cast_precision_loss)]
        (self.stats.total_bytes > 0)
            .then(|| (self.stats.bytes as f64 / self.stats.total_bytes as f64).clamp(0.0, 1.0))
    }

    /// The file being transferred right now, if any.
    pub fn current_file(&self) -> Option<&str> {
        self.stats.transferring.first().map(|t| t.name.as_str())
    }

    fn is_checking(&self, check: u64) -> bool {
        self.state == JobState::Checking && self.check == check
    }
}

/// What the caller must do after the queue changed.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Submit this job to rclone.
    Start { id: JobId, method: &'static str, params: Value },
    /// List `dir`, the job's target, and tell `Queue::checked` whether it is free, with
    /// the number of this check.
    Check { id: JobId, check: u64, dir: Location },
    /// Call `job/stop` for this job.
    Stop(JobId),
    /// A job ended; reload these directories. `error` is set when it failed.
    Finished { id: JobId, touched: Vec<Location>, error: Option<String> },
}

#[derive(Debug)]
pub struct Queue {
    pub jobs: Vec<Job>,
    /// The panes' locations, whose endpoints jobs need; `None` shows every endpoint.
    shown: Option<Vec<Location>>,
    /// Overwrite mode: jobs start without their target being checked, replacing what it
    /// has. Off at every start.
    pub overwrite: bool,
    max_concurrent: usize,
    next_id: JobId,
}

impl Queue {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            jobs: Vec::new(),
            shown: None,
            overwrite: false,
            max_concurrent: max_concurrent.max(1),
            next_id: 1,
        }
    }

    /// Adds transfers as queued jobs and starts as many as the limit allows.
    pub fn push(&mut self, transfers: Vec<Transfer>) -> Vec<Effect> {
        for transfer in transfers {
            let id = self.next_id;
            self.next_id += 1;
            let job = Job {
                id,
                transfer,
                state: JobState::Queued,
                stats: Stats::default(),
                check: 0,
                engine: None,
                resumable: false,
            };
            self.jobs.push(job);
        }
        self.refresh_endpoints();
        self.start_ready()
    }

    /// Whether the panes show every endpoint `transfer` needs.
    fn present(&self, transfer: &Transfer) -> bool {
        let Some(shown) = &self.shown else { return true };
        transfer.endpoints().all(|e| shown.iter().any(|l| endpoint_key(l) == e))
    }

    /// The panes show `locations` now: jobs that lost an endpoint pause, a check under
    /// way given up, and jobs whose endpoints are all shown go on.
    pub fn set_shown(&mut self, locations: Vec<Location>) -> Vec<Effect> {
        self.shown = Some(locations);
        self.refresh_endpoints();
        let mut out = Vec::new();
        for index in 0..self.jobs.len() {
            if self.present(&self.jobs[index].transfer) {
                continue;
            }
            let job = &mut self.jobs[index];
            match job.state {
                JobState::Running => {
                    job.state = JobState::Pausing;
                    out.push(Effect::Stop(job.id));
                }
                JobState::Checking => job.state = JobState::Queued,
                _ => {}
            }
        }
        out.extend(self.start_ready());
        out
    }

    /// Running jobs that showing `locations` would stop with what they have done of the
    /// file under way lost.
    pub fn interrupted_by(&self, locations: &[Location]) -> Vec<JobId> {
        let keys: Vec<String> = locations.iter().map(endpoint_key).collect();
        self.jobs
            .iter()
            .filter(|j| j.state == JobState::Running && !j.resumable)
            .filter(|j| !j.transfer.endpoints().all(|e| keys.contains(&e)))
            .map(|j| j.id)
            .collect()
    }

    /// Gives the jobs of shown endpoints the panes' fs strings, which carry what a saved
    /// queue leaves out, such as an obscured password.
    fn refresh_endpoints(&mut self) {
        let Some(shown) = &self.shown else { return };
        for job in &mut self.jobs {
            let t = &mut job.transfer;
            for dir in std::iter::once(&mut t.src_dir).chain(t.dst_dir.as_mut()) {
                if let Some(l) = shown.iter().find(|l| endpoint_key(l) == endpoint_key(dir)) {
                    dir.fs.clone_from(&l.fs);
                    dir.endpoint.clone_from(&l.endpoint);
                }
            }
        }
    }

    /// The jobs of the endpoints shown, in order.
    pub fn visible(&self) -> Vec<&Job> {
        self.jobs.iter().filter(|j| self.present(&j.transfer)).collect()
    }

    /// The engine that runs job `id` and whether it can resume, as reported once it has
    /// started.
    pub fn started(&mut self, id: JobId, engine: Engine, resumable: bool) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.engine = Some(engine);
            job.resumable = resumable;
        }
    }

    /// The jobs still to do, to be saved for the next start.
    pub fn unfinished(&self) -> Vec<Transfer> {
        self.jobs.iter().filter(|j| !j.state.is_finished()).map(|j| j.transfer.clone()).collect()
    }

    /// Starts queued jobs, in order, while the limit allows, passing over those `held`.
    /// A copy or move first has its target checked, since something of its name may have
    /// arrived there after it was queued, unless the user chose to overwrite it or
    /// overwrite mode is on.
    fn start_ready(&mut self) -> Vec<Effect> {
        let mut free = self.max_concurrent.saturating_sub(self.active_count());
        let mut out = Vec::new();
        for index in 0..self.jobs.len() {
            if free == 0 {
                break;
            }
            if self.jobs[index].state != JobState::Queued
                || self.held(index)
                || !self.present(&self.jobs[index].transfer)
            {
                continue;
            }
            let job = &mut self.jobs[index];
            match &job.transfer.dst_dir {
                Some(dir) if !job.transfer.overwrite && !self.overwrite => {
                    job.state = JobState::Checking;
                    job.check += 1;
                    out.push(Effect::Check { id: job.id, check: job.check, dir: dir.clone() });
                }
                _ => out.push(start(job)),
            }
            if job.state.is_active() {
                free -= 1;
            }
        }
        out
    }

    /// Whether the queued job at `index` waits for another that changes the same entry:
    /// one under way, or queued before it. A listing cannot see a transfer in flight, so
    /// without this two jobs could write one file at once, the last to end winning; this
    /// way the later one is checked once the earlier has ended, and sees what it wrote.
    /// It holds in overwrite mode too.
    fn held(&self, index: usize) -> bool {
        let transfer = &self.jobs[index].transfer;
        self.jobs.iter().enumerate().any(|(i, other)| {
            i != index
                && (other.state.is_active() || (i < index && other.state == JobState::Queued))
                && transfer.clashes(&other.transfer)
        })
    }

    /// The target of a checking job was listed, for its check number `check`: `Ok` when
    /// it was free, which starts the job, or why the job fails instead. A failure reloads
    /// the target, as the panes may not show what it has. The answer to an older check,
    /// cancelled since, changes nothing.
    pub fn checked(&mut self, id: JobId, check: u64, result: Result<(), String>) -> Vec<Effect> {
        let mut out = Vec::new();
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id && j.is_checking(check)) {
            match result {
                Ok(()) => out.push(start(job)),
                Err(e) => {
                    job.state = JobState::Failed(e.clone());
                    let touched = job.transfer.dst_dir.iter().cloned().collect();
                    out.push(Effect::Finished { id, touched, error: Some(e) });
                }
            }
        }
        out.extend(self.start_ready());
        out
    }

    /// The job, if its check number `check` is still under way.
    pub fn checking(&self, id: JobId, check: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id && j.is_checking(check))
    }

    /// Applies a poll result from the job worker.
    pub fn apply(&mut self, snaps: Vec<JobSnapshot>) -> Vec<Effect> {
        let mut out = Vec::new();
        for snap in snaps {
            let Some(job) = self.jobs.iter_mut().find(|j| j.id == snap.id) else {
                continue;
            };
            // Only jobs given to rclone report.
            if !matches!(job.state, JobState::Running | JobState::Cancelling | JobState::Pausing) {
                continue;
            }
            if snap.stats != Stats::default() || snap.finished.is_none() {
                job.stats = snap.stats;
            }
            let Some(result) = snap.finished else {
                continue;
            };
            let error = match (result, &job.state) {
                (Ok(()), _) => {
                    job.state = JobState::Done;
                    None
                }
                (Err(_), JobState::Cancelling) => {
                    job.state = JobState::Cancelled;
                    None
                }
                (Err(_), JobState::Pausing) => {
                    job.state = JobState::Queued;
                    job.stats = Stats::default();
                    continue;
                }
                (Err(e), _) => {
                    job.state = JobState::Failed(e.clone());
                    Some(e)
                }
            };
            out.push(Effect::Finished { id: job.id, touched: job.transfer.touched(), error });
        }
        out.extend(self.start_ready());
        out
    }

    /// Cancels a queued or checking job at once, or asks rclone to stop a running one, and
    /// starts what that makes room for.
    pub fn cancel(&mut self, id: JobId) -> Vec<Effect> {
        let mut out = self.cancel_only(id);
        out.extend(self.start_ready());
        out
    }

    /// Cancels a job, starting nothing in its place.
    fn cancel_only(&mut self, id: JobId) -> Vec<Effect> {
        let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else {
            return Vec::new();
        };
        match job.state {
            JobState::Queued | JobState::Checking => {
                job.state = JobState::Cancelled;
                Vec::new()
            }
            JobState::Running => {
                job.state = JobState::Cancelling;
                vec![Effect::Stop(id)]
            }
            _ => Vec::new(),
        }
    }

    /// Pauses everything and starts nothing: used when quitting, as the jobs left are
    /// saved for the next start.
    pub fn pause_all(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        for job in &mut self.jobs {
            match job.state {
                JobState::Running => {
                    job.state = JobState::Pausing;
                    out.push(Effect::Stop(job.id));
                }
                JobState::Checking => job.state = JobState::Queued,
                _ => {}
            }
        }
        self.shown = Some(Vec::new());
        out
    }

    /// Puts a failed, cancelled or skipped job back in the queue. Its target is checked
    /// again before it starts, unless the user chose to overwrite it.
    pub fn retry(&mut self, id: JobId) -> Vec<Effect> {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id)
            && matches!(job.state, JobState::Failed(_) | JobState::Cancelled | JobState::Skipped)
        {
            job.state = JobState::Queued;
            job.stats = Stats::default();
        }
        self.start_ready()
    }

    /// The user chose to overwrite what the target of a failed job has: it runs again
    /// without the target being checked.
    pub fn approve(&mut self, id: JobId) -> Vec<Effect> {
        if let Some(job) =
            self.jobs.iter_mut().find(|j| j.id == id && matches!(j.state, JobState::Failed(_)))
        {
            job.transfer.overwrite = true;
        }
        self.retry(id)
    }

    /// The user chose to keep what the target of a failed job has.
    pub fn skip(&mut self, id: JobId) {
        if let Some(job) =
            self.jobs.iter_mut().find(|j| j.id == id && matches!(j.state, JobState::Failed(_)))
        {
            job.state = JobState::Skipped;
        }
    }

    /// Removes the finished jobs (done, failed, cancelled, skipped) of the endpoints shown.
    pub fn clear_finished(&mut self) {
        let gone: Vec<JobId> =
            self.visible().iter().filter(|j| j.state.is_finished()).map(|j| j.id).collect();
        self.jobs.retain(|j| !gone.contains(&j.id));
    }

    pub fn active_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.state.is_active()).count()
    }

    pub fn queued_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.state == JobState::Queued).count()
    }

    /// Jobs that would be lost by quitting now.
    pub fn unfinished_count(&self) -> usize {
        self.jobs.iter().filter(|j| !j.state.is_finished()).count()
    }
}

/// Hands a job to rclone, or fails it when there is no call to make.
fn start(job: &mut Job) -> Effect {
    match job.transfer.rc_call() {
        Ok((method, params)) => {
            job.state = JobState::Running;
            // From here on what the target has of this name is the job's own doing.
            job.transfer.overwrite = true;
            Effect::Start { id: job.id, method, params }
        }
        Err(e) => {
            job.state = JobState::Failed(e.clone());
            Effect::Finished { id: job.id, touched: Vec::new(), error: Some(e) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Approved to overwrite, so it starts without its target being checked.
    fn t(kind: OpKind, name: &str, is_dir: bool) -> Transfer {
        Transfer {
            kind,
            src_dir: Location::new("/", "home/sky"),
            name: name.to_owned(),
            is_dir,
            dst_dir: (kind != OpKind::Delete).then(|| Location::new("nas:", "backup")),
            size: None,
            overwrite: true,
        }
    }

    /// A copy whose target is checked before it starts.
    fn checked_copy(name: &str) -> Transfer {
        Transfer { overwrite: false, ..t(OpKind::Copy, name, false) }
    }

    fn snap(id: JobId, finished: Option<Result<(), String>>) -> JobSnapshot {
        JobSnapshot { id, finished, stats: Stats { bytes: 1, total_bytes: 2, ..Stats::default() } }
    }

    #[test]
    fn copies_and_moves_use_the_right_rc_calls() {
        let (m, p) = t(OpKind::Copy, "a.txt", false).rc_call().unwrap();
        assert_eq!(m, "operations/copyfile");
        assert_eq!(
            p,
            json!({"srcFs": "/home/sky", "srcRemote": "a.txt", "dstFs": "nas:backup", "dstRemote": "a.txt"})
        );
        let (m, p) = t(OpKind::Copy, "d", true).rc_call().unwrap();
        assert_eq!(m, "sync/copy");
        assert_eq!(
            p,
            json!({"srcFs": "/home/sky/d", "dstFs": "nas:backup/d", "createEmptySrcDirs": true})
        );
        let (m, p) = t(OpKind::Move, "d", true).rc_call().unwrap();
        assert_eq!(m, "sync/move");
        assert_eq!(p["deleteEmptySrcDirs"], json!(true));
        assert_eq!(t(OpKind::Move, "a", false).rc_call().unwrap().0, "operations/movefile");
        assert_eq!(
            t(OpKind::Delete, "a", false).rc_call().unwrap(),
            ("operations/deletefile", json!({"fs": "/home/sky", "remote": "a"}))
        );
        assert_eq!(t(OpKind::Delete, "d", true).rc_call().unwrap().0, "operations/purge");
    }

    #[test]
    fn copy_without_target_fails_instead_of_deleting() {
        let mut bad = t(OpKind::Copy, "a", false);
        bad.dst_dir = None;
        assert!(bad.rc_call().is_err());
        let mut q = Queue::new(1);
        let effects = q.push(vec![bad]);
        assert!(matches!(&effects[0], Effect::Finished { error: Some(_), .. }));
        assert!(matches!(q.jobs[0].state, JobState::Failed(_)));
    }

    #[test]
    fn concurrency_limit_queues_the_rest() {
        let mut q = Queue::new(2);
        let effects = q.push(vec![
            t(OpKind::Copy, "a", false),
            t(OpKind::Copy, "b", false),
            t(OpKind::Copy, "c", false),
        ]);
        assert_eq!(effects.len(), 2);
        assert_eq!((q.active_count(), q.queued_count()), (2, 1));
        let effects = q.apply(vec![snap(1, Some(Ok(())))]);
        assert!(matches!(effects[0], Effect::Finished { id: 1, error: None, .. }));
        assert!(matches!(effects[1], Effect::Start { id: 3, .. }));
        assert_eq!(q.jobs[0].state, JobState::Done);
    }

    #[test]
    fn progress_updates_running_jobs() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Copy, "a", false)]);
        assert_eq!(q.apply(vec![snap(1, None)]), vec![]);
        assert_eq!(q.jobs[0].fraction(), Some(0.5));
    }

    #[test]
    fn cancel_queued_is_local_and_running_goes_through_rclone() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Copy, "a", false), t(OpKind::Copy, "b", false)]);
        assert_eq!(q.cancel(2), vec![]);
        assert_eq!(q.jobs[1].state, JobState::Cancelled);
        assert_eq!(q.cancel(1), vec![Effect::Stop(1)]);
        assert_eq!(q.jobs[0].state, JobState::Cancelling);
        // rclone reports the stopped job as failed; we show it as cancelled.
        let effects = q.apply(vec![snap(1, Some(Err("context canceled".into())))]);
        assert!(matches!(&effects[0], Effect::Finished { error: None, .. }));
        assert_eq!(q.jobs[0].state, JobState::Cancelled);
        assert_eq!(q.unfinished_count(), 0);
    }

    #[test]
    fn failure_keeps_the_error_and_retry_requeues() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Delete, "a", false)]);
        let effects = q.apply(vec![snap(1, Some(Err("permission denied".into())))]);
        assert!(
            matches!(&effects[0], Effect::Finished { error: Some(e), .. } if e == "permission denied")
        );
        assert_eq!(q.jobs[0].state, JobState::Failed("permission denied".into()));
        let effects = q.retry(1);
        assert!(matches!(effects[0], Effect::Start { id: 1, method: "operations/deletefile", .. }));
        assert_eq!(q.jobs[0].state, JobState::Running);
    }

    #[test]
    fn clear_removes_only_finished() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Copy, "a", false), t(OpKind::Copy, "b", false)]);
        q.apply(vec![snap(1, Some(Ok(())))]);
        q.clear_finished();
        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.jobs[0].id, 2);
    }

    /// Quitting pauses: the running job is stopped and comes back queued, as do the
    /// others, and nothing starts any more.
    #[test]
    fn pause_all_stops_running_and_keeps_every_job() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Copy, "a", false), t(OpKind::Copy, "b", false)]);
        assert_eq!(q.pause_all(), vec![Effect::Stop(1)]);
        assert_eq!(q.jobs[0].state, JobState::Pausing);
        assert_eq!(q.apply(vec![snap(1, Some(Err("context canceled".into())))]), vec![]);
        assert_eq!(q.queued_count(), 2);
        assert_eq!(q.unfinished().len(), 2);
    }

    fn shown(fs: &[&str]) -> Vec<Location> {
        fs.iter().map(|f| Location::new(*f, "")).collect()
    }

    /// A copy needs both endpoints shown, a delete the one it deletes in; the directory
    /// shown does not matter, nor which pane shows which.
    #[test]
    fn jobs_run_only_while_their_endpoints_are_shown() {
        let mut q = Queue::new(3);
        q.set_shown(shown(&["/", "s3:"]));
        let nas_delete =
            Transfer { src_dir: Location::new("nas:", "x"), ..t(OpKind::Delete, "d", false) };
        let effects = q.push(vec![t(OpKind::Copy, "a", false), nas_delete]);
        assert_eq!(effects, vec![]);
        assert_eq!(q.visible().len(), 0);
        let effects = q.set_shown(vec![Location::new("nas:", "deep/down"), Location::new("/", "")]);
        assert!(matches!(effects[..], [Effect::Start { id: 1, .. }, Effect::Start { id: 2, .. }]));
        assert_eq!(q.visible().len(), 2);
        // Only the delete keeps its endpoint.
        let effects = q.set_shown(shown(&["nas:", "s3:"]));
        assert_eq!(effects, vec![Effect::Stop(1)]);
        assert_eq!(q.jobs[0].state, JobState::Pausing);
        assert_eq!(q.visible().iter().map(|j| j.id).collect::<Vec<_>>(), vec![2]);
        // A paused job that finished all the same is done.
        q.apply(vec![snap(1, Some(Ok(())))]);
        assert_eq!(q.jobs[0].state, JobState::Done);
    }

    /// Only running jobs that lose the file under way are named before leaving.
    #[test]
    fn interrupted_by_names_running_jobs_that_cannot_resume() {
        let mut q = Queue::new(1);
        q.push(vec![t(OpKind::Copy, "a", false), t(OpKind::Copy, "b", false)]);
        assert_eq!(q.interrupted_by(&shown(&["/", "nas:"])), Vec::<JobId>::new());
        assert_eq!(q.interrupted_by(&shown(&["/", "s3:"])), vec![1]);
        q.started(1, Engine::Rsync, true);
        assert_eq!(q.interrupted_by(&[]), Vec::<JobId>::new());
    }

    /// A job saved without the password of its server gets the pane's back.
    #[test]
    fn shown_servers_lend_jobs_their_fs_strings() {
        let mut server = crate::endpoint::Endpoint::ssh("me@box");
        server.scheme = crate::endpoint::Scheme::Ftp;
        let saved = Location::on(server.clone(), "srv");
        server.pass = Some("obscured".into());
        let pane = Location::on(server, "elsewhere");
        let mut q = Queue::new(1);
        q.set_shown(vec![Location::new("/", ""), pane.clone()]);
        q.push(vec![Transfer { dst_dir: Some(saved), ..t(OpKind::Copy, "a", false) }]);
        let dst = q.jobs[0].transfer.dst_dir.as_ref().unwrap();
        assert_eq!((&dst.fs, dst.path.as_str()), (&pane.fs, "srv"));
        assert_eq!(q.jobs[0].state, JobState::Running);
    }

    #[test]
    fn the_target_is_checked_before_a_job_starts() {
        let mut q = Queue::new(1);
        let effects = q.push(vec![checked_copy("a"), checked_copy("b")]);
        assert_eq!(
            effects,
            vec![Effect::Check { id: 1, check: 1, dir: Location::new("nas:", "backup") }]
        );
        assert_eq!(q.jobs[0].state, JobState::Checking);
        assert_eq!((q.active_count(), q.queued_count()), (1, 1), "a check takes a place");
        let effects = q.checked(1, 1, Ok(()));
        assert!(matches!(
            effects[..],
            [Effect::Start { id: 1, method: "operations/copyfile", .. }]
        ));
        assert_eq!(q.jobs[0].state, JobState::Running);
    }

    /// A taken target fails the job and frees its place. Overwrite runs it without another
    /// check; skip ends it, and a retry checks again.
    #[test]
    fn a_taken_target_fails_the_job_until_the_user_chooses() {
        let mut q = Queue::new(1);
        q.push(vec![checked_copy("a"), checked_copy("b")]);
        let effects = q.checked(1, 1, Err("a already exists there".into()));
        assert_eq!(
            effects[0],
            Effect::Finished {
                id: 1,
                touched: vec![Location::new("nas:", "backup")],
                error: Some("a already exists there".into()),
            }
        );
        assert!(matches!(effects[1], Effect::Check { id: 2, .. }), "the next job goes on");
        assert_eq!(q.jobs[0].state, JobState::Failed("a already exists there".into()));

        q.checked(2, 1, Err("b already exists there".into()));
        assert!(matches!(q.approve(1)[..], [Effect::Start { id: 1, .. }]));
        q.skip(2);
        assert_eq!(q.jobs[1].state, JobState::Skipped);
        assert_eq!(q.unfinished_count(), 1);
        // A retry checks a skipped job again, but not one the user chose to overwrite.
        q.cancel(1);
        q.apply(vec![snap(1, Some(Err("context canceled".into())))]);
        assert!(matches!(q.retry(2)[..], [Effect::Check { id: 2, .. }]));
        q.cancel(2);
        assert!(matches!(q.retry(1)[..], [Effect::Start { id: 1, .. }]));
    }

    #[test]
    fn overwrite_mode_starts_jobs_without_a_check() {
        let mut q = Queue::new(1);
        q.overwrite = true;
        assert!(matches!(q.push(vec![checked_copy("a")])[..], [Effect::Start { id: 1, .. }]));
    }

    /// Cancelling a checking job makes room for the next. The answer to its check changes
    /// nothing, even once it is retried and checking again.
    #[test]
    fn a_cancelled_check_makes_room_and_its_answer_is_ignored() {
        let mut q = Queue::new(1);
        q.push(vec![checked_copy("a"), checked_copy("b")]);
        assert!(matches!(q.cancel(1)[..], [Effect::Check { id: 2, check: 1, .. }]));
        q.cancel(2);
        assert!(matches!(q.retry(1)[..], [Effect::Check { id: 1, check: 2, .. }]));
        assert_eq!(q.checked(1, 1, Err("a already exists there".into())), vec![]);
        assert_eq!(q.jobs[0].state, JobState::Checking);
        assert!(matches!(q.checked(1, 2, Ok(()))[..], [Effect::Start { id: 1, .. }]));
    }

    /// A listing cannot see a transfer in flight: a job waits while another that changes
    /// its entry, the same name in any case in the same directory, is under way or
    /// queued before it, in overwrite mode too. Other jobs go ahead.
    #[test]
    fn jobs_that_change_one_entry_never_run_at_once() {
        let mut q = Queue::new(3);
        q.overwrite = true;
        let effects =
            q.push(vec![checked_copy("IMG.JPG"), checked_copy("img.jpg"), checked_copy("x")]);
        assert!(matches!(effects[..], [Effect::Start { id: 1, .. }, Effect::Start { id: 3, .. }]));
        assert_eq!(q.jobs[1].state, JobState::Queued);
        let effects = q.apply(vec![snap(1, Some(Ok(())))]);
        assert!(matches!(
            effects[..],
            [Effect::Finished { id: 1, .. }, Effect::Start { id: 2, .. }]
        ));

        // Deleting the entry in the target counts too. Without overwrite mode the copy is
        // checked once the delete has ended.
        let mut q = Queue::new(3);
        let delete = Transfer {
            src_dir: Location::new("nas:", "backup"),
            ..t(OpKind::Delete, "img.jpg", false)
        };
        q.push(vec![delete, checked_copy("IMG.JPG")]);
        assert_eq!(q.jobs[1].state, JobState::Queued);
        let effects = q.apply(vec![snap(1, Some(Ok(())))]);
        assert!(matches!(
            effects[..],
            [Effect::Finished { id: 1, .. }, Effect::Check { id: 2, .. }]
        ));
    }

    /// A retried job waits for a later one that changes its entry and is under way.
    #[test]
    fn a_retried_job_waits_for_a_later_one_under_way() {
        let mut q = Queue::new(3);
        q.push(vec![checked_copy("a"), checked_copy("A")]);
        q.checked(1, 1, Err("a already exists there".into()));
        assert_eq!(q.jobs[1].state, JobState::Checking);
        assert_eq!(q.retry(1), vec![]);
        q.checked(2, 1, Ok(()));
        let effects = q.apply(vec![snap(2, Some(Ok(())))]);
        assert!(matches!(
            effects[..],
            [Effect::Finished { id: 2, .. }, Effect::Check { id: 1, check: 2, .. }]
        ));
    }

    #[test]
    fn touched_directories() {
        assert_eq!(t(OpKind::Copy, "a", false).touched(), vec![Location::new("nas:", "backup")]);
        assert_eq!(t(OpKind::Move, "a", false).touched().len(), 2);
        assert_eq!(t(OpKind::Delete, "a", false).touched(), vec![Location::new("/", "home/sky")]);
    }
}
