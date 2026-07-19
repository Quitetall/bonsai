//! `bonsai.toml` — the authored structure model (Stratum 2, ADR 0134) and the loader that
//! turns it (plus the live filesystem) into a [`Tree`].
//!
//! `init` auto-derives a directory-level capability tree (the *automatic configuration*);
//! hand-edits then place anything anywhere (the *manual configuration* — a node's `parent`
//! is its authored **home**, which may differ from where the file physically sits; that
//! divergence is exactly the misplacement signal the leanness folds read later).

use crate::model::{Kind, Node, NodeId, Plane};
use crate::tree::Tree;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The whole `bonsai.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub bonsai: Meta,
    /// The docs plane (optional) — the recursive compose executor (ADR 0134).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<DocsCfg>,
    /// Authored nodes. Serialized as `[[node]]` tables.
    #[serde(default, rename = "node")]
    pub nodes: Vec<NodeSpec>,
}

/// Docs-plane configuration. The docs are first-class nodes in the same tree; this points
/// Bonsai at the recursive compose executor that folds atoms → parents (the proven
/// no-regression generalization of the legacy `doc_compose`, ADR 0134).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DocsCfg {
    /// Shell command (run at repo root) that composes/checks the docs tree.
    /// Defaults to `python3 bonsai/docs/compose.py`.
    #[serde(default = "default_compose")]
    pub compose: String,
}

impl Default for DocsCfg {
    fn default() -> Self {
        DocsCfg {
            compose: default_compose(),
        }
    }
}

fn default_compose() -> String {
    "python3 bonsai/docs/compose.py".to_string()
}

/// Top-level `[bonsai]` settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Meta {
    /// The root node id (default `"."`).
    #[serde(default = "default_root")]
    pub root: String,
    #[serde(default = "default_version")]
    pub version: u32,
    /// Directory names never descended into (VCS/build/cache dirs).
    #[serde(default = "default_skip")]
    pub skip: Vec<String>,
    /// Cap on `init` directory depth (`None` = unbounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_init_depth: Option<usize>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            root: default_root(),
            version: default_version(),
            skip: default_skip(),
            max_init_depth: None,
        }
    }
}

/// One authored node (a `[[node]]` table). Everything but `id` is optional so a
/// hand-written entry stays terse; missing fields are inferred.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeSpec {
    pub id: String,
    /// `"atom"` | `"composite"` — inferred from children if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `"code"` | `"docs"` — inferred from the path if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<String>,
    /// Containment home; defaults to the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facets: BTreeMap<String, String>,
}

fn default_root() -> String {
    ".".to_string()
}
fn default_version() -> u32 {
    1
}
fn default_skip() -> Vec<String> {
    crate::walk::DEFAULT_SKIP
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl NodeSpec {
    fn plane_or(&self, default: Plane) -> Plane {
        match self.plane.as_deref() {
            Some("docs") => Plane::Docs,
            Some("code") => Plane::Code,
            _ => default,
        }
    }
    fn kind_or(&self, default: Kind) -> Kind {
        match self.kind.as_deref() {
            Some("atom") => Kind::Atom,
            Some("composite") => Kind::Composite,
            _ => default,
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Auto-derive the directory-level capability tree from the filesystem: one composite
    /// node per directory, parented to its containing directory. Files are NOT written as
    /// nodes — they are overlaid live at render time (keeps `bonsai.toml` about *structure*,
    /// not an inventory that rots). Facets (`plane`, `language`) are guessed.
    pub fn from_dir(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_dir_with_skip(root, &[])
    }

    /// Like [`from_dir`](Self::from_dir) but adds `extra_skip` directory names to the default
    /// skip set (e.g. vendored/sequestered/scratch trees when adopting on a real repo).
    pub fn from_dir_with_skip(
        root: impl AsRef<Path>,
        extra_skip: &[String],
    ) -> anyhow::Result<Self> {
        let mut meta = Meta::default();
        for s in extra_skip {
            if !meta.skip.contains(s) {
                meta.skip.push(s.clone());
            }
        }
        let root = root.as_ref();
        let mut nodes = Vec::new();
        for entry in crate::walk::walk(root, extra_skip) {
            if !entry.is_dir {
                continue;
            }
            let rel = &entry.rel;
            if let Some(depth) = meta.max_init_depth {
                if rel.components().count() > depth {
                    continue;
                }
            }
            let id = rel_id(rel);
            let parent = rel
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(rel_id)
                .unwrap_or_else(|| meta.root.clone());
            let mut facets = BTreeMap::new();
            facets.insert("plane".into(), plane_of(rel).into());
            if let Some(lang) = dominant_language(&root.join(rel)) {
                facets.insert("language".into(), lang.into());
            }
            nodes.push(NodeSpec {
                id,
                kind: Some("composite".into()),
                plane: Some(plane_of(rel).into()),
                parent: Some(parent),
                anchors: vec![rel_id(rel)],
                depends_on: Vec::new(),
                facets,
            });
        }
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Config {
            bonsai: meta,
            docs: None,
            nodes,
        })
    }

    /// Build the [`Tree`]. With `overlay`, live files under `root_dir` are added as atom
    /// nodes beneath their nearest authored directory node (so `tree`/folds see real files
    /// and the one-home invariant still holds).
    pub fn into_tree(&self, overlay: Option<&Path>) -> anyhow::Result<Tree> {
        let root_id = if self.bonsai.root.is_empty() {
            ".".to_string()
        } else {
            self.bonsai.root.clone()
        };
        let mut nodes: BTreeMap<NodeId, Node> = BTreeMap::new();
        nodes.insert(
            root_id.clone(),
            Node::new(root_id.clone(), Kind::Composite, Plane::Code),
        );

        for spec in &self.nodes {
            let mut n = Node::new(
                spec.id.clone(),
                spec.kind_or(Kind::Composite),
                spec.plane_or(Plane::Code),
            );
            n.parent = Some(spec.parent.clone().unwrap_or_else(|| root_id.clone()));
            n.depends_on = spec.depends_on.clone();
            n.facets = spec.facets.clone();
            n.anchors = spec.anchors.clone();
            nodes.insert(spec.id.clone(), n);
        }

        if let Some(dir) = overlay {
            overlay_files(&mut nodes, &root_id, dir, &self.bonsai.skip);
        }

        wire_children(&mut nodes);
        let mut tree = Tree::new(root_id);
        tree.nodes = nodes;
        Ok(tree)
    }
}

/// Overlay live files as atom nodes beneath their nearest existing directory node.
fn overlay_files(nodes: &mut BTreeMap<NodeId, Node>, root_id: &str, dir: &Path, skip: &[String]) {
    for entry in crate::walk::walk(dir, skip) {
        if entry.is_dir {
            continue;
        }
        let rel = &entry.rel;
        let id = rel_id(rel);
        if nodes.contains_key(&id) {
            continue; // an explicit override already placed this file
        }
        let home = rel
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| nearest_node(nodes, &rel_id(p)))
            .unwrap_or_else(|| root_id.to_string());
        let mut n = Node::new(id.clone(), Kind::Atom, plane_enum(rel));
        n.parent = Some(home);
        n.anchors = vec![rel_id(rel)];
        if let Some(lang) = language_of_ext(rel) {
            n.facets.insert("language".into(), lang.into());
        }
        nodes.insert(id, n);
    }
}

/// Walk a dotted/slashed id up its ancestors until one resolves to an existing node
/// (falls back to the root — guaranteed present).
fn nearest_node(nodes: &BTreeMap<NodeId, Node>, id: &str) -> NodeId {
    let mut cur = id.to_string();
    loop {
        if nodes.contains_key(&cur) {
            return cur;
        }
        match cur.rsplit_once('/') {
            Some((head, _)) => cur = head.to_string(),
            None => return ".".to_string(),
        }
    }
}

/// Push every node into its parent's `children` (dedup), so one-home holds both ways.
fn wire_children(nodes: &mut BTreeMap<NodeId, Node>) {
    let links: Vec<(NodeId, NodeId)> = nodes
        .values()
        .filter_map(|n| n.parent.clone().map(|p| (p, n.id.clone())))
        .collect();
    for (parent, child) in links {
        if let Some(p) = nodes.get_mut(&parent) {
            if !p.children.contains(&child) {
                p.children.push(child);
            }
        }
    }
    for n in nodes.values_mut() {
        n.children.sort();
    }
}

/// A repo-relative path → node id (forward-slashed, extension kept — paths are natural ids).
fn rel_id(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn plane_of(rel: &Path) -> &'static str {
    if rel.components().any(|c| c.as_os_str() == "docs") {
        "docs"
    } else {
        "code"
    }
}

fn plane_enum(rel: &Path) -> Plane {
    match plane_of(rel) {
        "docs" => Plane::Docs,
        _ => Plane::Code,
    }
}

fn language_of_ext(rel: &Path) -> Option<&'static str> {
    match rel.extension().and_then(|e| e.to_str())? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "md" => Some("markdown"),
        "toml" => Some("toml"),
        "c" | "h" => Some("c"),
        "cpp" | "hpp" | "cc" => Some("cpp"),
        _ => None,
    }
}

/// The dominant source language among a directory's *immediate* files (for the dir facet).
fn dominant_language(dir: &Path) -> Option<&'static str> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()) {
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Some(lang) = language_of_ext(&e.path()) {
                *counts.entry(lang).or_default() += 1;
            }
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(l, _)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tree_invariants_hold() {
        let toml = r#"
[bonsai]
root = "."
version = 1

[[node]]
id = "codec"
kind = "composite"
parent = "."
facets = { layer = "codec", language = "rust" }

[[node]]
id = "codec/lml"
kind = "atom"
parent = "codec"
depends_on = ["codec"]
"#;
        let cfg = Config::from_toml(toml).unwrap();
        assert_eq!(cfg.nodes.len(), 2);
        let tree = cfg.into_tree(None).unwrap();
        assert!(tree.check().is_empty(), "invariants: {:?}", tree.check());
        // parent/child wired both ways
        assert!(tree.nodes["codec"]
            .children
            .contains(&"codec/lml".to_string()));
        // re-serialize parses back
        let text = cfg.to_toml().unwrap();
        assert!(Config::from_toml(&text).is_ok());
    }

    #[test]
    fn default_parent_is_root() {
        let cfg = Config::from_toml("[[node]]\nid = \"a\"\n").unwrap();
        let tree = cfg.into_tree(None).unwrap();
        assert_eq!(tree.nodes["a"].parent.as_deref(), Some("."));
        assert!(tree.check().is_empty());
    }
}
