//! The placement oracle (ADR 0134) — *where does this code belong?*
//!
//! The misplacement fold already knows the ideal home of existing code (a file used only from
//! one directory belongs there). This inverts it into a question you can ask *while adding*: for
//! a file — one already in the graph, or one you just wrote — rank the directories it is coupled
//! to and name the home. It's the "Bonsai knows where it belongs" half of governed growth; the
//! conformance gate (`rules.rs`) is the enforcement half.

use crate::scip::CodeGraph;
use std::collections::BTreeMap;

/// A ranked placement recommendation for one file.
pub struct Placement {
    pub file: String,
    pub current_dir: String,
    /// Directories this file couples to, most-coupled first: `(dir, edge_count)`.
    pub ranked: Vec<(String, usize)>,
    /// True if the recommendation came from the SCIP coupling graph; false = import-scan of a
    /// not-yet-indexed new file (a weaker heuristic, honestly flagged).
    pub from_graph: bool,
}

impl Placement {
    /// The recommended home directory (the top-coupled dir), or `None` if there is no signal.
    pub fn recommended(&self) -> Option<&str> {
        self.ranked.first().map(|(d, _)| d.as_str())
    }
    /// True if the file already sits in its recommended home (or there is no signal to move it).
    pub fn well_placed(&self) -> bool {
        self.recommended()
            .map(|r| r == self.current_dir)
            .unwrap_or(true)
    }
}

/// The directory portion of a repo-relative path (`""` = repo root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// Placement from the SCIP graph: rank the directories `file` shares dependency edges with
/// (both the files it uses and the files that use it). `None` if the file has no edges (an
/// island — nothing to couple to). This is the strong, exact signal.
pub fn place_existing(graph: &CodeGraph, file: &str) -> Option<Placement> {
    let mut coupling: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(tos) = graph.file_deps.get(file) {
        for to in tos {
            *coupling.entry(dir_of(to).to_string()).or_default() += 1; // out-edge
        }
    }
    for (from, tos) in &graph.file_deps {
        if tos.contains(file) {
            *coupling.entry(dir_of(from).to_string()).or_default() += 1; // in-edge
        }
    }
    if coupling.is_empty() {
        return None;
    }
    let mut ranked: Vec<(String, usize)> = coupling.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some(Placement {
        file: file.to_string(),
        current_dir: dir_of(file).to_string(),
        ranked,
        from_graph: true,
    })
}

/// Placement for a NEW, not-yet-indexed file, from its `crate::` imports: rank the directories
/// that own the modules it pulls in. `module_dirs` maps a top-level module name to the directory
/// that defines it (derive it from the workspace with [`module_dir_map`]). Weaker than the graph
/// signal, but answers "I just wrote this — where does it go?" without a reindex.
pub fn place_new(
    content: &str,
    current_dir: &str,
    module_dirs: &BTreeMap<String, String>,
) -> Option<Placement> {
    let mut coupling: BTreeMap<String, usize> = BTreeMap::new();
    for module in crate_imports(content) {
        if let Some(dir) = module_dirs.get(&module) {
            *coupling.entry(dir.clone()).or_default() += 1;
        }
    }
    if coupling.is_empty() {
        return None;
    }
    let mut ranked: Vec<(String, usize)> = coupling.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some(Placement {
        file: String::new(),
        current_dir: current_dir.to_string(),
        ranked,
        from_graph: false,
    })
}

/// Extract the first path segment of every `crate::<seg>::…` reference in `content` (the module
/// the reference roots at). Deduplication is intentional-free: repeated use of a module counts.
pub fn crate_imports(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let mut rest = line;
        while let Some(pos) = rest.find("crate::") {
            rest = &rest[pos + "crate::".len()..];
            let seg: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !seg.is_empty() {
                out.push(seg);
            }
        }
    }
    out
}

/// Build a `top-level module name → directory` map from a workspace's `.rs` files, so
/// [`place_new`] can resolve `crate::<module>` to the directory that owns it. `src/a/b.rs`
/// contributes `a -> src/a` and `b -> src/a`; `src/a.rs` contributes `a -> src`.
pub fn module_dir_map(rs_files: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for f in rs_files {
        let dir = dir_of(f).to_string();
        // the module name is the file stem, minus the mod/lib/main sentinels
        if let Some(stem) = f.rsplit('/').next().and_then(|b| b.strip_suffix(".rs")) {
            if !matches!(stem, "lib" | "main" | "mod") {
                out.entry(stem.to_string()).or_insert_with(|| dir.clone());
            }
        }
        // each path segment under src also names a directory-module
        for seg in dir.split('/') {
            if !seg.is_empty() && seg != "src" {
                out.entry(seg.to_string()).or_insert_with(|| dir.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_existing_ranks_coupled_dir() {
        // util/helper.rs is used by two feature/*.rs and uses one core/*.rs.
        let mut g = CodeGraph::default();
        g.file_deps
            .entry("feature/a.rs".into())
            .or_default()
            .insert("util/helper.rs".into());
        g.file_deps
            .entry("feature/b.rs".into())
            .or_default()
            .insert("util/helper.rs".into());
        g.file_deps
            .entry("util/helper.rs".into())
            .or_default()
            .insert("core/x.rs".into());

        let p = place_existing(&g, "util/helper.rs").unwrap();
        assert_eq!(p.current_dir, "util");
        // two in-edges from feature/ beat one out-edge to core/
        assert_eq!(p.recommended(), Some("feature"));
        assert!(!p.well_placed(), "helper belongs in feature/, not util/");
        assert!(p.from_graph);
    }

    #[test]
    fn place_new_from_imports() {
        let content =
            "use crate::scan::Scan;\nuse crate::scan::ScanConfig;\nuse crate::model::Node;\n";
        let files = vec![
            "src/scan.rs".to_string(),
            "src/model.rs".to_string(),
            "src/lib.rs".to_string(),
        ];
        let map = module_dir_map(&files);
        let p = place_new(content, "src", &map).unwrap();
        // two crate::scan hits vs one crate::model → scan's dir wins
        assert_eq!(p.recommended(), Some("src"));
        assert!(!p.from_graph);
        assert_eq!(crate_imports(content).len(), 3);
    }

    #[test]
    fn island_file_has_no_recommendation() {
        let g = CodeGraph::default();
        assert!(place_existing(&g, "lonely.rs").is_none());
    }
}
