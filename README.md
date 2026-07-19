# Bonsai

**One recursive atom/parent engine for code structure *and* documentation.** A general-purpose
structure formatter: point it at any repo and get a clean capability tree, a complexity map,
reference-safe file moves, and a fail-closed leanness gate — with an automatic configuration you
can hand-tune to put anything anywhere.

Design record: [`docs/decisions/0134`](../docs/decisions/0134-bonsai-unified-atom-parent-engine.md).

## The model

Three orthogonal structures, never conflated:

- **Containment** — a single-rooted tree of `Node`s. A node is an **atom** (leaf) or a
  **composite** (parent); a composite may be a child of another composite, so the tree recurses to
  arbitrary depth. This is an *initial algebra*: depth is structural and every derived fact
  (counts, complexity, leanness) is a **catamorphism** — write the one-level rule once, it folds at
  every depth.
- **Dependency** — an acyclic `depends_on` DAG (*uses*, not *part-of*).
- **Facets** — cross-cutting tags (`plane`, `language`, `layer`, …), never tree branches.

Code and docs are the **same** node type, differing only by `plane`. One engine governs both.

## Install

```sh
cargo install --path .      # or: cargo build --release
```

## Use

```sh
bonsai init                 # auto-derive bonsai.toml from the directory structure
bonsai tree                 # render the capability tree (composites + subtree file counts)
bonsai tree --files         # …expanding file atoms
bonsai stat                 # rank subtrees by relative complexity (no dev-hours — ranks only)
bonsai lean                 # duplicate files, empty structure, a leanness score (propose-only)
bonsai lean --save          # freeze the ratchet baseline (bonsai.lean.json)
bonsai check                # fail-closed gate: tree invariants + leanness-may-only-improve
bonsai diff  a/x.rs  b/x.rs # reference-safe move as a DRY-RUN diff (mutates nothing)
bonsai apply a/x.rs b/x.rs --write   # …actually rewrite the references + move the file
bonsai docs                 # run the recursive docs compose (if a [docs] plane is configured)
```

Every command takes `--root <dir>` (default `.`).

## `bonsai.toml`

`init` writes one composite node per directory. Edit it to **put anything anywhere** — a node's
`parent` is its *authored home*, which may differ from where the file physically sits (that
divergence is the misplacement signal). Missing fields are inferred.

```toml
[bonsai]
root = "."
skip = ["target", ".git", "node_modules"]   # never descended into

[docs]                                        # optional docs plane
compose = "python3 bonsai/docs/compose.py"

[[node]]
id = "codec"
kind = "composite"
parent = "."
facets = { layer = "codec", language = "rust" }

[[node]]
id = "codec/lml"
parent = "codec"
depends_on = ["codec"]
```

## Reference-safe moves

The one thing no other tool does. Moving a file in a path-addressed module system (Rust
`mod`/`use`) silently breaks every reference; `bonsai diff` computes the exact rewrite set and
shows it. The resolver is a trait — the shipped `RustModuleResolver` handles `crate::`-absolute
paths + `mod` relocation and honestly flags relative (`super::`/`self::`) paths for review; a
SCIP-graph resolver (exact symbols, glob re-exports, cross-language FFI) plugs into the same seam
later.

## Status

Kernel + moves + config/init + folds + leanness ratchet + docs-plane bridge are built and tested
(`cargo test`, clippy-clean). Deferred (seams exist): SCIP-graph resolver + Python↔Rust FFI
stitcher, Rust-native docs fold, dead-code/misplacement folds, near-duplicate detection,
wiki/C4 export. See ADR 0134's Progress Log.

## License

AGPL-3.0-or-later.
