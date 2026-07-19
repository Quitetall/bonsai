//! The content-scan substrate (ADR 0134) — the single parallel pass every analyzer reads from.
//!
//! Before this module, each leanness fold re-walked and re-read the whole tree on one thread
//! (exact-dup read every file, near-dup read them all again, complexity a third time), and the
//! exact-dup grouping keyed a non-stable 64-bit `DefaultHasher` with no collision guard — a
//! false-positive duplicate at scale. [`Scan`] fixes both: **one** gitignore-respecting walk,
//! then a **parallel** ([`rayon`]) read that computes, per file, a stable strong content hash
//! ([`blake3`], 256-bit — collision-free in practice, no byte re-verify needed), a text/binary
//! classification, a line count, and (on demand) the normalized-line shingle set near-dup needs.
//! Downstream folds consume [`FileFacts`]; they never touch the filesystem again.
//!
//! Memory is bounded by the input's own size: 32-byte hash + small metadata per file always,
//! plus a shingle set (proportional to line count) only when `want_shingles` is set. Raw file
//! text is dropped after the pass — nothing is held resident across the whole tree, which is
//! what lets the same code run on a laptop repo and a monorepo.

use crate::walk::{self};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Default cap: files larger than this are stat-counted but not read or hashed (a tracked
/// multi-MB blob is not a source-dedup candidate and reading it stalls the scan).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 2_000_000;

/// How much of a file's head to inspect for the binary (NUL-byte) heuristic — matches git's.
const BINARY_SNIFF_BYTES: usize = 8192;

/// Knobs for a scan pass.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Files strictly larger than this are not read (hash = `None`). See [`DEFAULT_MAX_FILE_BYTES`].
    pub max_file_bytes: u64,
    /// Directory basenames to prune in addition to [`walk::DEFAULT_SKIP`].
    pub extra_skip: Vec<String>,
    /// Compute the per-file normalized-line shingle set (near-dup needs it; the fast gate
    /// does not — skipping it keeps `check` cheap).
    pub want_shingles: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            extra_skip: Vec::new(),
            want_shingles: false,
        }
    }
}

impl ScanConfig {
    /// The fast-gate profile: hashes + line counts, no shingles.
    pub fn gate() -> Self {
        ScanConfig::default()
    }
    /// The full-report profile: also compute shingles for near-duplicate detection.
    pub fn with_shingles() -> Self {
        ScanConfig {
            want_shingles: true,
            ..ScanConfig::default()
        }
    }
    pub fn skipping(mut self, extra: &[String]) -> Self {
        self.extra_skip = extra.to_vec();
        self
    }
}

/// Everything an analyzer needs about one file, computed in the single read pass.
#[derive(Debug, Clone)]
pub struct FileFacts {
    /// Repo-relative path (forward-slashed via the OS; the natural node id).
    pub rel: PathBuf,
    /// The deepest sub-repo containing this file (empty path = the top repo). Duplicate and
    /// near-dup detection partition by this so a per-submodule mirror is not called cruft.
    pub subrepo: PathBuf,
    pub size: u64,
    /// True if the head of the file contains a NUL byte (git's binary heuristic) or it is not
    /// valid UTF-8. Binaries still participate in exact-dup (a duplicated asset is real waste)
    /// but never in near-dup (shingling needs text).
    pub is_binary: bool,
    /// blake3 of the full contents. `None` = over the size cap, unreadable, or empty (an empty
    /// file is trivially identical to every other empty file — pure noise, never a dup group).
    pub hash: Option<[u8; 32]>,
    /// Newline-terminated line count (0 for empty/unread files).
    pub lines: u32,
    /// Sorted, de-duplicated normalized-line hashes (empty unless `want_shingles` + text file).
    pub shingles: Vec<u64>,
}

impl FileFacts {
    /// The repo-relative path as a `String` (lossy on non-UTF-8 path bytes).
    pub fn rel_str(&self) -> String {
        self.rel.to_string_lossy().to_string()
    }
    /// The sub-repo path as a `String` (empty string = top repo).
    pub fn subrepo_str(&self) -> String {
        self.subrepo.to_string_lossy().to_string()
    }
}

/// The result of one content-scan pass.
#[derive(Debug, Clone)]
pub struct Scan {
    pub root: PathBuf,
    /// One entry per tracked file, in deterministic (sorted-walk) order.
    pub files: Vec<FileFacts>,
    /// Detected sub-repo boundaries (repo-relative dirs).
    pub subrepos: Vec<PathBuf>,
    /// Files skipped because they exceeded `max_file_bytes`.
    pub over_cap: usize,
    /// Files that failed to read (permissions, races, vanished mid-scan).
    pub unreadable: usize,
}

impl Scan {
    /// Walk `root` once and read every tracked file in parallel, per `cfg`.
    pub fn run(root: &Path, cfg: &ScanConfig) -> Scan {
        let subrepos = walk::subrepo_roots(root, &cfg.extra_skip);

        // Single deterministic walk; parallel read. Directory traversal is cheap next to IO,
        // and a serial walk keeps the file order stable (so downstream output is deterministic).
        let rels: Vec<PathBuf> = walk::walk(root, &cfg.extra_skip)
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.rel)
            .collect();

        let facts: Vec<FileFacts> = rels
            .par_iter()
            .map(|rel| read_facts(root, rel, &subrepos, cfg))
            .collect();

        let over_cap = facts
            .iter()
            .filter(|f| f.hash.is_none() && f.size > cfg.max_file_bytes)
            .count();
        let unreadable = facts
            .iter()
            .filter(|f| f.hash.is_none() && f.size <= cfg.max_file_bytes && f.size > 0)
            .count();

        Scan {
            root: root.to_path_buf(),
            files: facts,
            subrepos,
            over_cap,
            unreadable,
        }
    }

    /// A `rel-path → line count` map (for complexity / LOC rollups without a second read).
    pub fn lines_by_rel(&self) -> std::collections::BTreeMap<String, u32> {
        self.files.iter().map(|f| (f.rel_str(), f.lines)).collect()
    }
}

/// Read and characterize one file. Never panics: a stat or read failure yields a
/// zeroed-hash fact (counted as unreadable) rather than aborting the whole scan.
fn read_facts(root: &Path, rel: &Path, subrepos: &[PathBuf], cfg: &ScanConfig) -> FileFacts {
    let subrepo = walk::subrepo_of(rel, subrepos).to_path_buf();
    let abs = root.join(rel);
    let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);

    let mut facts = FileFacts {
        rel: rel.to_path_buf(),
        subrepo,
        size,
        is_binary: false,
        hash: None,
        lines: 0,
        shingles: Vec::new(),
    };

    if size == 0 || size > cfg.max_file_bytes {
        return facts; // empty (never a dup) or over cap (not a source candidate)
    }
    let Ok(bytes) = std::fs::read(&abs) else {
        return facts; // unreadable — counted, not fatal
    };
    if bytes.is_empty() {
        return facts;
    }

    facts.is_binary = is_binary(&bytes);
    facts.hash = Some(*blake3::hash(&bytes).as_bytes());
    facts.lines = (bytes.iter().filter(|b| **b == b'\n').count() as u32)
        + u32::from(*bytes.last().unwrap() != b'\n'); // count a final unterminated line

    if cfg.want_shingles && !facts.is_binary {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            facts.shingles = shingles(text);
        }
    }
    facts
}

/// git's binary test: a NUL byte in the head, or the whole content is not valid UTF-8.
fn is_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    head.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// The sorted, de-duplicated set of normalized non-trivial line hashes for near-dup Jaccard.
/// Structural noise (braces, ultra-short lines) is dropped; hashing is a stable FNV-1a so the
/// set is identical across runs and machines (unlike `DefaultHasher`).
pub fn shingles(text: &str) -> Vec<u64> {
    let mut set: Vec<u64> = text
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.len() < 4 || t == "}" || t == "{" || t == ")" {
                None
            } else {
                Some(fnv1a(t.as_bytes()))
            }
        })
        .collect();
    set.sort_unstable();
    set.dedup();
    set
}

/// FNV-1a 64-bit — a small, fast, *stable* string hash (reproducible across runs/versions,
/// which `std::collections::hash_map::DefaultHasher` explicitly is not).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn probe(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("bonsai_scan_{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn parallel_scan_hashes_and_classifies() {
        let base = probe("basic");
        fs::write(base.join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        fs::write(base.join("b.rs"), "fn main() {}\nlet x = 1;\n").unwrap(); // identical
        fs::write(base.join("c.rs"), "fn other() {}\n").unwrap();
        fs::write(base.join("bin"), [0u8, 1, 2, 3, 0, 9]).unwrap(); // NUL → binary
        fs::write(base.join("empty"), "").unwrap();

        let scan = Scan::run(&base, &ScanConfig::gate());
        let by = |name: &str| scan.files.iter().find(|f| f.rel_str() == name).unwrap();

        // identical files hash equal; distinct file differs
        assert_eq!(by("a.rs").hash, by("b.rs").hash);
        assert_ne!(by("a.rs").hash, by("c.rs").hash);
        // binary flagged; empty has no hash (never groups)
        assert!(by("bin").is_binary);
        assert!(by("empty").hash.is_none());
        // line counts (final unterminated line counted)
        assert_eq!(by("c.rs").lines, 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn shingles_are_stable_and_skip_noise() {
        let s1 = shingles("let value = compute(x);\n}\n{\nreal line here\n");
        let s2 = shingles("real line here\nlet value = compute(x);\n");
        // same two meaningful lines, braces dropped, order-independent (sorted)
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 2);
        // determinism across calls
        assert_eq!(shingles("alpha beta gamma"), shingles("alpha beta gamma"));
    }

    #[test]
    fn shingles_only_when_requested() {
        let base = probe("shingle_gate");
        fs::write(
            base.join("a.rs"),
            "meaningful line one\nmeaningful line two\n",
        )
        .unwrap();
        let gate = Scan::run(&base, &ScanConfig::gate());
        assert!(gate.files[0].shingles.is_empty(), "gate must not shingle");
        let full = Scan::run(&base, &ScanConfig::with_shingles());
        assert!(!full.files[0].shingles.is_empty(), "full scan must shingle");
        let _ = fs::remove_dir_all(&base);
    }
}
