//! Phase 0 experiment, kept as a regression test: can `RcloneRPC` be called from several
//! threads at once without one slow call blocking another?
#![cfg(feature = "embedded")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rext::rclone::{Backend, Embedded, api};
use serde_json::json;

/// Builds a directory with many files so `operations/list` takes a while.
fn big_dir(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        std::fs::write(dir.path().join(format!("file-{i:06}.txt")), b"x").unwrap();
    }
    dir
}

#[test]
fn stats_latency_is_independent_of_concurrent_listing() {
    let backend: Arc<dyn Backend> = Arc::new(Embedded::new());
    let dir = big_dir(20_000);
    let path = dir.path().to_str().unwrap().to_owned();

    // Baseline: core/stats latency with nothing else running.
    let mut idle = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        api::stats(backend.as_ref(), "").unwrap();
        idle.push(t.elapsed());
    }

    let stop = Arc::new(AtomicBool::new(false));
    let lister = {
        let (backend, stop, path) = (backend.clone(), stop.clone(), path.clone());
        thread::spawn(move || {
            let mut lists = 0u32;
            let mut longest = Duration::ZERO;
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                let items = api::list(backend.as_ref(), &path, "").unwrap();
                longest = longest.max(t.elapsed());
                assert_eq!(items.len(), 20_000);
                lists += 1;
            }
            (lists, longest)
        })
    };

    let mut busy = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let t = Instant::now();
        api::stats(backend.as_ref(), "").unwrap();
        busy.push(t.elapsed());
        thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::SeqCst);
    let (lists, longest_list) = lister.join().unwrap();

    let idle_max = idle.iter().max().copied().unwrap_or_default();
    let busy_max = busy.iter().max().copied().unwrap_or_default();
    eprintln!(
        "lists={lists} longest_list={longest_list:?} stats idle_max={idle_max:?} busy_max={busy_max:?} samples={}",
        busy.len()
    );
    assert!(lists >= 1);
    // A blocking implementation would make core/stats wait for a whole listing.
    assert!(
        busy_max < Duration::from_millis(50) && busy_max < longest_list,
        "core/stats took {busy_max:?} while a listing took {longest_list:?}"
    );
}

#[test]
fn many_threads_get_their_own_results() {
    let backend: Arc<dyn Backend> = Arc::new(Embedded::new());
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let backend = backend.clone();
            thread::spawn(move || {
                for i in 0..10_000 {
                    let out = api::noop(backend.as_ref(), json!({ "t": t, "i": i })).unwrap();
                    assert_eq!(out, json!({ "t": t, "i": i }));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}
