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

use crate::cache::{self, Cache, CacheEntry};
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
    /// Path to an incremental scan cache (`.bonsai/scan-cache.json`). When set, unchanged
    /// files (matching `(size, mtime)`) reuse their stored hash/line-count instead of being
    /// re-read; the refreshed cache is written back after the pass. `None` = no caching.
    /// Caching is skipped when `want_shingles` is set (shingles need a read regardless).
    pub cache_path: Option<PathBuf>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            extra_skip: Vec::new(),
            want_shingles: false,
            cache_path: None,
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
    /// Override the read cap (files larger than this are stat-counted but not hashed).
    pub fn max_bytes(mut self, n: u64) -> Self {
        self.max_file_bytes = n;
        self
    }
    /// Enable the incremental cache at `path`.
    pub fn with_cache(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }
    /// The conventional cache location under a repo root.
    pub fn default_cache_path(root: &Path) -> PathBuf {
        root.join(".bonsai").join("scan-cache.json")
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
    /// Files served from the incremental cache without a read (0 if caching was off).
    pub cache_hits: usize,
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

        // The incremental cache accelerates the gate path only (shingles always need a read).
        let use_cache = cfg.cache_path.is_some() && !cfg.want_shingles;
        let old = match &cfg.cache_path {
            Some(p) if use_cache => Cache::load(p),
            _ => Cache::default(),
        };

        // (facts, was_cache_hit) per file, in walk order.
        let scored: Vec<(FileFacts, bool)> = rels
            .par_iter()
            .map(|rel| read_facts(root, rel, &subrepos, cfg, use_cache.then_some(&old)))
            .collect();

        let cache_hits = scored.iter().filter(|(_, hit)| *hit).count();
        let facts: Vec<FileFacts> = scored.into_iter().map(|(f, _)| f).collect();

        let over_cap = facts
            .iter()
            .filter(|f| f.hash.is_none() && f.size > cfg.max_file_bytes)
            .count();
        let unreadable = facts
            .iter()
            .filter(|f| f.hash.is_none() && f.size <= cfg.max_file_bytes && f.size > 0)
            .count();

        // Refresh + persist the cache from this pass (both gate and shingle runs produce
        // hashes/line-counts, so a `lean` run also warms the cache for the next `check`).
        if let Some(p) = &cfg.cache_path {
            write_cache(p, root, &facts);
        }

        Scan {
            root: root.to_path_buf(),
            files: facts,
            subrepos,
            over_cap,
            unreadable,
            cache_hits,
        }
    }

    /// A `rel-path → line count` map (for complexity / LOC rollups without a second read).
    pub fn lines_by_rel(&self) -> std::collections::BTreeMap<String, u32> {
        self.files.iter().map(|f| (f.rel_str(), f.lines)).collect()
    }
}

/// Read and characterize one file, consulting `cache` first. Never panics: a stat or read
/// failure yields a zeroed-hash fact (counted as unreadable) rather than aborting the scan.
/// Returns `(facts, cache_hit)`.
fn read_facts(
    root: &Path,
    rel: &Path,
    subrepos: &[PathBuf],
    cfg: &ScanConfig,
    cache: Option<&Cache>,
) -> (FileFacts, bool) {
    let subrepo = walk::subrepo_of(rel, subrepos).to_path_buf();
    let abs = root.join(rel);

    let mut facts = FileFacts {
        rel: rel.to_path_buf(),
        subrepo,
        size: 0,
        is_binary: false,
        hash: None,
        lines: 0,
        shingles: Vec::new(),
    };

    // Skip symlinks: never follow them. Following would (a) let a symlink pointing at a tracked
    // file masquerade as a duplicate of its own target, and (b) risk traversing a cycle. A
    // symlink carries no content of its own, so it is simply excluded (size 0, no hash) — not
    // counted as unreadable.
    if std::fs::symlink_metadata(&abs)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return (facts, false);
    }

    let fp = cache::stat_fingerprint(&abs);
    facts.size = fp.map(|(s, _)| s).unwrap_or(0);
    let size = facts.size;

    // Cache hit: reuse the stored fingerprint without reading the file.
    if let (Some(cache), Some((size, mtime))) = (cache, fp) {
        if let Some(e) = cache.entries.get(&facts.rel_str()) {
            if e.matches(size, mtime) {
                facts.is_binary = e.is_binary;
                facts.lines = e.lines;
                facts.hash = e.hash.as_deref().and_then(cache::hex_to_hash);
                return (facts, true);
            }
        }
    }

    if size == 0 || size > cfg.max_file_bytes {
        return (facts, false); // empty (never a dup) or over cap (not a source candidate)
    }
    let Ok(bytes) = std::fs::read(&abs) else {
        return (facts, false); // unreadable — counted, not fatal
    };
    if bytes.is_empty() {
        return (facts, false);
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
    (facts, false)
}

/// Build a fresh cache from this pass's facts and write it atomically. Only files with a live
/// stat fingerprint are stored (an unreadable file is left out so it's retried next run).
fn write_cache(path: &Path, root: &Path, facts: &[FileFacts]) {
    let mut cache = Cache {
        version: cache::CACHE_VERSION,
        entries: Default::default(),
    };
    for f in facts {
        let Some((size, mtime)) = cache::stat_fingerprint(&root.join(&f.rel)) else {
            continue;
        };
        // Only cache files we actually characterized (read or a prior hit). Skip over-cap/
        // unreadable non-empty files so they are re-attempted, but DO cache empty files
        // (hash=None, cheap) so they don't churn the cache.
        if f.hash.is_none() && f.size > 0 {
            continue;
        }
        cache.entries.insert(
            f.rel_str(),
            CacheEntry {
                size,
                mtime_s: mtime.0,
                mtime_ns: mtime.1,
                hash: f.hash.as_ref().map(cache::hash_to_hex),
                lines: f.lines,
                is_binary: f.is_binary,
            },
        );
    }
    cache.save(path);
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

    #[test]
    fn incremental_cache_reuses_unchanged_files() {
        let base = probe("cache_hit");
        fs::write(base.join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(base.join("b.rs"), "fn b() {}\n").unwrap();
        let cache = base.join(".bonsai").join("scan-cache.json");
        let cfg = ScanConfig::gate().with_cache(cache.clone());

        // cold run: nothing cached yet → 0 hits, cache written
        let cold = Scan::run(&base, &cfg);
        assert_eq!(cold.cache_hits, 0);
        assert!(cache.exists(), "cache file should be written");
        let a_hash = cold
            .files
            .iter()
            .find(|f| f.rel_str() == "a.rs")
            .unwrap()
            .hash;

        // warm run: both files unchanged → both served from cache, identical hashes
        let warm = Scan::run(&base, &cfg);
        assert_eq!(warm.cache_hits, 2, "both files should hit the cache");
        assert_eq!(
            warm.files
                .iter()
                .find(|f| f.rel_str() == "a.rs")
                .unwrap()
                .hash,
            a_hash
        );

        // change one file → it misses, the other still hits
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(base.join("a.rs"), "fn a() { changed(); }\n").unwrap();
        let after = Scan::run(&base, &cfg);
        assert_eq!(after.cache_hits, 1, "only the unchanged file should hit");
        let a_new = after
            .files
            .iter()
            .find(|f| f.rel_str() == "a.rs")
            .unwrap()
            .hash;
        assert_ne!(a_new, a_hash, "changed file must be re-hashed");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_never_masks_same_size_content_change() {
        // adversarial: same byte length, different content, mtime forced to move.
        let base = probe("cache_samesize");
        fs::write(base.join("x.rs"), "AAAA\n").unwrap();
        let cache = base.join(".bonsai").join("scan-cache.json");
        let cfg = ScanConfig::gate().with_cache(cache);
        let h1 = Scan::run(&base, &cfg).files[0].hash;
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(base.join("x.rs"), "BBBB\n").unwrap(); // same length, new mtime
        let h2 = Scan::run(&base, &cfg).files[0].hash;
        assert_ne!(
            h1, h2,
            "same-size content change must not be masked by the cache"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
