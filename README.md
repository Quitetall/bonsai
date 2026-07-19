# Bonsai

**A recursive atom/parent structure engine — `cargo fmt` for your whole repository.** Point it
at any tree (a laptop project or a monorepo) and it finds duplication, near-duplication, dead code,
empty structure, and misplaced files; renders a complexity map; performs **reference-safe file
moves**; and enforces a **fail-closed leanness gate** in CI. One engine governs both code and docs.

Bonsai reads *tracked* structure (it respects `.gitignore`), scans in parallel with a strong,
cached content hash, and reports through a uniform finding stream — human text, JSON, or **SARIF
2.1.0** for code-scanning CI.

```
$ bonsai lean
bonsai lean — 4004 files, 718 composites, depth 10
  leanness score : 0.9763  (1.0 = no measured waste)
  duplicate files: 109 across 107 group(s), 2569 redundant LOC
  cross-repo mirrors: 252 (identical across sub-repos — NOT counted as waste)
  near-duplicates: 152 pair(s) (≥0.85 similar, not identical)
    99%  crates/lamquant-tui/src/panels/output.rs  ~  tui/src/tui/panels/output.rs
  empty dirs     : 11
```

## Why

Refactoring has two halves. The **semantic** half (does the logic make sense?) needs a human. The
**structural** half — where a file lives, whether it's dead or duplicated, whether it's organized by
how it's actually used — is *computable*. Bonsai automates the structural half and holds the repo at
its provable minimum, tracked as a ratcheted trend so it can never silently rot.

## The model

Three orthogonal structures, never conflated:

- **Containment** — a single-rooted tree of `Node`s. A node is an **atom** (leaf) or a **composite**
  (parent); a composite may be a child of another composite, so the tree recurses to arbitrary
  depth. This is an *initial algebra*: depth is structural, and every derived fact (counts,
  complexity, leanness) is a **catamorphism** — write the one-level rule once, it folds at every
  depth.
- **Dependency** — an acyclic `depends_on` DAG (*uses*, not *part-of*).
- **Facets** — cross-cutting tags (`plane`, `language`, `layer`, …), never tree branches.

Code and docs are the **same** node type, differing only by `plane`.

## Install

```sh
cargo install --path .          # or: cargo build --release
```

## Use

```sh
bonsai init                     # auto-derive bonsai.toml from the directory structure
bonsai tree                     # render the capability tree (+ subtree file counts)
bonsai stat                     # rank subtrees by relative complexity (ranks only, no dev-hours)
bonsai lean                     # duplication, near-dup, dead code, empty structure, a leanness score
bonsai lean --format sarif      # …the same findings as SARIF 2.1.0 for CI code scanning
bonsai lean --save              # freeze the ratchet baseline (bonsai.lean.json)
bonsai check                    # fail-closed gate: tree invariants + leanness-may-only-improve
bonsai diff  a/x.rs  b/x.rs     # reference-safe move as a DRY-RUN diff (mutates nothing)
bonsai apply a/x.rs b/x.rs --write   # …actually rewrite the references + move the file
bonsai index --generate         # build a SCIP index (rust-analyzer) for dead-code/misplacement
bonsai tidy                     # propose reference-safe moves from the misplacement signal
bonsai ffi                      # stitch the Python↔Rust (PyO3) boundary SCIP can't see
```

Every command takes `--root <dir>` (default `.`).

## Scale

Bonsai runs one gitignore-respecting walk, then reads every file in parallel (`rayon`) with a stable
**blake3** content hash (256-bit — no collision guard needed) and an incremental `(size, mtime)`
cache. On a synthetic **200,000-file / 428 MiB** tree, a cold scan completes in ~1.0s
(**≈204k files/s, ≈437 MiB/s**); the dedup fold over all of it runs in ~0.1s. Run the benchmark
yourself:

```sh
cargo test --release --test scale -- --ignored --nocapture
```

Memory is bounded by the input's own size (a 32-byte hash + small metadata per file); nothing is
held resident across the whole tree, so the same binary serves a laptop repo and a monorepo.

## Sub-repos

Bonsai detects nested git repositories and submodules. A `LICENSE` or spec mirrored across
sub-repos is reported as an informational **mirror**, not counted as duplication — only copies
*within* one repository are waste.

## Reference-safe moves

Moving a file in a path-addressed module system (Rust `mod`/`use`) silently breaks every reference.
`bonsai diff` computes the exact rewrite set and shows it; `apply --write` performs it **byte-for-
byte** (CRLF and no-trailing-newline preserved) via an atomic temp-file + rename. The resolver is a
trait: the shipped `RustModuleResolver` handles `crate::`-absolute paths + `mod` relocation and
honestly flags relative (`super::`/`self::`) paths for review.

## SCIP-backed signals (polyglot)

With a [SCIP](https://github.com/sourcegraph/scip) index present, Bonsai measures **dead code**
(reachability from the public API, `main`, tests, and trait impls) and **misplacement** (a file used
only from one other directory belongs there). It **merges every `*.scip` at the root**, so a polyglot
monorepo (rust-analyzer + scip-python + scip-typescript + scip-clang + …) becomes one graph. Dead-code
detection is conservatively Rust-only; the file DAG and misplacement span all languages. Without an
index those signals are reported **unmeasured — never faked as zero**.

## CI

```yaml
# fail the build if leanness regressed, and upload findings for inline annotation
- run: bonsai check                                   # exit 1 on any regression
- run: bonsai lean --format sarif > bonsai.sarif      # for code-scanning upload
```

## `bonsai.toml`

`init` writes one composite node per directory. Edit it to **put anything anywhere** — a node's
`parent` is its *authored home*, which may differ from where the file physically sits (that
divergence is the misplacement signal). Missing fields are inferred.

```toml
[bonsai]
root = "."
skip = ["target", ".git", "node_modules"]     # never descended into

[[node]]
id = "codec"
kind = "composite"
parent = "."
facets = { layer = "codec", language = "rust" }
```

## Status

Kernel, reference-safe moves, config/init, the parallel+cached scan substrate, the leanness folds
(duplication, near-dup, dead code, misplacement, empty structure), the ratchet gate, the uniform
finding model (text/JSON/SARIF), multi-language SCIP merge, the Python↔Rust FFI stitcher, and the
docs-plane bridge are all built and tested (`cargo test`, clippy-clean, an edge-case battery, and a
large-repo benchmark). See `CHANGELOG.md`.

## License

AGPL-3.0-or-later. See `LICENSE`.
