//! Incremental scan cache (ADR 0134) — the monorepo re-run speedup.
//!
//! A content scan re-hashes every tracked file. On a large or mono-repo where a fail-closed
//! `bonsai check` runs on every commit, that is almost all wasted work: the overwhelming
//! majority of files are byte-identical to the last run. This cache keys each file by
//! `(size, mtime)` — the same signal every incremental build system and linter uses
//! (eslintcache, mypy, cargo) — and reuses the stored blake3 hash, line count, and
//! text/binary flag when both match, skipping the read entirely.
//!
//! It is a pure accelerator: a miss (new file, changed size/mtime, or a bumped
//! [`CACHE_VERSION`]) simply reads and recomputes, and the result is never *trusted* over the
//! filesystem — a stale or corrupt cache can only cost a re-read, never a wrong answer. The
//! file is written atomically (temp + rename) so a crashed or concurrent run can't corrupt it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Bump when [`CacheEntry`] semantics or the hash algorithm change, invalidating old caches.
pub const CACHE_VERSION: u32 = 1;

/// One cached file fingerprint. `hash`/`lines`/`is_binary` are reused only when the live
/// `(size, mtime)` still match, so they never diverge from disk without a miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_s: i64,
    pub mtime_ns: u32,
    /// blake3 hex; `None` mirrors an unreadable/empty/over-cap file (no hash).
    pub hash: Option<String>,
    pub lines: u32,
    pub is_binary: bool,
}

impl CacheEntry {
    /// Does this cached fingerprint still describe the file with these live stats?
    pub fn matches(&self, size: u64, mtime: (i64, u32)) -> bool {
        self.size == size && (self.mtime_s, self.mtime_ns) == mtime
    }
}

/// The on-disk cache: a version tag plus one entry per repo-relative path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub entries: BTreeMap<String, CacheEntry>,
}

impl Cache {
    /// Load the cache at `path`. A missing, unparseable, or version-mismatched cache yields an
    /// empty one (every file will miss and be recomputed) — never an error, never a wrong hit.
    pub fn load(path: &Path) -> Cache {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Cache::default();
        };
        match serde_json::from_str::<Cache>(&text) {
            Ok(c) if c.version == CACHE_VERSION => c,
            _ => Cache::default(),
        }
    }

    /// Write the cache to `path` atomically (temp file in the same dir + rename), creating the
    /// parent dir if needed. Best-effort: a write failure is swallowed (the cache is optional).
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string(self) else {
            return;
        };
        // unique-ish temp name without Date/rand (unavailable): use the pid + path hash.
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Read a file's `(size, mtime)` fingerprint for cache comparison. `None` if it can't be
/// stat-ed (a miss — the caller will attempt a read and count it unreadable).
pub fn stat_fingerprint(abs: &Path) -> Option<(u64, (i64, u32))> {
    let meta = std::fs::metadata(abs).ok()?;
    let size = meta.len();
    let mtime = meta.modified().ok()?;
    let mtime = match mtime.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        // file mtime before the epoch (clock skew / odd fs): negative seconds
        Err(e) => (
            -(e.duration().as_secs() as i64),
            e.duration().subsec_nanos(),
        ),
    };
    Some((size, mtime))
}

/// blake3 bytes → lowercase hex (for the JSON cache).
pub fn hash_to_hex(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// hex → blake3 bytes (`None` if malformed — treated as a miss).
pub fn hex_to_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrips() {
        let h = [
            0u8, 1, 2, 255, 128, 64, 32, 16, 8, 4, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 42,
        ];
        assert_eq!(hex_to_hash(&hash_to_hex(&h)), Some(h));
        assert_eq!(hex_to_hash("nope"), None);
        assert_eq!(hex_to_hash(&"z".repeat(64)), None);
    }

    #[test]
    fn version_mismatch_yields_empty() {
        let dir = std::env::temp_dir().join("bonsai_cache_ver");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("c.json");
        let bad = format!("{{\"version\":{},\"entries\":{{}}}}", CACHE_VERSION + 99);
        std::fs::write(&p, bad).unwrap();
        assert!(Cache::load(&p).entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("bonsai_cache_rt");
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("sub").join("c.json"); // parent created on save
        let mut c = Cache {
            version: CACHE_VERSION,
            entries: BTreeMap::new(),
        };
        c.entries.insert(
            "a.rs".into(),
            CacheEntry {
                size: 10,
                mtime_s: 5,
                mtime_ns: 7,
                hash: Some("ab".repeat(32)),
                lines: 3,
                is_binary: false,
            },
        );
        c.save(&p);
        let back = Cache::load(&p);
        assert_eq!(back.entries.len(), 1);
        assert!(back.entries["a.rs"].matches(10, (5, 7)));
        assert!(!back.entries["a.rs"].matches(11, (5, 7)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
