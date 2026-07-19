# Changelog

All notable changes to Bonsai. Format loosely follows [Keep a Changelog]; versions are
[SemVer](https://semver.org). Dates are ISO-8601.

## [Unreleased]

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
