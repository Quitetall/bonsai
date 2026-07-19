//! Scale benchmark (ADR 0134). Generates a synthetic repo of N files and times a full scan,
//! proving the parallel substrate handles monorepo-order trees. `#[ignore]` so it never slows
//! the normal suite; run explicitly:
//!
//! ```sh
//! cargo test --release --test scale -- --ignored --nocapture
//! ```
//!
//! It asserts only a generous upper bound (so it can't flake in CI) and prints the real
//! throughput (files/s, MiB/s) for the record. The second timed pass exercises the incremental
//! cache — on an unchanged tree every file should hit, so the warm pass is dramatically cheaper.

use bonsai::scan::{Scan, ScanConfig};
use std::path::Path;
use std::time::Instant;
use tempfile::tempdir;

/// Generate `n` realistically-sized (~1.5 KB) source files across `dirs` directories under
/// `root`, returning total bytes. Real repos are KB-scale files, not 100-byte stubs — that is
/// the regime where reading dominates and the incremental cache actually pays off.
fn generate(root: &Path, n: usize, dirs: usize) -> u64 {
    let mut total = 0u64;
    for i in 0..n {
        let d = root.join(format!("pkg{:03}/mod{:02}", i % dirs, (i / dirs) % 40));
        std::fs::create_dir_all(&d).unwrap();
        // a ~1.5 KB body: a unique header + a block of varied lines (distinct hashes per file)
        let mut body = format!("//! module {i}\n\npub struct Item{i} {{ id: u64 }}\n\n");
        for j in 0..30 {
            body.push_str(&format!(
                "pub fn op_{i}_{j}(x: i64) -> i64 {{ let y = x * {} + {}; compute(y) }}\n",
                (i + j) % 97,
                (i * 7 + j) % 31
            ));
        }
        std::fs::write(d.join(format!("f{i}.rs")), &body).unwrap();
        total += body.len() as u64;
    }
    total
}

#[test]
#[ignore = "scale benchmark; run with --ignored --nocapture"]
fn scan_scales_to_many_files() {
    let n: usize = std::env::var("BONSAI_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000);
    let dir = tempdir().unwrap();
    let root = dir.path();

    let gen_start = Instant::now();
    let total_bytes = generate(root, n, 64);
    let gen_s = gen_start.elapsed().as_secs_f64();
    eprintln!(
        "generated {n} files ({:.1} MiB) in {gen_s:.2}s",
        total_bytes as f64 / 1e6
    );

    // cold scan (no cache): the parallel read + blake3 pass
    let cache = ScanConfig::default_cache_path(root);
    let cold_cfg = ScanConfig::gate().with_cache(cache.clone());
    let t = Instant::now();
    let cold = Scan::run(root, &cold_cfg);
    let cold_s = t.elapsed().as_secs_f64();
    assert_eq!(cold.files.len(), n, "every generated file must be scanned");
    assert_eq!(cold.cache_hits, 0, "cold scan has no cache hits");
    eprintln!(
        "COLD  scan: {n} files in {cold_s:.3}s  ({:.0} files/s, {:.1} MiB/s)",
        n as f64 / cold_s,
        total_bytes as f64 / 1e6 / cold_s
    );

    // warm scan (cache hit on every unchanged file)
    let t = Instant::now();
    let warm = Scan::run(root, &cold_cfg);
    let warm_s = t.elapsed().as_secs_f64();
    assert_eq!(warm.cache_hits, n, "warm scan should hit every file");
    eprintln!(
        "WARM  scan: {n} files in {warm_s:.3}s  ({:.0} files/s)  [{:.1}x faster]",
        n as f64 / warm_s,
        cold_s / warm_s.max(1e-9)
    );

    // exact-dup grouping over the whole set (the fold downstream analyzers run)
    let t = Instant::now();
    let (_groups, _mirrors) = bonsai::lean::exact_dups(&cold);
    eprintln!(
        "dedup fold over {n} files in {:.3}s",
        t.elapsed().as_secs_f64()
    );

    // generous ceiling so this never flakes; the eprintln numbers are the real signal.
    assert!(
        cold_s < 60.0,
        "cold scan of {n} files took {cold_s:.1}s (>60s ceiling)"
    );
}
