//! Bonsai — one recursive atom/parent engine for code structure AND documentation.
//!
//! ADR 0134: docs and code are the *same* recursive tree, governed by one engine.
//! A [`Node`] is an **atom** (leaf) or a **composite** (parent); the distinction is
//! collapsed (as ABIR's `Chain` *is* a `Stage`, ADR 0074) so a composite may be a
//! child of a grandparent — the tree recurses to arbitrary depth (the depth-1 limit
//! of the legacy doc compose is gone). This is an **initial algebra**: arbitrary
//! depth is structural, and every derived fact is a **catamorphism** ([`Tree::fold`]).
//!
//! Three orthogonal structures, never conflated:
//! - **Containment** — a single-rooted acyclic tree, one home per node.
//! - **Dependency** — an acyclic DAG (`depends_on`), levelized toward stable nodes.
//! - **Facets** — cross-cutting axes (plane, layer, repo, diataxis, …), not branches.

pub mod config;
pub mod ffi;
pub mod lean;
pub mod model;
pub mod moves;
pub mod scip;
pub mod tree;

pub use config::{Config, NodeSpec};
pub use lean::{analyze, LeanBaseline, LeanReport};
pub use scip::CodeGraph;
pub use model::{Kind, Node, NodeId, Plane};
pub use moves::{Move, MovePlan, ReferenceResolver, RustModuleResolver, Workspace};
pub use tree::Tree;
