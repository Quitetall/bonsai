# Changelog

All notable changes to Bonsai. Format loosely follows [Keep a Changelog]; versions are
[SemVer](https://semver.org). Dates are ISO-8601.

## [Unreleased]

### Added — repository digital twin and executable architecture
- **Declarative blueprints** for slots, typed ports, flows, invariants, implementations, variants,
  direct compatibility adapters, fusion policy, and executable witnesses.
- **Shape locks and semantic diffs** with content-stable BLAKE3 identities. Implementation changes
  remain within one identity; topology/schema/invariant changes require a new identity.
- **Transitive evolution checks** that resolve `supersedes` identities and require every historical
  permanent-port contract to remain represented in the new shape.
- **Typed contract scaffolding** for Rust, Python, C, and TypeScript, including schema constants,
  input/output envelopes, implementation signatures, and contract-test surfaces. Generation is
  non-overwriting.
- **Automatic admission-gate integration**: `bonsai check` discovers root and `.bonsai/blueprints/`
  contracts, validates them, fails on missing or drifting locks, and reruns the exact witness
  contract pinned into lock schema v2.
- **Immutable typed fact snapshots** in bundled SQLite, source provenance, mutable refs over
  immutable history, bounded BQL dependency/impact traversal, and a read-only GraphQL projection.
- **Katana integration** through brokered MCP tools, replay-recorded results, a native deterministic
  Bonsai oracle, and optional data-only impact/shape-safe workflows.
- **LamQuant production proof** locking the existing LML lossless pipeline and its typed-chain,
  independent reference-DAG, and firmware/desktop byte-equality witnesses.

### Changed
- The framework is now Apache-2.0 licensed to support broad adoption across infrastructure,
  production, and meta-repository ecosystems.

### Added — polyglot duplication
- **Python function-clone detection** — the clone fold now spans `.rs` (brace-matched) *and*
  `.py` (indentation-based `def`/`async def` blocks); `Workspace::from_dir_ext` reads both. Excludes
  `def test_*` (pytest cases duplicate legitimately). Extends the "duplicated" pillar to LamQuant's
  Python half; dogfood surfaces 509 clone groups across the codebase.

### Deferred
- Move-engine SCIP-precision (nested `use crate::{…}` groups, `super::`/`self::`) is intentionally
  NOT done via source-grep — it would risk corrupting code in a tool that promises "reference-safe."
  It needs the SCIP symbol resolver (tracked deferred item in ADR 0134); the current resolver stays
  correct-for-the-idiomatic-case and honestly warns on what it can't prove.

### Added — duplication, deeper
- **Function-level clone detection** (`clones.rs`, surfaced by `lean`): extracts each function
  body (brace-matched, string/comment-safe), normalizes away comments/whitespace, and groups
  byte-identical normalized bodies across files — the copy-pasted-function (Type-1/2) clones that
  file-level dup misses. Excludes the signature (so renamed clones still match), trivial bodies
  (min-lines threshold), and `#[cfg(test)]` regions. New `clone-function` rule. Dogfood: 129
  clone groups surfaced on LamQuant.

### Added — ergonomics
- **Diff-scoped checking** — `bonsai check --staged` (index vs HEAD) / `--since <ref>` scope
  file-level findings to the changed files, so the pre-commit hook judges *the addition* rather
  than pre-existing debt in untouched files. Repo-wide gates (leanness ratchet, tree invariants,
  abstraction bounds) always apply. The installed hook now uses `--staged`.

### Added — false-positive reduction (trust)
- **Inline `// bonsai:allow` suppression** — the line-level FP valve: a finding is waived when its
  own line or the line above carries `// bonsai:allow` (any rule), `// bonsai:allow(dead-code)`
  (one rule / a `foo-*` family), or `// bonsai:allow(a, b)` (a set). `check` reports how many were
  waived (never silent). Lets a dev clear a genuine false positive at the source instead of
  fighting the gate.
- **Cross-file contract anchors** — a triggered file satisfies its `must_impl` anchor when the
  `impl` lives in *any* file under the level's path (against a `pub` type the file defines), not
  only the same file — eliminating the most common contract false positive.

### Added — nothing forgotten
- **Forgotten-code fold** (`forgotten.rs`, surfaced by `lean`): **unwired modules** — a `.rs`
  under `src/` that no `mod` declaration includes, so it isn't even compiled (the purest
  "forgotten"); and **undocumented public API** — module-level `pub` items with no doc comment.
  Source-based (works without a SCIP index), advisory (warning/info). Conservative: crate roots,
  aggregators, and cargo-discovered trees (bin/examples/benches/tests) are never flagged, and a
  module is only "unwired" when its name appears in no `mod` declaration at all.

### Added — structural health
- **Dependency-cycle detection** (`CodeGraph::dependency_cycles`, iterative Tarjan SCC over the
  real `file_deps` graph): mutually-entangled module groups — the canonical structural-inefficiency
  smell — are reported by `lean` (warning) and gated by `check` when `[rules] forbid_cycles = true`.
  Monorepo-safe (no recursion). New `structure-cycle` rule.

### Added — governed growth
- **Placement oracle** (`bonsai place <file>`): ranks the directories a file couples to and names
  its home — exact SCIP coupling for an indexed file, `crate::`-import scan for a new one; prints
  the reference-safe move when it's misplaced.
- **Conformance rules** (`[rules]`): `layers` (downward-only dependency direction), `max_files_per_dir`
  (bounded growth), `enforce_placement` (a misplaced file fails the gate). Opt-in per repo.
- **Architecture contracts** (`[[contract]]`): per-level fitness functions — *grow-through-anchor-traits*
  (`when_defines` → `must_impl`), *forbidden reach* (`forbid`), and *sealed seams* (`sealed`: an
  abstraction that may be extended with new impls but never replaced). Enforced fail-closed by `check`.
- **Automatic pre-commit gate** (`bonsai hook install`): writes a git hook that runs `bonsai check`
  on every commit, so a non-conformant addition is blocked with zero friction. `status` / `uninstall`
  included; resolves the hooks dir via `git rev-parse` (submodule/worktree-safe).

## [0.1.0] — 2026-07-19

First tagged release: the proof-of-concept hardened into a tool that runs from a laptop repo to a
monorepo.

### Added
- **Parallel content-scan substrate** (`scan.rs`): one gitignore-respecting walk, then a
  `rayon`-parallel read computing a stable **blake3** (256-bit) content hash, text/binary
  classification, line count, and on-demand near-dup shingles — replacing three serial re-reads and
  a non-stable 64-bit hash. ~204k files/s, ~437 MiB/s on a 200k-file synthetic tree.
- **Incremental scan cache** (`cache.rs`): `(size, mtime)`-keyed, atomic write, fail-safe (a stale
  cache can only cost a re-read, never a wrong answer). `.bonsai/scan-cache.json`, `--no-cache` opts
  out.
- **Uniform finding model** (`finding.rs`) with **text / JSON / SARIF 2.1.0** renderers and a rule
  catalog. `bonsai lean --format {text,json,sarif}` and `bonsai check --format …` for CI code
  scanning + a fail-closed exit code.
- **Multi-language SCIP merge**: `bonsai` discovers and merges every `*.scip` at the root, so a
  polyglot monorepo becomes one graph. Dead-code detection is conservatively Rust-only; the file DAG
  and misplacement span all languages.
- **Sub-repo detection**: nested repos/submodules are boundaries; cross-repo mirrors are
  informational, not counted as duplication.
- **Edge-case battery** (`tests/edge_cases.rs`) + **large-repo benchmark** (`tests/scale.rs`).

### Fixed
- **`apply` is byte-exact**: preserves each line's exact terminator (CRLF / LF / none) and the
  no-trailing-newline property; validates edits up front (a stale plan aborts with no partial write
  instead of panicking); writes atomically (temp + rename). Previously `lines().join("\n")` silently
  corrupted CRLF files.
- **Symlinks** are no longer followed (no phantom duplicate of a symlink's target, no cycle risk).
- **`dead_symbols`** no longer panics when a symbol's source file is missing/unreadable.

### Notes
- Deferred (design record in ADR 0134): a SCIP-precise move resolver (glob/re-export), a Rust-native
  docs fold, wiki/C4 export.
