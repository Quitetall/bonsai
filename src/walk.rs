//! The single filesystem-walk policy (ADR 0134). Every Bonsai subsystem walks through here
//! so they all see the *same* tracked structure — not build artifacts, vendored trees, or
//! gitignored checkpoints. A structure tool that reads `target/` or a multi-GB `ai_models/`
//! checkpoint is worse than useless; respecting `.gitignore` is the correct default.

use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Skip these regardless of `.gitignore` (VCS/build/cache trees that may be tracked).
pub const DEFAULT_SKIP: &[&str] =
    &["target", ".git", "node_modules", "__pycache__", ".venv", "_dist", "graphify-out"];

/// One walked entry, path relative to the walk root.
pub struct Entry {
    pub rel: PathBuf,
    pub is_dir: bool,
}

/// Walk `root` respecting `.gitignore` + hidden-file rules, additionally pruning any
/// directory named in `extra_skip`. Yields entries (dirs and files) relative to `root`,
/// excluding the root itself. Deterministic ordering (sorted).
pub fn walk(root: &Path, extra_skip: &[String]) -> Vec<Entry> {
    let skip: BTreeSet<String> = DEFAULT_SKIP
        .iter()
        .map(|s| s.to_string())
        .chain(extra_skip.iter().cloned())
        .collect();

    let mut wb = WalkBuilder::new(root);
    // require_git(false): honor `.gitignore` even when the target isn't itself a git repo
    // (a general structure tool should respect ignore rules everywhere, not only in-repo).
    wb.hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .sort_by_file_name(Ord::cmp);
    wb.filter_entry(move |e| e.file_name().to_str().map(|n| !skip.contains(n)).unwrap_or(true));

    let mut out = Vec::new();
    for res in wb.build() {
        let Ok(entry) = res else { continue };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(Entry { rel: rel.to_path_buf(), is_dir });
    }
    out
}

/// Detect **sub-repo boundaries** — directories that are their own git repository
/// (a nested repo has a `.git/` dir; a submodule has a `.git` *file* gitlink). Returned as
/// repo-relative dir paths. This is what lets Bonsai avoid the classic false positive of
/// flagging a per-submodule `LICENSE` (or mirrored spec/test) as duplication: two files in
/// *different* sub-repos are legitimately separate, not cruft.
pub fn subrepo_roots(root: &Path, extra_skip: &[String]) -> Vec<PathBuf> {
    walk(root, extra_skip)
        .into_iter()
        .filter(|e| e.is_dir && root.join(&e.rel).join(".git").exists())
        .map(|e| e.rel)
        .collect()
}

/// The deepest sub-repo containing `rel` (empty path = the top repo). `roots` from
/// [`subrepo_roots`]; the top repo is always the implicit fallback.
pub fn subrepo_of<'a>(rel: &Path, roots: &'a [PathBuf]) -> &'a Path {
    roots
        .iter()
        .filter(|r| rel.starts_with(r))
        .max_by_key(|r| r.components().count())
        .map(|r| r.as_path())
        .unwrap_or_else(|| Path::new(""))
}

/// Read a file to a string only if it is under `max_bytes` (guards against a tracked but
/// huge data/blob file slipping past `.gitignore` and stalling a scan). `None` if too big
/// or unreadable.
pub fn read_text_capped(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subrepo_of_picks_deepest() {
        let roots = vec![
            PathBuf::from("sub"),
            PathBuf::from("sub/nested"),
            PathBuf::from("other"),
        ];
        assert_eq!(subrepo_of(Path::new("sub/nested/a.rs"), &roots), Path::new("sub/nested"));
        assert_eq!(subrepo_of(Path::new("sub/a.rs"), &roots), Path::new("sub"));
        assert_eq!(subrepo_of(Path::new("top.rs"), &roots), Path::new("")); // top repo
        assert_eq!(subrepo_of(Path::new("other/x.rs"), &roots), Path::new("other"));
    }
}

/// Files only, filtered to the given extensions (lowercase, no dot). Empty `exts` = all files.
pub fn files_with_ext(root: &Path, extra_skip: &[String], exts: &[&str]) -> Vec<PathBuf> {
    walk(root, extra_skip)
        .into_iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel)
        .filter(|p| {
            exts.is_empty()
                || p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| exts.contains(&x.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect()
}
