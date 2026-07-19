//! The conformance / admission gate (ADR 0134) — how Bonsai *dictates the way code is added*.
//!
//! The leanness ratchet says the repo may only get leaner. These rules say *how new code must
//! enter*: in its correct home (placement), coupled only downward (layering), and without
//! bloating any one directory (bounded growth). Each is a [`Finding`] at **error** severity, so
//! `bonsai check` fails closed — a non-conformant addition can't land. Every rule is opt-in via
//! the `[rules]` block in `bonsai.toml`; a repo that declares none keeps the pure ratchet.

use crate::config::Rules;
use crate::finding::{Finding, Location, Severity};
use crate::model::Kind;
use crate::scip::CodeGraph;
use crate::tree::Tree;
use std::collections::BTreeMap;

/// Evaluate every declared rule over the tree (structure) + optional SCIP graph (coupling),
/// returning error-severity findings — empty means the addition is conformant.
pub fn evaluate(tree: &Tree, graph: Option<&CodeGraph>, rules: &Rules) -> Vec<Finding> {
    let mut out = Vec::new();
    bounded_growth(tree, rules, &mut out);
    abstraction_bounds(tree, rules, &mut out);
    if let Some(g) = graph {
        layering(tree, g, rules, &mut out);
        placement(g, rules, &mut out);
    }
    out
}

/// Anti-abstraction-hell: bound how deep the abstraction stack may go. Abstractions constrain the
/// levels beneath them, but the depth of that stack — containment nesting and the declared level
/// count — is itself capped, so growth can't spiral into an infinite tower of indirection.
fn abstraction_bounds(tree: &Tree, rules: &Rules, out: &mut Vec<Finding>) {
    if rules.max_layers > 0 && rules.layers.len() > rules.max_layers {
        out.push(
            Finding::new(
                "abstraction-layers",
                Severity::Error,
                format!(
                    "{} architecture layers declared (ceiling {}) — collapse levels; more layers is not more structure",
                    rules.layers.len(),
                    rules.max_layers
                ),
                Location::file("bonsai.toml"),
            )
            .with_fix("merge adjacent layers, or raise max_layers deliberately"),
        );
    }
    if rules.max_depth > 0 {
        let depth = containment_depth(tree);
        if depth > rules.max_depth {
            out.push(
                Finding::new(
                    "abstraction-depth",
                    Severity::Error,
                    format!(
                        "containment nested {depth} deep (ceiling {}) — flatten the tower of directories",
                        rules.max_depth
                    ),
                    Location::file(tree.root.clone()),
                )
                .with_fix("hoist over-nested modules up, or raise max_depth deliberately"),
            );
        }
    }
}

/// The deepest containment path length (a catamorphism; mirrors the leanness depth fold).
fn containment_depth(tree: &Tree) -> usize {
    let depths = tree.fold(&|node, kids: &[usize]| {
        1 + kids.iter().copied().max().unwrap_or(0) * (node.kind == Kind::Composite) as usize
    });
    depths.get(&tree.root).copied().unwrap_or(0)
}

/// **The migration valve.** Partition `findings` into (kept, exempted) using the `allow` list, so
/// a declared or in-flight exception doesn't fight the gate while new violations still fail. An
/// entry is `rule` or `rule:path-glob`; `rule` matches an exact rule id, a `foo` prefix of a
/// `foo-*` family, or `*`; the optional glob matches the finding's file (`*`/`**`/`pre*`/`*suf`/
/// `dir/**`). Returns `(kept, exempted)` — the caller reports the exempted count (never silent).
pub fn apply_allow(findings: Vec<Finding>, allow: &[String]) -> (Vec<Finding>, Vec<Finding>) {
    if allow.is_empty() {
        return (findings, Vec::new());
    }
    let mut kept = Vec::new();
    let mut exempted = Vec::new();
    for f in findings {
        if allow.iter().any(|e| matches_allow(&f, e)) {
            exempted.push(f);
        } else {
            kept.push(f);
        }
    }
    (kept, exempted)
}

fn matches_allow(f: &Finding, entry: &str) -> bool {
    let (rule_pat, path_pat) = match entry.split_once(':') {
        Some((r, p)) => (r, Some(p)),
        None => (entry, None),
    };
    let rule_ok =
        rule_pat == "*" || f.rule == rule_pat || f.rule.starts_with(&format!("{rule_pat}-"));
    if !rule_ok {
        return false;
    }
    match path_pat {
        None => true,
        Some(g) => glob_match(g, &f.location.file),
    }
}

/// A deliberately small path glob: `*`/`**` = any; `dir/**` or `pre*` = prefix; `*suf` = suffix;
/// otherwise exact.
fn glob_match(glob: &str, path: &str) -> bool {
    if glob == "*" || glob == "**" {
        return true;
    }
    if let Some(pre) = glob.strip_suffix("/**").or_else(|| glob.strip_suffix('*')) {
        return path.starts_with(pre);
    }
    if let Some(suf) = glob.strip_prefix('*') {
        return path.ends_with(suf);
    }
    path == glob
}

/// Bounded growth: no directory may hold more than `max_files_per_dir` tracked files directly.
fn bounded_growth(tree: &Tree, rules: &Rules, out: &mut Vec<Finding>) {
    if rules.max_files_per_dir == 0 {
        return;
    }
    for node in tree.nodes.values() {
        if node.kind != Kind::Composite {
            continue;
        }
        let direct = node
            .children
            .iter()
            .filter(|c| {
                tree.nodes
                    .get(*c)
                    .map(|n| n.kind == Kind::Atom)
                    .unwrap_or(false)
            })
            .count();
        if direct > rules.max_files_per_dir {
            out.push(
                Finding::new(
                    "conformance-size",
                    Severity::Error,
                    format!(
                        "{direct} files directly in this directory (ceiling {})",
                        rules.max_files_per_dir
                    ),
                    Location::file(node.id.clone()),
                )
                .with_fix("split into sub-modules, or relocate files to their coupled home"),
            );
        }
    }
}

/// Layering: every file-dependency edge must run downward (a file may depend only on files in
/// its own layer or a more-stable one). An upward edge is a forbidden coupling.
fn layering(tree: &Tree, graph: &CodeGraph, rules: &Rules, out: &mut Vec<Finding>) {
    if rules.layers.is_empty() {
        return;
    }
    let layer_of = build_layer_map(tree, &rules.layers);
    for (from, tos) in &graph.file_deps {
        let Some(&fi) = layer_of.get(from) else {
            continue;
        };
        for to in tos {
            let Some(&ti) = layer_of.get(to) else {
                continue;
            };
            if ti > fi {
                out.push(
                    Finding::new(
                        "conformance-layering",
                        Severity::Error,
                        format!(
                            "{from} (layer '{}') depends UPWARD on {to} (layer '{}')",
                            rules.layers[fi], rules.layers[ti]
                        ),
                        Location::file(from.clone()),
                    )
                    .with_related(vec![Location::file(to.clone())])
                    .with_fix("invert the dependency, or move the shared code down a layer"),
                );
            }
        }
    }
}

/// Placement enforcement: a misplaced file (used only from one other directory) fails the gate.
fn placement(graph: &CodeGraph, rules: &Rules, out: &mut Vec<Finding>) {
    if !rules.enforce_placement {
        return;
    }
    for m in graph.misplacements() {
        out.push(
            Finding::new(
                "conformance-placement",
                Severity::Error,
                format!(
                    "used only from {}/ — belongs there, not in {}/",
                    m.suggested_dir,
                    if m.current_dir.is_empty() {
                        "the repo root"
                    } else {
                        &m.current_dir
                    }
                ),
                Location::file(m.file.clone()),
            )
            .with_fix(format!(
                "bonsai apply {} {}/… --write",
                m.file, m.suggested_dir
            )),
        );
    }
}

/// Map each file (atom-node id = repo-relative path) to its layer index, by walking up to the
/// nearest ancestor directory carrying a `layer` facet that names one of the declared layers.
/// Files with no layered ancestor are absent (unjudgeable — never a false violation).
fn build_layer_map(tree: &Tree, layers: &[String]) -> BTreeMap<String, usize> {
    let idx: BTreeMap<&str, usize> = layers
        .iter()
        .enumerate()
        .map(|(i, l)| (l.as_str(), i))
        .collect();
    let mut out = BTreeMap::new();
    for node in tree.nodes.values() {
        if node.kind != Kind::Atom {
            continue;
        }
        let mut cur = Some(node.id.clone());
        while let Some(id) = cur {
            let Some(n) = tree.nodes.get(&id) else { break };
            if let Some(layer) = n.facets.get("layer") {
                if let Some(&i) = idx.get(layer.as_str()) {
                    out.insert(node.id.clone(), i);
                    break;
                }
            }
            cur = n.parent.clone();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// A tree with three layered dirs; `kernel/k.rs` wrongly depends on `cli/c.rs` (upward).
    fn layered_config() -> Config {
        let toml = r#"
[bonsai]
root = "."

[rules]
layers = ["kernel", "cli"]
max_files_per_dir = 2

[[node]]
id = "kernel"
kind = "composite"
parent = "."
facets = { layer = "kernel" }

[[node]]
id = "cli"
kind = "composite"
parent = "."
facets = { layer = "cli" }
"#;
        Config::from_toml(toml).unwrap()
    }

    #[test]
    fn layering_flags_upward_dependency() {
        let cfg = layered_config();
        let tree = cfg.into_tree(None).unwrap();
        // graph: kernel/k.rs -> cli/c.rs  (upward: kernel is more stable than cli)
        let mut g = CodeGraph::default();
        g.file_deps
            .entry("kernel/k.rs".into())
            .or_default()
            .insert("cli/c.rs".into());
        // both files need a home node so the layer map resolves; add them as atoms via overlay-
        // style ids is unnecessary — build_layer_map walks parents, and k.rs' parent is "kernel".
        let mut tree = tree;
        add_atom(&mut tree, "kernel/k.rs", "kernel");
        add_atom(&mut tree, "cli/c.rs", "cli");

        let rules = cfg.rules.clone().unwrap();
        let f = evaluate(&tree, Some(&g), &rules);
        assert!(
            f.iter().any(|x| x.rule == "conformance-layering"),
            "upward dep should be flagged: {f:?}"
        );

        // the reverse edge (cli -> kernel, downward) is allowed
        let mut g2 = CodeGraph::default();
        g2.file_deps
            .entry("cli/c.rs".into())
            .or_default()
            .insert("kernel/k.rs".into());
        let f2 = evaluate(&tree, Some(&g2), &rules);
        assert!(
            !f2.iter().any(|x| x.rule == "conformance-layering"),
            "downward dep must be allowed: {f2:?}"
        );
    }

    #[test]
    fn bounded_growth_flags_overfull_dir() {
        let cfg = layered_config(); // max_files_per_dir = 2
        let mut tree = cfg.into_tree(None).unwrap();
        for i in 0..3 {
            add_atom(&mut tree, &format!("kernel/f{i}.rs"), "kernel");
        }
        let rules = cfg.rules.clone().unwrap();
        let f = evaluate(&tree, None, &rules);
        assert!(
            f.iter()
                .any(|x| x.rule == "conformance-size" && x.location.file == "kernel"),
            "overfull dir should be flagged: {f:?}"
        );
    }

    #[test]
    fn abstraction_bounds_cap_layers_and_depth() {
        // too many declared layers
        let mut rules = Rules {
            layers: vec!["a".into(), "b".into(), "c".into()],
            max_layers: 2,
            ..Default::default()
        };
        let tree = Config::from_toml("[bonsai]\nroot=\".\"\n")
            .unwrap()
            .into_tree(None)
            .unwrap();
        let f = evaluate(&tree, None, &rules);
        assert!(
            f.iter().any(|x| x.rule == "abstraction-layers"),
            "3 layers > 2 must flag: {f:?}"
        );

        // nesting deeper than max_depth
        rules = Rules {
            max_depth: 1,
            ..Default::default()
        };
        let mut deep = Config::from_toml("[bonsai]\nroot=\".\"\n")
            .unwrap()
            .into_tree(None)
            .unwrap();
        add_composite(&mut deep, "a", ".");
        add_composite(&mut deep, "a/b", "a");
        add_atom(&mut deep, "a/b/f.rs", "a/b");
        let g = evaluate(&deep, None, &rules);
        assert!(
            g.iter().any(|x| x.rule == "abstraction-depth"),
            "deep tree > max_depth 1 must flag: {g:?}"
        );
    }

    #[test]
    fn allow_list_exempts_matching_findings_and_reports_them() {
        let findings = vec![
            Finding::new(
                "conformance-layering",
                Severity::Error,
                "up",
                Location::file("legacy/x.rs"),
            ),
            Finding::new(
                "conformance-layering",
                Severity::Error,
                "up",
                Location::file("core/y.rs"),
            ),
            Finding::new(
                "contract-seal",
                Severity::Error,
                "gone",
                Location::file("codec/src"),
            ),
        ];
        // exempt all layering violations under legacy/, plus every contract-seal
        let allow = vec![
            "conformance-layering:legacy/**".to_string(),
            "contract-seal".to_string(),
        ];
        let (kept, exempted) = apply_allow(findings, &allow);
        // core/y.rs layering stays; legacy/x.rs + the seal are exempted
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].location.file, "core/y.rs");
        assert_eq!(exempted.len(), 2);
    }

    /// Helper: attach an atom child to an existing parent, both-ways (one-home).
    fn add_atom(tree: &mut Tree, id: &str, parent: &str) {
        use crate::model::{Node, Plane};
        let mut n = Node::new(id, Kind::Atom, Plane::Code);
        n.parent = Some(parent.to_string());
        tree.nodes.insert(id.to_string(), n);
        if let Some(p) = tree.nodes.get_mut(parent) {
            if !p.children.contains(&id.to_string()) {
                p.children.push(id.to_string());
            }
        }
    }

    /// Helper: attach a composite (directory) child to an existing parent.
    fn add_composite(tree: &mut Tree, id: &str, parent: &str) {
        use crate::model::{Node, Plane};
        let mut n = Node::new(id, Kind::Composite, Plane::Code);
        n.parent = Some(parent.to_string());
        tree.nodes.insert(id.to_string(), n);
        if let Some(p) = tree.nodes.get_mut(parent) {
            if !p.children.contains(&id.to_string()) {
                p.children.push(id.to_string());
            }
        }
    }
}
