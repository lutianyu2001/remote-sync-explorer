//! Cancelling transfers part-way. Its own test binary because it sets rclone's global
//! bandwidth limit to keep transfers slow enough to interrupt.
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use rext::event::Event;
use rext::exec::Exec;
use rext::jobs::{Effect, JobState, OpKind, Queue, Transfer};
use rext::location::Location;
use rext::rclone::{Backend, Embedded, api};
use serde_json::json;

const SIZE: usize = 64 << 20;

fn big_file(path: &Path) {
    let mut f = fs::File::create(path).unwrap();
    let mut block = vec![0u8; 1 << 20];
    for i in 0..SIZE >> 20 {
        // Not all zeros, so nothing can shortcut the copy.
        block
            .iter_mut()
            .enumerate()
            .for_each(|(j, b)| *b = u8::try_from((i * 31 + j) % 251).unwrap());
        f.write_all(&block).unwrap();
    }
}

fn setup() -> (Arc<dyn Backend>, Exec, Receiver<Event>) {
    let backend: Arc<dyn Backend> = Arc::new(Embedded::new());
    backend.rpc("core/bwlimit", json!({ "rate": "8M" })).unwrap();
    let (tx, rx) = unbounded();
    let exec = Exec::spawn(backend.clone(), tx, 1, Duration::from_millis(100));
    (backend, exec, rx)
}

fn run(exec: &Exec, effects: Vec<Effect>) {
    for e in effects {
        match e {
            Effect::Start { id, method, params } => exec.submit(id, method, params),
            Effect::Stop(id) => exec.stop(id),
            Effect::Check { .. } => unreachable!("approved transfers start without a check"),
            Effect::Finished { .. } => {}
        }
    }
}

/// Starts the transfer, waits until some bytes have moved, cancels, and waits for the end.
fn cancel_midway(exec: &Exec, rx: &Receiver<Event>, transfer: Transfer) -> (JobState, i64) {
    let mut q = Queue::new(1);
    run(exec, q.push(vec![transfer]));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut cancelled = false;
    let mut seen = 0;
    while !q.jobs[0].state.is_finished() {
        assert!(Instant::now() < deadline, "transfer did not end");
        if let Ok(Event::Jobs(snaps)) = rx.recv_timeout(Duration::from_millis(500)) {
            let effects = q.apply(snaps);
            run(exec, effects);
            seen = seen.max(q.jobs[0].stats.bytes);
            if !cancelled && q.jobs[0].stats.bytes > 4 << 20 {
                run(exec, q.cancel(q.jobs[0].id));
                cancelled = true;
            }
        }
    }
    assert!(cancelled, "finished before it could be cancelled");
    (q.jobs[0].state.clone(), seen)
}

/// A local copy target. On macOS rclone clones files within a volume instead of copying
/// them, which is instant and ignores the bandwidth limit, so turn cloning off.
fn target(dir: &Path) -> Location {
    Location::new(":local,no_clone:", dir.to_string_lossy())
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = walk(dir).into_iter().collect();
    v.sort();
    v
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if e.path().is_dir() {
            out.extend(walk(&e.path()).into_iter().map(|n| format!("{name}/{n}")));
        } else {
            out.push(name);
        }
    }
    out
}

#[test]
fn cancelled_transfers_leave_no_partial_files() {
    let (backend, exec, rx) = setup();
    let src = tempfile::tempdir().unwrap();
    big_file(&src.path().join("big.bin"));
    fs::create_dir(src.path().join("dir")).unwrap();
    big_file(&src.path().join("dir/inner.bin"));
    let s = Location::local(src.path());

    // File copy, local to local.
    let dst = tempfile::tempdir().unwrap();
    let t = Transfer {
        kind: OpKind::Copy,
        src_dir: s.clone(),
        name: "big.bin".into(),
        is_dir: false,
        dst_dir: Some(target(dst.path())),
        size: None,
        overwrite: true,
    };
    let (state, seen) = cancel_midway(&exec, &rx, t);
    eprintln!("file copy: {state:?} after {seen} bytes; target: {:?}", listing(dst.path()));
    assert_eq!(state, JobState::Cancelled);
    assert!(seen < i64::try_from(SIZE).unwrap());
    assert_eq!(listing(dst.path()), Vec::<String>::new(), "target not clean");

    // Directory copy (sync/copy).
    let dst = tempfile::tempdir().unwrap();
    let t = Transfer {
        kind: OpKind::Copy,
        src_dir: s.clone(),
        name: "dir".into(),
        is_dir: true,
        dst_dir: Some(target(dst.path())),
        size: None,
        overwrite: true,
    };
    let (state, _) = cancel_midway(&exec, &rx, t);
    let left = listing(dst.path());
    eprintln!("dir copy: {state:?}; target: {left:?}");
    assert_eq!(state, JobState::Cancelled);
    assert!(
        left.iter().all(|n| !n.contains("partial") && !n.ends_with("inner.bin")),
        "target not clean: {left:?}"
    );

    // A cancelled move across backends keeps the source and leaves nothing at the target.
    // (A move within one local fs is a rename and cannot be caught midway.)
    let mem = Location::new(":memory:", "cancel-move");
    let t = Transfer {
        kind: OpKind::Move,
        src_dir: s,
        name: "big.bin".into(),
        is_dir: false,
        dst_dir: Some(mem.clone()),
        size: None,
        overwrite: true,
    };
    let (state, _) = cancel_midway(&exec, &rx, t);
    assert_eq!(state, JobState::Cancelled);
    assert_eq!(fs::metadata(src.path().join("big.bin")).unwrap().len(), SIZE as u64);
    let left = api::list(backend.as_ref(), &mem.fs, &mem.path).map_or(0, |l| l.len());
    assert_eq!(left, 0, "memory target not clean");

    exec.shutdown();
    assert!(api::version(backend.as_ref()).is_ok());
}

#[test]
fn shutdown_stops_running_jobs() {
    let (backend, exec, rx) = setup();
    let src = tempfile::tempdir().unwrap();
    big_file(&src.path().join("big.bin"));
    let dst = tempfile::tempdir().unwrap();
    let mut q = Queue::new(1);
    run(
        &exec,
        q.push(vec![Transfer {
            kind: OpKind::Copy,
            src_dir: Location::local(src.path()),
            name: "big.bin".into(),
            is_dir: false,
            dst_dir: Some(target(dst.path())),
            size: None,
            overwrite: true,
        }]),
    );
    // Wait until it is running, then drop the workers as rext does at exit.
    let Ok(Event::Jobs(_)) = rx.recv_timeout(Duration::from_secs(5)) else { panic!("no progress") };
    exec.shutdown();
    // The job worker stopped the job on its way out. Wait until rclone has ended it: every rc
    // call gets a job id, so look for the one that ended with a cancel.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let jobs = backend.rpc("job/list", json!({})).unwrap();
        let cancelled = jobs["finishedIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|id| api::job_status(backend.as_ref(), id.as_i64().unwrap()).ok())
            .filter(|s| !s.success && s.error.contains("cancel"))
            .count();
        if cancelled >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "no cancelled job: {jobs}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(listing(dst.path()), Vec::<String>::new());
    assert_eq!(fs::metadata(src.path().join("big.bin")).unwrap().len(), SIZE as u64);
}
