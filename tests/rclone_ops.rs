//! File operations against the real embedded rclone, on the local and memory backends.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use rse::event::{BrowseOp, Event};
use rse::exec::Exec;
use rse::jobs::{Effect, JobState, OpKind, Queue, Transfer};
use rse::location::Location;
use rse::rclone::{Backend, Embedded, api};

struct Rig {
    backend: Arc<dyn Backend>,
    exec: Exec,
    rx: Receiver<Event>,
    queue: Queue,
}

impl Rig {
    fn new() -> Self {
        let backend: Arc<dyn Backend> = Arc::new(Embedded::new());
        let (tx, rx) = unbounded();
        let exec = Exec::spawn(backend.clone(), tx, 2, Duration::from_millis(50));
        Self { backend, exec, rx, queue: Queue::new(2) }
    }

    fn run_effects(&self, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Start { id, transfer, method, params } => {
                    self.exec.submit(id, *transfer, method, params);
                }
                Effect::Stop(id) => self.exec.stop(id),
                Effect::Check { .. } => unreachable!("approved transfers start without a check"),
                Effect::Finished { .. } => {}
            }
        }
    }

    /// Queues the transfers and waits until every job has finished.
    fn transfer(&mut self, transfers: Vec<Transfer>) -> Vec<JobState> {
        let first = self.queue.jobs.len();
        let effects = self.queue.push(transfers);
        self.run_effects(effects);
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.queue.jobs[first..].iter().any(|j| !j.state.is_finished()) {
            assert!(Instant::now() < deadline, "jobs did not finish");
            if let Ok(Event::Jobs(snaps)) = self.rx.recv_timeout(Duration::from_millis(200)) {
                let effects = self.queue.apply(snaps);
                self.run_effects(effects);
            }
        }
        self.queue.jobs[first..].iter().map(|j| j.state.clone()).collect()
    }

    fn op(&self, op: BrowseOp) -> Result<(), rse::rclone::RpcError> {
        self.exec.op(1, op);
        loop {
            if let Event::OpDone { result, .. } =
                self.rx.recv_timeout(Duration::from_secs(30)).expect("op result")
            {
                return result;
            }
        }
    }

    fn names(&self, loc: &Location) -> Vec<String> {
        let mut v: Vec<String> = api::list(self.backend.as_ref(), &loc.fs, &loc.path)
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        v.sort();
        v
    }
}

fn local(p: &Path) -> Location {
    Location::local(p)
}

fn t(kind: OpKind, src: &Location, name: &str, is_dir: bool, dst: Option<&Location>) -> Transfer {
    Transfer {
        kind,
        src_dir: src.clone(),
        name: name.into(),
        is_dir,
        dst_dir: dst.cloned(),
        size: None,
        // As if the user chose to overwrite: no check of the target first.
        overwrite: true,
    }
}

fn tree(root: &Path) {
    fs::create_dir_all(root.join("d/sub")).unwrap();
    fs::create_dir_all(root.join("d/empty")).unwrap();
    fs::write(root.join("a.txt"), "alpha").unwrap();
    fs::write(root.join("d/one.txt"), "one").unwrap();
    fs::write(root.join("d/sub/two.txt"), "two").unwrap();
}

#[test]
fn copy_file_and_directory_locally() {
    let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    tree(src.path());
    let mut rig = Rig::new();
    let states = rig.transfer(vec![
        t(OpKind::Copy, &local(src.path()), "a.txt", false, Some(&local(dst.path()))),
        t(OpKind::Copy, &local(src.path()), "d", true, Some(&local(dst.path()))),
    ]);
    assert_eq!(states, [JobState::Done, JobState::Done]);
    assert_eq!(fs::read_to_string(dst.path().join("a.txt")).unwrap(), "alpha");
    assert_eq!(fs::read_to_string(dst.path().join("d/sub/two.txt")).unwrap(), "two");
    assert!(dst.path().join("d/empty").is_dir(), "createEmptySrcDirs");
    assert!(src.path().join("a.txt").exists(), "copy keeps the source");
}

#[test]
fn copy_to_memory_backend_and_back() {
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let mut rig = Rig::new();
    let mem = Location::new(":memory:", "bucket-copy");
    let states = rig.transfer(vec![
        t(OpKind::Copy, &local(src.path()), "d", true, Some(&mem)),
        t(OpKind::Copy, &local(src.path()), "a.txt", false, Some(&mem)),
    ]);
    assert_eq!(states, [JobState::Done, JobState::Done]);
    assert_eq!(rig.names(&mem), ["a.txt", "d"]);
    assert_eq!(rig.names(&mem.join("d").join("sub")), ["two.txt"]);

    let back = tempfile::tempdir().unwrap();
    let states = rig.transfer(vec![t(OpKind::Copy, &mem, "d", true, Some(&local(back.path())))]);
    assert_eq!(states, [JobState::Done]);
    assert_eq!(fs::read_to_string(back.path().join("d/one.txt")).unwrap(), "one");
}

#[test]
fn move_file_and_directory_removes_the_source() {
    let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    tree(src.path());
    let mut rig = Rig::new();
    let states = rig.transfer(vec![
        t(OpKind::Move, &local(src.path()), "a.txt", false, Some(&local(dst.path()))),
        t(OpKind::Move, &local(src.path()), "d", true, Some(&local(dst.path()))),
    ]);
    assert_eq!(states, [JobState::Done, JobState::Done]);
    assert_eq!(fs::read_to_string(dst.path().join("d/sub/two.txt")).unwrap(), "two");
    assert!(dst.path().join("d/empty").is_dir());
    assert!(!src.path().join("a.txt").exists());
    let left: Vec<_> = fs::read_dir(src.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(left.is_empty(), "source not emptied: {left:?}");
}

#[test]
fn move_between_backends() {
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let mut rig = Rig::new();
    let mem = Location::new(":memory:", "bucket-move");
    let states = rig.transfer(vec![t(OpKind::Move, &local(src.path()), "d", true, Some(&mem))]);
    assert_eq!(states, [JobState::Done]);
    assert_eq!(rig.names(&mem.join("d")), ["one.txt", "sub"]);
    assert!(!src.path().join("d/one.txt").exists());
    // rclone leaves the emptied source root; rse removes it afterwards.
    assert!(src.path().join("d").is_dir());
    rig.op(BrowseOp::RemoveEmptyDir(local(src.path()).join("d"))).unwrap();
    assert!(!src.path().join("d").exists());
    // Already gone is fine too.
    rig.op(BrowseOp::RemoveEmptyDir(local(src.path()).join("d"))).unwrap();
}

#[test]
fn remove_empty_dir_refuses_a_non_empty_directory() {
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let rig = Rig::new();
    assert!(rig.op(BrowseOp::RemoveEmptyDir(local(src.path()).join("d"))).is_err());
    assert_eq!(fs::read_to_string(src.path().join("d/one.txt")).unwrap(), "one");
}

#[test]
fn delete_file_and_purge_directory() {
    let src = tempfile::tempdir().unwrap();
    tree(src.path());
    let mut rig = Rig::new();
    let states = rig.transfer(vec![
        t(OpKind::Delete, &local(src.path()), "a.txt", false, None),
        t(OpKind::Delete, &local(src.path()), "d", true, None),
    ]);
    assert_eq!(states, [JobState::Done, JobState::Done]);
    assert!(!src.path().join("a.txt").exists());
    assert!(!src.path().join("d").exists());
}

#[test]
fn directory_copy_merges_into_existing() {
    let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    tree(src.path());
    fs::create_dir_all(dst.path().join("d")).unwrap();
    fs::write(dst.path().join("d/one.txt"), "OLD CONTENT").unwrap();
    fs::write(dst.path().join("d/keep.txt"), "untouched").unwrap();
    let mut rig = Rig::new();
    let states = rig.transfer(vec![t(
        OpKind::Copy,
        &local(src.path()),
        "d",
        true,
        Some(&local(dst.path())),
    )]);
    assert_eq!(states, [JobState::Done]);
    assert_eq!(
        fs::read_to_string(dst.path().join("d/one.txt")).unwrap(),
        "one",
        "different file overwritten"
    );
    assert_eq!(
        fs::read_to_string(dst.path().join("d/keep.txt")).unwrap(),
        "untouched",
        "extra file kept"
    );
}

#[test]
fn copy_overwrites_an_existing_file() {
    let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(src.path().join("f"), "new").unwrap();
    fs::write(dst.path().join("f"), "old-and-longer").unwrap();
    let mut rig = Rig::new();
    let states = rig.transfer(vec![t(
        OpKind::Copy,
        &local(src.path()),
        "f",
        false,
        Some(&local(dst.path())),
    )]);
    assert_eq!(states, [JobState::Done]);
    assert_eq!(fs::read_to_string(dst.path().join("f")).unwrap(), "new");
}

#[test]
fn failures_carry_rclones_error() {
    let src = tempfile::tempdir().unwrap();
    let mut rig = Rig::new();
    let states = rig.transfer(vec![t(
        OpKind::Copy,
        &local(src.path()),
        "missing.txt",
        false,
        Some(&Location::new(":memory:", "b")),
    )]);
    let [JobState::Failed(e)] = states.as_slice() else { panic!("{states:?}") };
    assert!(e.contains("not found"), "{e}");
}

#[test]
fn mkdir_and_rename_on_the_browse_workers() {
    let root = tempfile::tempdir().unwrap();
    tree(root.path());
    let rig = Rig::new();
    let here = local(root.path());
    rig.op(BrowseOp::Mkdir(here.join("made"))).unwrap();
    assert!(root.path().join("made").is_dir());
    rig.op(BrowseOp::Rename { from: here.join("a.txt"), to: here.join("b.txt"), is_dir: false })
        .unwrap();
    assert_eq!(fs::read_to_string(root.path().join("b.txt")).unwrap(), "alpha");
    rig.op(BrowseOp::Rename { from: here.join("d"), to: here.join("e"), is_dir: true }).unwrap();
    assert_eq!(fs::read_to_string(root.path().join("e/sub/two.txt")).unwrap(), "two");
    assert!(root.path().join("e/empty").is_dir());
    assert!(!root.path().join("d").exists(), "old directory left behind");

    // Renaming on a bucket backend works too.
    let mem = Location::new(":memory:", "bucket-rename");
    rig.op(BrowseOp::Mkdir(mem.clone())).unwrap();
    api::mkdir(rig.backend.as_ref(), &mem.fs, &format!("{}/x", mem.path)).unwrap();
    let err = rig.op(BrowseOp::Rename { from: mem.join("nope"), to: mem.join("y"), is_dir: false });
    assert!(err.is_err());
}

#[test]
fn listing_reports_errors_for_missing_directories() {
    let rig = Rig::new();
    let err = api::list(rig.backend.as_ref(), "/", "definitely/not/here/x1").unwrap_err();
    assert!(err.message.contains("not found"), "{err:?}");
}
