//! Adversarial tests (ADR 0134) — plant cases designed to *fool* Bonsai's detectors and
//! assert it does the right thing. The point is falsification: a leanness tool that cries
//! "cruft" on legitimate structure (a per-submodule LICENSE, a spec mirrored into a sub-repo)
//! is worse than none. Each test is a trap; Bonsai must not step in it.

use bonsai::config::Config;
use bonsai::lean;
use std::fs;
use std::path::{Path, PathBuf};

/// A throwaway tree under the OS temp dir (unique per test tag). Removed on drop.
struct Tree(PathBuf);
impl Tree {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("bonsai_adv_{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Tree(base)
    }
    fn file(&self, rel: &str, content: &str) -> &Self {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
        self
    }
    /// Mark `rel` as its own git repo (a submodule-style `.git` gitlink file).
    fn subrepo(&self, rel: &str) -> &Self {
        let d = self.0.join(rel);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(".git"), "gitdir: /nonexistent\n").unwrap();
        self
    }
    fn analyze(&self) -> (lean::LeanReport, Vec<lean::NearDup>) {
        let cfg = Config::from_dir(&self.0).unwrap();
        let tree = cfg.into_tree(Some(&self.0)).unwrap();
        let r = lean::analyze(&tree, &self.0);
        let nd = lean::near_duplicates(&tree, &self.0, 0.8);
        (r, nd)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const LICENSE: &str = "AGPL-3.0-or-later\n\nCopyright the authors.\nAll rights reserved here.\n";

/// TRAP 1 — a LICENSE copied into two separate sub-repos must NOT read as duplication.
#[test]
fn cross_subrepo_license_is_not_duplication() {
    let t = Tree::new("license");
    t.subrepo("modA").subrepo("modB");
    t.file("modA/LICENSE", LICENSE).file("modB/LICENSE", LICENSE);
    let (r, _) = t.analyze();
    assert_eq!(r.dup_files(), 0, "cross-repo LICENSE wrongly flagged: {:?}", r.dup_groups);
    assert!(r.cross_repo_mirrors >= 1, "should record the mirror informationally");
}

/// TRAP 2 — the SAME file duplicated *within one repo* is real cruft and MUST be flagged.
#[test]
fn within_repo_duplicate_is_flagged() {
    let t = Tree::new("withindup");
    // both under the top repo (no sub-repo boundary between them)
    t.file("a/dup.py", "def f():\n    return 42\n\nx = f()\n");
    t.file("b/dup.py", "def f():\n    return 42\n\nx = f()\n");
    let (r, _) = t.analyze();
    assert_eq!(r.dup_files(), 1, "within-repo dup missed: {:?}", r.dup_groups);
    assert_eq!(r.cross_repo_mirrors, 0);
}

/// TRAP 3 — a near-identical file across sub-repo boundaries (a mirrored spec) is legitimate.
#[test]
fn cross_subrepo_near_dup_is_ignored() {
    let t = Tree::new("neardupx");
    t.subrepo("repoA").subrepo("repoB");
    let body: String = (0..20).map(|i| format!("row {i}: value = {i}\n")).collect();
    t.file("repoA/spec.md", &format!("{body}last line A\n"));
    t.file("repoB/spec.md", &format!("{body}last line B\n"));
    let (_, nd) = t.analyze();
    assert!(nd.is_empty(), "cross-repo near-dup wrongly flagged: {nd:?}");
}

/// TRAP 4 — the SAME near-dup *within one repo* must still be caught (no over-suppression).
#[test]
fn within_repo_near_dup_is_flagged() {
    let t = Tree::new("neardupin");
    let body: String = (0..20).map(|i| format!("row {i}: value = {i}\n")).collect();
    t.file("x/a.md", &format!("{body}last line A\n"));
    t.file("x/b.md", &format!("{body}last line B\n"));
    let (_, nd) = t.analyze();
    assert_eq!(nd.len(), 1, "within-repo near-dup missed: {nd:?}");
    assert!(nd[0].similarity > 0.8 && nd[0].similarity < 1.0);
}

/// TRAP 5 — deeply nested sub-repos: attribution must pick the *innermost* boundary, so a
/// file copied between a repo and its own nested sub-repo is still cross-boundary (not cruft).
#[test]
fn nested_subrepo_boundary_is_innermost() {
    let t = Tree::new("nested");
    t.subrepo("outer").subrepo("outer/inner");
    t.file("outer/shared.txt", "alpha beta gamma delta epsilon\n")
        .file("outer/inner/shared.txt", "alpha beta gamma delta epsilon\n");
    let (r, _) = t.analyze();
    assert_eq!(r.dup_files(), 0, "nested-repo copy wrongly flagged: {:?}", r.dup_groups);
    assert!(r.cross_repo_mirrors >= 1);
}

/// TRAP 6 — empty files must never form a duplicate group (they are trivially identical).
#[test]
fn empty_files_are_not_duplicates() {
    let t = Tree::new("empties");
    t.file("a/__init__.py", "").file("b/__init__.py", "").file("c/__init__.py", "");
    let (r, _) = t.analyze();
    assert_eq!(r.dup_files(), 0, "empty files wrongly grouped: {:?}", r.dup_groups);
}

/// TRAP 7 — a `.gitignore`d artifact must be invisible to the scan (no phantom cruft, no
/// reading a build blob). Bonsai walks tracked structure, not artifacts.
#[test]
fn gitignored_artifact_is_invisible() {
    let t = Tree::new("ignored");
    // top-level .gitignore hides build/
    t.file(".gitignore", "build/\n");
    t.file("src/real.rs", "pub fn a() {}\n");
    t.file("build/real.rs", "pub fn a() {}\n"); // identical, but ignored
    let cfg = Config::from_dir(&t.0).unwrap();
    let tree = cfg.into_tree(Some(&t.0)).unwrap();
    // the ignored copy must not even be a node
    assert!(
        !tree.nodes.contains_key("build/real.rs"),
        "gitignored file leaked into the model"
    );
    let r = lean::analyze(&tree, &t.0);
    assert_eq!(r.dup_files(), 0, "ignored artifact created a phantom dup");
}

/// Sanity: the top-repo files (no sub-repo) all share the "" scope, so a genuine top-level
/// dup is still counted even when sub-repos exist elsewhere in the tree.
#[test]
fn top_repo_dup_counts_alongside_subrepos() {
    let t = Tree::new("mixed");
    t.subrepo("vendor");
    t.file("vendor/LICENSE", LICENSE); // lone copy in a sub-repo — not a dup
    t.file("tools/x.sh", "#!/bin/sh\necho hello world\n");
    t.file("scripts/x.sh", "#!/bin/sh\necho hello world\n"); // top-repo dup
    let (r, _) = t.analyze();
    assert_eq!(r.dup_files(), 1, "top-repo dup miscounted: {:?}", r.dup_groups);
}

/// Sub-repo detection itself: a `.git` file marks a boundary; plain dirs do not.
#[test]
fn subrepo_roots_detects_git_boundaries() {
    let t = Tree::new("roots");
    t.subrepo("a").file("a/f.rs", "x").file("plain/f.rs", "y");
    let roots = bonsai::walk::subrepo_roots(&t.0, &[]);
    assert!(roots.iter().any(|p| p == Path::new("a")), "missed sub-repo: {roots:?}");
    assert!(!roots.iter().any(|p| p == Path::new("plain")), "plain dir flagged as sub-repo");
}
