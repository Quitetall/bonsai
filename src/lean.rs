//! The leanness engine (ADR 0134) — the telos made mechanical.
//!
//! Every anti-entropy property is a fold/scan over the loaded [`Tree`]. This module ships
//! the signals computable **without** the SCIP graph (which is deferred): exact-duplicate
//! files, empty structure, and shape. Dead-code (reachability) and misplacement (the
//! coupling fold) need the Stratum-1 code graph and are declared, not faked (ADR 0134 —
//! honest fail-closed, never a false "clean").
//!
//! Posture is **propose-by-default**: [`analyze`] reports; `check` ratchets the composite
//! score so leanness can only improve (sequester-never-delete, monotone toward 0 waste).

use crate::model::Kind;
use crate::tree::Tree;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

/// A set of files with byte-identical content (an exact clone group).
#[derive(Debug, Clone)]
pub struct DupGroup {
    pub files: Vec<String>,
    pub loc: usize,
}

/// A pair of files that are highly similar but not byte-identical (a near-clone).
#[derive(Debug, Clone)]
pub struct NearDup {
    pub a: String,
    pub b: String,
    pub similarity: f64,
}

/// The leanness snapshot. Counts are exact; the score is a relative composite in `[0,1]`.
#[derive(Debug, Clone, Default)]
pub struct LeanReport {
    pub files: usize,
    pub composites: usize,
    /// Composite nodes with zero descendant files (dead structure).
    pub empty_dirs: Vec<String>,
    /// Exact-duplicate content groups (non-empty files only).
    pub dup_groups: Vec<DupGroup>,
    pub max_depth: usize,
    /// Dead internal symbols (unreachable from the public API) — `Some` only when a SCIP
    /// index is present; `None` = unmeasured (never faked as 0).
    pub dead_code: Option<usize>,
    /// Misplaced files (used only from one other directory) — `Some` only when a SCIP index
    /// is present; `None` = unmeasured.
    pub misplaced: Option<usize>,
    /// Signals that still need the SCIP graph — reported as unmeasured, never as zero.
    pub deferred: Vec<&'static str>,
}

impl LeanReport {
    /// Redundant files: every clone beyond the first in each group.
    pub fn dup_files(&self) -> usize {
        self.dup_groups
            .iter()
            .map(|g| g.files.len().saturating_sub(1))
            .sum()
    }

    /// LOC that duplication wastes (redundant copies × their size).
    pub fn wasted_loc(&self) -> usize {
        self.dup_groups
            .iter()
            .map(|g| g.files.len().saturating_sub(1) * g.loc)
            .sum()
    }

    /// A relative leanness score in `[0,1]` (1 = no measured waste). Weighted blend of the
    /// duplication ratio and the empty-structure ratio — the two axes we can measure today.
    pub fn score(&self) -> f64 {
        let dup_ratio = ratio(self.dup_files(), self.files);
        let empty_ratio = ratio(self.empty_dirs.len(), self.composites);
        (1.0 - (0.7 * dup_ratio + 0.3 * empty_ratio)).clamp(0.0, 1.0)
    }
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

/// Compute the leanness report over the loaded tree, reading anchored file contents.
pub fn analyze(tree: &Tree, root: &Path) -> LeanReport {
    let mut report = LeanReport {
        deferred: vec![
            "dead-code (reachability — needs SCIP)",
            "misplacement (coupling fold — needs the dependency graph)",
        ],
        ..Default::default()
    };

    // file counts per subtree (catamorphism) → empty-composite detection
    let counts = tree.fold(&|node, kids: &[usize]| match node.kind {
        Kind::Atom => 1usize,
        Kind::Composite => kids.iter().sum(),
    });
    for node in tree.nodes.values() {
        match node.kind {
            Kind::Atom => report.files += 1,
            Kind::Composite => {
                report.composites += 1;
                if node.id != tree.root && counts.get(&node.id).copied().unwrap_or(0) == 0 {
                    report.empty_dirs.push(node.id.clone());
                }
            }
        }
    }
    report.empty_dirs.sort();
    report.max_depth = depth_of(tree);

    // exact-duplicate content groups (skip empty files — trivially identical, pure noise)
    let mut by_hash: BTreeMap<u64, Vec<(String, usize)>> = BTreeMap::new();
    for node in tree.nodes.values() {
        if node.kind != Kind::Atom {
            continue;
        }
        let Some(anchor) = node.anchors.first() else {
            continue;
        };
        let p = root.join(anchor);
        if std::fs::metadata(&p).map(|m| m.len() > 2_000_000).unwrap_or(true) {
            continue; // skip huge/unreadable (blobs aren't source dup candidates)
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        let loc = bytes.iter().filter(|b| **b == b'\n').count() + 1;
        by_hash
            .entry(h.finish())
            .or_default()
            .push((node.id.clone(), loc));
    }
    for (_, mut group) in by_hash {
        if group.len() > 1 {
            group.sort();
            let loc = group[0].1;
            report.dup_groups.push(DupGroup {
                files: group.into_iter().map(|(id, _)| id).collect(),
                loc,
            });
        }
    }
    report
        .dup_groups
        .sort_by_key(|g| std::cmp::Reverse(g.files.len()));
    report
}

/// The deepest containment path length (structure shape).
fn depth_of(tree: &Tree) -> usize {
    let depths = tree.fold(&|node, kids: &[usize]| {
        1 + kids.iter().copied().max().unwrap_or(0) * (node.kind == Kind::Composite) as usize
    });
    depths.get(&tree.root).copied().unwrap_or(0)
}

/// The committed ratchet baseline (`bonsai.lean.json`). `check` fails if the current score
/// drops below `min_score` or duplication rises above `max_dup_files` — leanness may only
/// improve (monotone). `--save` refreshes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeanBaseline {
    pub min_score: f64,
    pub max_dup_files: usize,
    pub max_empty_dirs: usize,
    /// Ratcheted dead-symbol ceiling (only present once measured via SCIP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_dead: Option<usize>,
    /// Ratcheted misplaced-file ceiling (only present once measured via SCIP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_misplaced: Option<usize>,
}

impl LeanBaseline {
    pub fn from_report(r: &LeanReport) -> Self {
        LeanBaseline {
            min_score: r.score(),
            max_dup_files: r.dup_files(),
            max_empty_dirs: r.empty_dirs.len(),
            max_dead: r.dead_code,
            max_misplaced: r.misplaced,
        }
    }

    /// Ratchet check: returns the list of regressions (empty = held or improved).
    pub fn regressions(&self, r: &LeanReport) -> Vec<String> {
        let mut v = Vec::new();
        // small epsilon so float noise doesn't trip the gate
        if r.score() + 1e-9 < self.min_score {
            v.push(format!(
                "leanness score {:.4} < baseline {:.4}",
                r.score(),
                self.min_score
            ));
        }
        if r.dup_files() > self.max_dup_files {
            v.push(format!(
                "duplicate files {} > baseline {}",
                r.dup_files(),
                self.max_dup_files
            ));
        }
        if r.empty_dirs.len() > self.max_empty_dirs {
            v.push(format!(
                "empty dirs {} > baseline {}",
                r.empty_dirs.len(),
                self.max_empty_dirs
            ));
        }
        // dead-code / misplacement ratchets only when both baseline and current are measured
        if let (Some(cur), Some(max)) = (r.dead_code, self.max_dead) {
            if cur > max {
                v.push(format!("dead symbols {cur} > baseline {max}"));
            }
        }
        if let (Some(cur), Some(max)) = (r.misplaced, self.max_misplaced) {
            if cur > max {
                v.push(format!("misplaced files {cur} > baseline {max}"));
            }
        }
        v
    }
}

/// Count dead internal symbols via the SCIP index at `<root>/index.scip`, if present.
/// `None` = no index → unmeasured (never faked as 0). Turns the "deferred" dead-code
/// signal into a real, ratchetable measurement when the graph is available.
pub fn dead_code_count(root: &Path) -> Option<usize> {
    let g = load_graph(root)?;
    Some(g.dead_symbols(root, &[]).len())
}

/// Count misplaced files (used only from one other directory) via the SCIP index, if present.
pub fn misplacement_count(root: &Path) -> Option<usize> {
    Some(load_graph(root)?.misplacements().len())
}

fn load_graph(root: &Path) -> Option<crate::scip::CodeGraph> {
    let scip = root.join("index.scip");
    if !scip.exists() {
        return None;
    }
    crate::scip::CodeGraph::from_file(&scip).ok()
}

/// Near-duplicate file pairs (similar but not byte-identical) via normalized-line shingling
/// with a shared-line inverted index (so it stays near-linear instead of O(n²)). A pair is
/// a near-dup when its line-set Jaccard is ≥ `threshold` and below 1.0 (exact dups are
/// reported separately). Boilerplate lines shared by many files are dropped as stop-shingles.
pub fn near_duplicates(tree: &Tree, root: &Path, threshold: f64) -> Vec<NearDup> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    const MIN_LINES: usize = 8; // ignore tiny files (too little signal)
    const MAX_DF: usize = 40; // a line in >40 files is boilerplate → skip
    const MIN_SHARED: usize = 5; // candidate pairs must share ≥5 non-trivial lines

    // per-file normalized line-hash set
    let mut paths: Vec<String> = Vec::new();
    let mut sets: Vec<BTreeSet<u64>> = Vec::new();
    for node in tree.nodes.values() {
        if node.kind != Kind::Atom {
            continue;
        }
        let Some(anchor) = node.anchors.first() else { continue };
        let Some(text) = crate::walk::read_text_capped(&root.join(anchor), 2_000_000) else {
            continue;
        };
        let mut set = BTreeSet::new();
        for line in text.lines() {
            let t = line.trim();
            if t.len() < 4 || t == "}" || t == "{" || t == ")" {
                continue; // structural noise
            }
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            set.insert(h.finish());
        }
        if set.len() >= MIN_LINES {
            paths.push(anchor.clone());
            sets.push(set);
        }
    }

    // inverted index: line-hash → files containing it (dropping stop-shingles)
    let mut inv: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (i, set) in sets.iter().enumerate() {
        for &h in set {
            inv.entry(h).or_default().push(i);
        }
    }
    // candidate pair → shared-line count
    let mut shared: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for files in inv.values() {
        if files.len() > MAX_DF {
            continue;
        }
        for a in 0..files.len() {
            for b in (a + 1)..files.len() {
                *shared.entry((files[a], files[b])).or_default() += 1;
            }
        }
    }

    let mut out = Vec::new();
    for ((i, j), n) in shared {
        if n < MIN_SHARED {
            continue;
        }
        let (si, sj) = (&sets[i], &sets[j]);
        let inter = si.intersection(sj).count();
        let union = si.len() + sj.len() - inter;
        let jac = inter as f64 / union.max(1) as f64;
        if jac >= threshold && jac < 1.0 {
            out.push(NearDup { a: paths[i].clone(), b: paths[j].clone(), similarity: jac });
        }
    }
    out.sort_by(|x, y| y.similarity.partial_cmp(&x.similarity).unwrap());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    fn probe_dir(tag: &str) -> std::path::PathBuf {
        // per-test scratch dir (tests run in parallel — a shared path races)
        let base = std::env::temp_dir().join(format!("bonsai_lean_probe_{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("a")).unwrap();
        fs::create_dir_all(base.join("b")).unwrap();
        fs::create_dir_all(base.join("empty")).unwrap();
        fs::write(base.join("a/x.rs"), "fn main() {}\nlet z = 1;\n").unwrap();
        fs::write(base.join("b/y.rs"), "fn main() {}\nlet z = 1;\n").unwrap(); // exact dup
        fs::write(base.join("a/uniq.rs"), "fn other() {}\n").unwrap();
        base
    }

    #[test]
    fn detects_exact_dup_and_empty_dir() {
        let base = probe_dir("dup");
        let cfg = Config::from_dir(&base).unwrap();
        let tree = cfg.into_tree(Some(&base)).unwrap();
        let r = analyze(&tree, &base);

        // x.rs and y.rs are byte-identical → one dup group of size 2 → 1 redundant file
        assert_eq!(r.dup_files(), 1, "groups: {:?}", r.dup_groups);
        // the `empty/` dir has no files
        assert!(
            r.empty_dirs.iter().any(|d| d == "empty"),
            "empty: {:?}",
            r.empty_dirs
        );
        // score penalized but valid
        assert!(r.score() < 1.0 && r.score() > 0.0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn near_dup_detects_similar_not_identical() {
        let base = probe_dir("neardup");
        let body: String = (0..20).map(|i| format!("let v{i} = compute({i});\n")).collect();
        // two files differing only in the last line → high Jaccard, not identical
        fs::write(base.join("a/x.rs"), format!("{body}return v0;\n")).unwrap();
        fs::write(base.join("b/y.rs"), format!("{body}return v19;\n")).unwrap();
        let cfg = Config::from_dir(&base).unwrap();
        let tree = cfg.into_tree(Some(&base)).unwrap();
        let nd = near_duplicates(&tree, &base, 0.8);
        assert_eq!(nd.len(), 1, "{nd:?}");
        assert!(nd[0].similarity > 0.8 && nd[0].similarity < 1.0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ratchet_flags_regression() {
        let base = probe_dir("ratchet");
        let cfg = Config::from_dir(&base).unwrap();
        let tree = cfg.into_tree(Some(&base)).unwrap();
        let r = analyze(&tree, &base);
        let baseline = LeanBaseline::from_report(&r);
        assert!(baseline.regressions(&r).is_empty(), "same state must hold");

        // a stricter baseline (0 dups) must flag the existing dup as a regression
        let strict = LeanBaseline {
            min_score: 1.0,
            max_dup_files: 0,
            max_empty_dirs: 0,
            max_dead: None,
            max_misplaced: None,
        };
        assert!(!strict.regressions(&r).is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
