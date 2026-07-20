# Bonsai

**A local-first repository digital twin and executable architecture contract.** Bonsai lets a team
declare what a serious production system does, bind that logical shape to real implementations,
prove the bindings, and lock the shape against accidental drift. Underneath, it combines a
Kythe-like typed fact graph, bounded impact queries, GraphQL, structural analysis, reference-safe
refactoring, and fail-closed repository gates.

The original recursive atom/parent engine remains the structural substrate. Blueprints add the
missing semantic layer: slots, typed ports, flows, variants, compatibility adapters, invariants,
implementation bindings, and executable witnesses. A new predictor can inhabit the existing
`Predict` slot; changing `Transform → Predict` into a different topology requires a new blueprint
identity. Optimized fused code is allowed only when a differential witness proves it equivalent.

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

Production complexity is spread across code, schemas, build systems, infrastructure, tests, docs,
and compatibility history. Search can locate fragments but cannot state which boundaries are
intentional, which changes are implementation-only, or what must never break. Bonsai turns those
claims into versioned data and deterministic gates while retaining the structural hygiene engine
that keeps a repository at its provable minimum.

## The model

Four orthogonal structures, never conflated:

- **Logical shape** — blueprint slots connected by typed ports and flows. Shape identity excludes
  prose and implementation bindings but includes topology, schemas, compatibility, fusion policy,
  and invariants.
- **Typed facts** — immutable, content-addressed SQLite snapshots with source provenance. Mutable
  refs such as `main` point to immutable snapshots; they never rewrite history.

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
bonsai place src/foo.rs         # where does this belong? (coupling oracle; new or indexed file)
bonsai hook install             # install the automatic pre-commit gate (runs check on commit)

# Declarative architecture
bonsai blueprint validate architecture/codec.toml
bonsai blueprint verify architecture/codec.toml --root . --out witnesses.json
bonsai blueprint lock architecture/codec.toml --results witnesses.json --out codec.lock.json
bonsai blueprint check architecture/codec.toml --lock codec.lock.json
bonsai blueprint diff old.toml new.toml
bonsai blueprint scaffold architecture/codec.toml --language rust --out src/contracts

# Repository fact graph and query surfaces
bonsai graph snapshot --blueprint architecture/codec.toml --ref main
bonsai graph query --snapshot main --bql 'IMPACT codec.v1#slot/predict DEPTH 3'
bonsai graph graphql --query '{ impact(entity: "codec.v1#slot/predict", depth: 3) { entities } }'
```

Every command takes `--root <dir>` (default `.`).

## Shape locks and compatibility

A locked blueprint is a change-control boundary, not a diagram. `bonsai blueprint lock` refuses to
freeze an invalid shape, a non-locked lifecycle, a missing witness, or a failed witness. The lock
pins the canonical shape digest, permanent external ports, and witness set. The ordinary
`bonsai check` command automatically discovers `bonsai.blueprint.toml` and
`.bonsai/blueprints/*.toml`, so existing hooks and CI protect the new contract. For a locked
blueprint, the lock pins each witness's kind, command, and coverage; `bonsai check` reruns that
exact witness contract and fails closed on drift or a failing command.

Permanent compatibility is expressed with versioned ports and direct, witnessed adapters. Bonsai
rejects adapter chains: each supported old representation must adapt directly to the current
boundary, keeping compatibility cost visible and preventing a fragile tower of historical shims.
Multi-slot fused implementations require every slot to opt in and a differential witness covering
the complete fused region.

When a blueprint declares `supersedes`, the repository gate resolves both identities and requires
the new shape to retain every complete permanent-port contract from its predecessor. Because each
generation carries the full historical set, this rule is transitive: arbitrarily old callers stay
represented, and any conversion remains a direct adapter immediately below the stable boundary.
Historical blueprint files must remain in `.bonsai/blueprints/`; a missing predecessor fails closed.

Scaffolding emits non-overwriting typed input/output envelopes, schema constants, implementation
signatures, and contract-test surfaces for Rust, Python, C, and TypeScript. Payloads cross the
generated seam as schema-governed bytes so repositories can bind their own generated or native
types without making Bonsai a schema compiler. Generated files are a starting seam, never a source
of authority; authored schemas, witnesses, and the lock remain authoritative.

## Digital twin and query APIs

Blueprint adapters emit provenance-bearing facts into a bundled SQLite store. Snapshots are
deduplicated by BLAKE3 over their parent and canonical facts. BQL deliberately starts small and
bounded—`DEPENDENCIES` or `IMPACT`, an entity, and an explicit depth from 1 to 64. The same store is
projected through read-only GraphQL for tools that prefer a schema-driven API.

This split is intentional: SQLite and deterministic traversal are the authority; BQL and GraphQL
are interfaces over it. The first source adapters ingest blueprints and SCIP symbol/file
dependencies. Additional adapters can ingest build metadata, deployment manifests, API schemas,
ownership, policy, and runtime evidence without changing the core fact model.

## Katana agentic harness

Katana integrates Bonsai without giving the model architectural authority. Its MCP surface exposes
`bonsai.query`, `bonsai.impact`, `bonsai.agent_context`, and `bonsai.shape_check`. Calls traverse
Katana's normal broker, sandbox, output budget, and event log. Repositories containing a blueprint
also receive a deterministic post-edit and post-task Bonsai oracle; service/critical tiers cannot
declare completion while the gate is red. See Katana's `bonsai-integration.md` and data-only
workflow package.

## Governed growth — where new code goes, enforced

The point isn't just to *report* structure — it's to hold the repo at its provable minimum as it
grows. Bonsai does that with a placement oracle plus a fail-closed admission gate that runs
automatically on every commit:

- **`bonsai place <file>`** ranks the directories a file couples to and names its home (exact SCIP
  coupling for an indexed file; a `crate::`-import scan for one you just wrote).
- **`bonsai hook install`** writes a git pre-commit hook that runs `bonsai check` — so nobody has to
  remember to run anything; a non-conformant addition is blocked before it lands.
- **`bonsai check`** enforces, from `bonsai.toml`, three families of rule (all opt-in):

```toml
[rules]
layers = ["kernel", "engine", "cli"]   # dependencies must run DOWNWARD (never couple upward)
max_files_per_dir = 40                  # bound growth — no directory bloats into a dumping ground
enforce_placement = true                # a misplaced file fails the gate, not just warns

[[contract]]                            # the architecture gate for one level
path  = "codec/src"
level = "codec"
when_defines = "WireMagic"              # a file that declares a wire format…
must_impl    = "Codec"                  # …must implement the Codec anchor ("grow through anchors")
sealed       = "Codec"                  # …and the Codec seam itself can't be replaced, only extended
forbid = ["std::mem::transmute"]        # an escape hatch this level may never reach for
```

A programmer can add a new codec a layer down that fulfills `Codec`, but cannot replace the `Codec`
abstraction or add a wire format that bypasses it — the gate forces each addition to do what its
level needs, and no more, without friction.

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

The blueprint kernel, witness runner, shape locks/diffs, four-language scaffolding, automatic hook
integration, typed SQLite snapshots, BQL traversal, GraphQL projection, structural engine,
reference-safe moves, leanness ratchet, JSON/SARIF findings, SCIP merge, FFI stitching, and docs
plane are built and tested. The next adapter wave is build/deployment/schema ingestion and
long-running GraphQL/MCP serving; the current CLI surfaces are stable local composition points.

## License

Apache-2.0. See `LICENSE`.
