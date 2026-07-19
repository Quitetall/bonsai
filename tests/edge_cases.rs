//! Edge-case battery for the scan substrate (ADR 0134). A structure tool that runs on real —
//! and monorepo-scale — trees meets symlinks, oversized blobs, binary files, non-UTF-8 names,
//! and empty repos. Each test plants one of those and asserts Bonsai does the sane thing:
//! never panics, never invents a duplicate, never follows a symlink, and stays deterministic.

use bonsai::scan::{Scan, ScanConfig};
use std::fs;
use tempfile::tempdir;

/// A symlink pointing at a tracked file must NOT be read as a duplicate of its target, and a
/// symlink to a directory (or a cycle) must not be followed.
#[test]
#[cfg(unix)]
fn symlinks_are_not_followed_or_duplicated() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("real.rs"), "pub fn a() { work(); }\n").unwrap();
    symlink(root.join("real.rs"), root.join("link.rs")).unwrap(); // symlink to the file
    symlink(root.join("real.rs"), root.join("selfcycle")).ok(); // extra alias
    fs::create_dir(root.join("d")).unwrap();
    symlink(root.join("d"), root.join("dlink")).unwrap(); // symlink to a dir

    let scan = Scan::run(root, &ScanConfig::gate());
    // the symlink has no hash of its own → real.rs is not part of any dup group
    let (groups, mirrors) = bonsai::lean::exact_dups(&scan);
    assert!(
        groups.is_empty(),
        "symlink created a phantom duplicate: {groups:?}"
    );
    assert_eq!(mirrors, 0);
    // real.rs is present and hashed; the symlink entry (if walked) carries no hash
    let real = scan
        .files
        .iter()
        .find(|f| f.rel_str() == "real.rs")
        .unwrap();
    assert!(real.hash.is_some());
    for f in &scan.files {
        if f.rel_str() == "link.rs" {
            assert!(f.hash.is_none(), "symlink must not be hashed");
        }
    }
}

/// A file over the read cap is stat-counted (`over_cap`) but never hashed, so it can't form a
/// duplicate group — reading a multi-MB blob would stall the scan for no signal.
#[test]
fn oversized_files_are_capped_not_read() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let big = "x".repeat(5000);
    fs::write(root.join("big1.dat"), &big).unwrap();
    fs::write(root.join("big2.dat"), &big).unwrap(); // identical + oversized
    fs::write(root.join("small.rs"), "fn a() {}\n").unwrap();

    let scan = Scan::run(root, &ScanConfig::gate().max_bytes(1000));
    assert_eq!(scan.over_cap, 2, "both big files should be over cap");
    let (groups, _) = bonsai::lean::exact_dups(&scan);
    assert!(
        groups.is_empty(),
        "capped files must not form a dup group: {groups:?}"
    );
    // the small file is still hashed
    assert!(scan
        .files
        .iter()
        .find(|f| f.rel_str() == "small.rs")
        .unwrap()
        .hash
        .is_some());
}

/// Two byte-identical BINARY files ARE a duplicate (a duplicated asset is real waste), but a
/// binary file never contributes shingles (near-dup needs text).
#[test]
fn identical_binaries_are_duplicates_but_never_shingled() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let blob: Vec<u8> = (0..256u16).map(|b| (b % 256) as u8).collect(); // contains NUL
    fs::write(root.join("a.bin"), &blob).unwrap();
    fs::write(root.join("b.bin"), &blob).unwrap();

    let scan = Scan::run(root, &ScanConfig::with_shingles());
    let (groups, _) = bonsai::lean::exact_dups(&scan);
    assert_eq!(
        groups.len(),
        1,
        "identical binaries should be one dup group"
    );
    assert_eq!(groups[0].files.len(), 2);
    for f in &scan.files {
        assert!(f.is_binary, "both files are binary");
        assert!(f.shingles.is_empty(), "binaries must not be shingled");
    }
}

/// A non-UTF-8 filename must not panic the scan (paths are OS bytes on unix).
#[test]
#[cfg(unix)]
fn non_utf8_filename_does_not_panic() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = tempdir().unwrap();
    let root = dir.path();
    let name = OsStr::from_bytes(b"weird-\xff\xfe.rs"); // invalid UTF-8
    fs::write(root.join(name), "fn a() {}\n").unwrap();
    fs::write(root.join("ok.rs"), "fn b() {}\n").unwrap();

    let scan = Scan::run(root, &ScanConfig::gate()); // must not panic
    assert!(scan.files.iter().any(|f| f.rel_str().contains("ok.rs")));
    assert!(scan.files.len() >= 2);
}

/// An empty repo (or one whose only content is ignored) scans cleanly to zero findings.
#[test]
fn empty_repo_yields_no_findings() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join(".gitignore"), "build/\n").unwrap();
    fs::create_dir(root.join("build")).unwrap();
    fs::write(root.join("build/artifact.o"), [0u8, 1, 2]).unwrap(); // ignored

    let scan = Scan::run(root, &ScanConfig::with_shingles());
    let (groups, mirrors) = bonsai::lean::exact_dups(&scan);
    assert!(groups.is_empty() && mirrors == 0);
    // .gitignore itself is the only visible tracked file; build/ is invisible
    assert!(!scan
        .files
        .iter()
        .any(|f| f.rel_str().contains("artifact.o")));
}

/// Two independent scans of an unchanged tree produce identical file order and identical
/// hashes — determinism is load-bearing for the ratchet and for reproducible CI output.
#[test]
fn scan_is_deterministic() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    for i in 0..50 {
        fs::create_dir_all(root.join(format!("m{}", i % 7))).unwrap();
        fs::write(
            root.join(format!("m{}/f{i}.rs", i % 7)),
            format!("fn f{i}() {{ compute({i}); }}\n"),
        )
        .unwrap();
    }
    let a = Scan::run(root, &ScanConfig::with_shingles());
    let b = Scan::run(root, &ScanConfig::with_shingles());
    let key = |s: &Scan| -> Vec<(String, Option<[u8; 32]>)> {
        s.files.iter().map(|f| (f.rel_str(), f.hash)).collect()
    };
    assert_eq!(
        key(&a),
        key(&b),
        "scan output must be identical across runs"
    );
}
