//! The node model — one schema for both planes (ADR 0134).

use std::collections::BTreeMap;

/// A node is an **atom** (leaf) or a **composite** (parent). The distinction is
/// *collapsed*: a composite is itself a [`Node`] and may appear as a child of another
/// composite, so the tree recurses to arbitrary depth. (Mirrors ABIR's
/// `Chain`-is-a-`Stage` closure, ADR 0074 — the type-proven fragment of the ideal.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Irreducible leaf — a code entity/seam, or a prose atom.
    Atom,
    /// A composite of child nodes.
    Composite,
}

/// The two planes that share one engine (ADR 0134): code structure and documentation.
/// Docs are a first-class node type, exactly like code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    Code,
    Docs,
}

/// Hierarchical dotted id (e.g. `codec.abir.codec`). Dotted → never runs out, unlike
/// the flat ADR counter; survives indefinite nesting.
pub type NodeId = String;

/// A first-class node. Docs and code nodes are the *same* type, differing only by
/// [`Plane`], [`facets`](Node::facets), and what [`anchors`](Node::anchors) point at.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub kind: Kind,
    pub plane: Plane,
    /// Exactly one containment parent — `None` only for the single root.
    pub parent: Option<NodeId>,
    /// Child nodes (a composite child is admissible → recursion).
    pub children: Vec<NodeId>,
    /// The dependency DAG (uses, not part-of); must stay acyclic.
    pub depends_on: Vec<NodeId>,
    /// Cross-cutting axes (layer, repo, language, diataxis, maturity, policy…).
    pub facets: BTreeMap<String, String>,
    /// Code: file/symbol paths; Docs: the source atom path(s).
    pub anchors: Vec<String>,
}

impl Node {
    pub fn new(id: impl Into<NodeId>, kind: Kind, plane: Plane) -> Self {
        Node {
            id: id.into(),
            kind,
            plane,
            parent: None,
            children: Vec::new(),
            depends_on: Vec::new(),
            facets: BTreeMap::new(),
            anchors: Vec::new(),
        }
    }
}
