//! The Bonsai tree — the initial-algebra containment tree + the dependency DAG, with
//! the fail-closed invariant check (the "order-lock") and the catamorphism fold.

use crate::model::{Node, NodeId};
use std::collections::{BTreeMap, HashSet};

/// A set of nodes rooted at a single node. Both planes (docs + code) live in one tree.
pub struct Tree {
    pub nodes: BTreeMap<NodeId, Node>,
    pub root: NodeId,
}

impl Tree {
    pub fn new(root: impl Into<NodeId>) -> Self {
        Tree {
            nodes: BTreeMap::new(),
            root: root.into(),
        }
    }

    pub fn insert(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Fail-closed invariants (ADR 0134). Returns the list of violations (empty = valid):
    /// exactly one parentless root; every `parent`/`child`/`depends_on` id resolves;
    /// containment is a single-rooted tree (one home per node — parent and child agree —
    /// reachable from root, no cycles); the dependency graph is acyclic.
    pub fn check(&self) -> Vec<String> {
        let mut errs = Vec::new();

        match self.nodes.get(&self.root) {
            None => errs.push(format!("root '{}' is not a node", self.root)),
            Some(r) if r.parent.is_some() => {
                errs.push(format!("root '{}' must have no parent", self.root))
            }
            _ => {}
        }

        // referential integrity + one-home (parent/child must agree both ways)
        for n in self.nodes.values() {
            match &n.parent {
                Some(p) => match self.nodes.get(p) {
                    None => errs.push(format!("{}: parent '{}' missing", n.id, p)),
                    Some(par) if !par.children.contains(&n.id) => errs.push(format!(
                        "{}: parent '{}' does not list it as a child (one-home broken)",
                        n.id, p
                    )),
                    _ => {}
                },
                None if n.id != self.root => errs.push(format!(
                    "{}: no parent (only the root may be parentless)",
                    n.id
                )),
                None => {}
            }
            for c in &n.children {
                match self.nodes.get(c) {
                    None => errs.push(format!("{}: child '{}' missing", n.id, c)),
                    Some(ch) if ch.parent.as_ref() != Some(&n.id) => errs.push(format!(
                        "{}: child '{}' does not name it as parent (one-home broken)",
                        n.id, c
                    )),
                    _ => {}
                }
            }
            for d in &n.depends_on {
                if !self.nodes.contains_key(d) {
                    errs.push(format!("{}: depends_on '{}' missing", n.id, d));
                }
            }
        }

        // containment: reachable from root, no cycles (walk parents up)
        for n in self.nodes.values() {
            if n.id == self.root {
                continue;
            }
            let mut seen = HashSet::new();
            let mut cur = Some(n.id.clone());
            let mut ok = false;
            while let Some(id) = cur {
                if id == self.root {
                    ok = true;
                    break;
                }
                if !seen.insert(id.clone()) {
                    errs.push(format!("{}: containment cycle", n.id));
                    ok = true; // cycle already reported; don't also report unreachable
                    break;
                }
                cur = self.nodes.get(&id).and_then(|x| x.parent.clone());
            }
            if !ok {
                errs.push(format!("{}: not reachable from root '{}'", n.id, self.root));
            }
        }

        if let Some(cycle) = self.dep_cycle() {
            errs.push(format!("dependency cycle: {}", cycle.join(" -> ")));
        }

        errs
    }

    /// Detect a cycle in the `depends_on` DAG (DFS three-colouring). Returns the cycle
    /// path if any.
    fn dep_cycle(&self) -> Option<Vec<NodeId>> {
        // 0 = unseen, 1 = on-stack, 2 = done
        let mut colour: BTreeMap<NodeId, u8> = BTreeMap::new();

        fn visit(
            id: &NodeId,
            nodes: &BTreeMap<NodeId, Node>,
            colour: &mut BTreeMap<NodeId, u8>,
            path: &mut Vec<NodeId>,
        ) -> Option<Vec<NodeId>> {
            colour.insert(id.clone(), 1);
            path.push(id.clone());
            if let Some(n) = nodes.get(id) {
                for d in &n.depends_on {
                    match colour.get(d).copied().unwrap_or(0) {
                        1 => {
                            let mut c = path.clone();
                            c.push(d.clone());
                            return Some(c);
                        }
                        0 => {
                            if let Some(c) = visit(d, nodes, colour, path) {
                                return Some(c);
                            }
                        }
                        _ => {}
                    }
                }
            }
            path.pop();
            colour.insert(id.clone(), 2);
            None
        }

        for id in self.nodes.keys() {
            if colour.get(id).copied().unwrap_or(0) == 0 {
                let mut path = Vec::new();
                if let Some(c) = visit(id, &self.nodes, &mut colour, &mut path) {
                    return Some(c);
                }
            }
        }
        None
    }

    /// **Catamorphism** — fold the containment tree bottom-up, applying
    /// `f(node, folded_children)` at each node over its already-folded children. The
    /// uniform any-depth rollup engine: coverage, complexity, counts, leanness — write
    /// the one-level rule once, it applies at every depth (ADR 0134). Returns the value
    /// computed AT EACH node so callers can inspect subtree rollups.
    pub fn fold<T, F>(&self, f: &F) -> BTreeMap<NodeId, T>
    where
        T: Clone,
        F: Fn(&Node, &[T]) -> T,
    {
        let mut out: BTreeMap<NodeId, T> = BTreeMap::new();
        self.fold_rec(&self.root, f, &mut out);
        out
    }

    fn fold_rec<T, F>(&self, id: &NodeId, f: &F, out: &mut BTreeMap<NodeId, T>) -> Option<T>
    where
        T: Clone,
        F: Fn(&Node, &[T]) -> T,
    {
        let n = self.nodes.get(id)?;
        let mut kids = Vec::with_capacity(n.children.len());
        for c in &n.children {
            if let Some(v) = self.fold_rec(c, f, out) {
                kids.push(v);
            }
        }
        let v = f(n, &kids);
        out.insert(id.clone(), v.clone());
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Plane};

    fn n(id: &str, kind: Kind, parent: Option<&str>, children: &[&str]) -> Node {
        let mut node = Node::new(id, kind, Plane::Code);
        node.parent = parent.map(|s| s.to_string());
        node.children = children.iter().map(|s| s.to_string()).collect();
        node
    }

    fn sample() -> Tree {
        let mut t = Tree::new("root");
        t.insert(n("root", Kind::Composite, None, &["a", "b"]));
        t.insert(n("a", Kind::Composite, Some("root"), &["a.1"]));
        t.insert(n("a.1", Kind::Atom, Some("a"), &[]));
        t.insert(n("b", Kind::Atom, Some("root"), &[]));
        t
    }

    #[test]
    fn valid_tree_passes() {
        let t = sample();
        assert!(t.check().is_empty(), "unexpected: {:?}", t.check());
    }

    #[test]
    fn fold_counts_leaves_at_every_depth() {
        let t = sample();
        // subtree leaf count = a catamorphism (atoms→1, composites→sum(children)).
        let counts = t.fold(&|node: &Node, kids: &[usize]| match node.kind {
            Kind::Atom => 1usize,
            Kind::Composite => kids.iter().sum(),
        });
        assert_eq!(counts["root"], 2); // a.1 + b
        assert_eq!(counts["a"], 1); // a.1
        assert_eq!(counts["b"], 1);
    }

    #[test]
    fn one_home_violation_detected() {
        let mut t = Tree::new("root");
        t.insert(n("root", Kind::Composite, None, &["a"]));
        t.insert(n("a", Kind::Atom, Some("root"), &[]));
        // 'b' claims root as parent but root doesn't list it → one-home broken.
        t.insert(n("b", Kind::Atom, Some("root"), &[]));
        assert!(t.check().iter().any(|e| e.contains("one-home")));
    }

    #[test]
    fn dependency_cycle_detected() {
        let mut t = Tree::new("root");
        t.insert(n("root", Kind::Composite, None, &["a", "b"]));
        let mut a = n("a", Kind::Atom, Some("root"), &[]);
        let mut b = n("b", Kind::Atom, Some("root"), &[]);
        a.depends_on = vec!["b".into()];
        b.depends_on = vec!["a".into()];
        t.insert(a);
        t.insert(b);
        assert!(t.check().iter().any(|e| e.contains("dependency cycle")));
    }
}
