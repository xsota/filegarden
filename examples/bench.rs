//! Watcher benchmarks used for the numbers in the README.
//!
//!     cargo run --release --example bench [-- FILES]
//!
//! Creates a temporary project with FILES files (default 10 000) and measures the initial
//! scan, change-to-event latency, and burst throughput.

use std::fs;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use filegarden::change::{ChangeKind, WatchMsg};
use filegarden::watcher::{self, filter::Filter, WatchConfig};

fn main() {
    let files: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    let per_dir = 100;
    for i in 0..files {
        let d = root.join(format!("pkg{:03}/src", i / per_dir));
        if i % per_dir == 0 {
            fs::create_dir_all(&d).expect("mkdir");
        }
        fs::write(d.join(format!("file{:03}.ts", i % per_dir)), "export const x = 1;\n").expect("write");
    }
    println!("project: {files} files in {} directories", files.div_ceil(per_dir) * 2);

    let mut cfg = WatchConfig::new(root.clone());
    cfg.filter = Filter::with_defaults(&[], true).expect("filter");
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let _handle = watcher::spawn(cfg, move |m| tx.send(m).is_ok()).expect("watch");
    match rx.recv().expect("snapshot") {
        WatchMsg::Snapshot { entries, elapsed, .. } => println!(
            "initial scan: {} entries in {:.1} ms (ready after {:.1} ms)",
            entries.len(),
            elapsed.as_secs_f64() * 1e3,
            started.elapsed().as_secs_f64() * 1e3
        ),
        other => panic!("unexpected {other:?}"),
    }
    // Drain the startup reconcile.
    while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}

    // Latency: one modification at a time.
    let mut samples = Vec::new();
    for i in 0..60 {
        let path = root.join(format!("pkg{:03}/src/file{:03}.ts", (i * 7) % (files / per_dir), i % per_dir));
        let t0 = Instant::now();
        fs::write(&path, format!("export const x = {i};\n")).expect("write");
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
                WatchMsg::Changes(c) if c.iter().any(|c| c.kind == ChangeKind::Modify) => break,
                _ => {}
            }
        }
        samples.push(t0.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(30));
    }
    samples.sort_by(f64::total_cmp);
    let pct = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize];
    println!(
        "change -> event latency: median {:.0} ms, p95 {:.0} ms, max {:.0} ms (aggregation window 100 ms)",
        pct(0.5),
        pct(0.95),
        pct(1.0)
    );

    // Burst: many new files as fast as possible.
    let burst = 5_000;
    let t0 = Instant::now();
    fs::create_dir_all(root.join("burst")).expect("mkdir");
    for i in 0..burst {
        fs::write(root.join(format!("burst/f{i:05}.ts")), "x").expect("write");
    }
    let written = t0.elapsed();
    let mut seen = 0;
    let mut batches = 0;
    while seen < burst {
        if let WatchMsg::Changes(c) = rx.recv_timeout(Duration::from_secs(10)).expect("burst events") {
            batches += 1;
            seen += c.iter().filter(|c| c.kind == ChangeKind::Create && !c.is_dir).count();
        }
    }
    println!(
        "burst: {burst} files written in {:.0} ms, all delivered after {:.0} ms in {batches} batches",
        written.as_secs_f64() * 1e3,
        t0.elapsed().as_secs_f64() * 1e3
    );
}
