//! The reference-safe move engine (ADR 0134) — the one genuinely-novel piece.
//!
//! Moving a file in a language with a *path-addressed* module system (Rust `mod`/`use`,
//! Python `import`) silently breaks every reference unless you rewrite them. Bonsai
//! computes the exact rewrite set from the workspace and emits it as a **dry-run diff** —
//! it mutates nothing unless `apply --write` is asked for.
//!
//! The [`ReferenceResolver`] trait is the seam: this module ships a correct-for-idiomatic
//! resolver driven by module-path arithmetic (`crate::a::b` prefix rewriting + `mod`
//! declaration relocation). A SCIP-graph resolver (exact symbol resolution, glob
//! re-exports, cross-language FFI edges) plugs into the *same* trait later (ADR 0134,
//! deferred — the abstraction is built now because the SCIP path builds on it).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A proposed relocation. Paths are repo-relative.
#[derive(Debug, Clone)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What a single edit does to a line. `Insert` adds a new line *after* `line`
/// (0 = top of file); `Delete` removes `line`; `Replace` swaps its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKind {
    Replace,
    Delete,
    Insert,
}

/// One line-level rewrite in one file, with the reason it exists (for the diff legend).
#[derive(Debug, Clone)]
pub struct Edit {
    pub path: PathBuf,
    pub line: usize, // 1-indexed; for Insert, the line to insert after (0 = file top)
    pub before: String,
    pub after: String,
    pub reason: String,
    pub kind: EditKind,
}

/// The full plan: the file move plus every reference rewrite it forces, plus honest
/// warnings for what the first-cut resolver cannot prove (relative paths, missing
/// declaring modules).
#[derive(Debug, Clone, Default)]
pub struct MovePlan {
    pub mv: Option<Move>,
    pub edits: Vec<Edit>,
    pub warnings: Vec<String>,
}

/// A read view of the tree the resolver reasons over. Built from disk or from test pairs;
/// the resolver never touches the filesystem itself (pure, deterministic, testable).
pub struct Workspace {
    pub root: PathBuf,
    /// repo-relative path → file contents.
    pub files: BTreeMap<PathBuf, String>,
}

impl Workspace {
    /// Build from an in-memory set of (path, content) pairs — the test constructor.
    pub fn from_pairs<I, P, S>(root: impl Into<PathBuf>, pairs: I) -> Self
    where
        I: IntoIterator<Item = (P, S)>,
        P: Into<PathBuf>,
        S: Into<String>,
    {
        let files = pairs
            .into_iter()
            .map(|(p, s)| (p.into(), s.into()))
            .collect();
        Workspace {
            root: root.into(),
            files,
        }
    }

    /// Build from disk: every `.rs` file under `root` (respecting `.gitignore` via the shared
    /// walk policy). Paths are stored repo-relative so plans and diffs are location-independent.
    pub fn from_dir(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut files = BTreeMap::new();
        for rel in crate::walk::files_with_ext(&root, &[], &["rs"]) {
            if let Ok(text) = std::fs::read_to_string(root.join(&rel)) {
                files.insert(rel, text);
            }
        }
        Ok(Workspace { root, files })
    }
}

/// The seam. A resolver turns a proposed [`Move`] into a [`MovePlan`] against a
/// [`Workspace`], mutating nothing. Implementors: the module-path resolver (here) and,
/// later, a SCIP-graph resolver (ADR 0134).
pub trait ReferenceResolver {
    /// Language/scheme this resolver claims a move for (e.g. a `.rs` file). If no resolver
    /// claims a move, `bonsai diff` still shows the bare rename with a "no resolver" note.
    fn claims(&self, mv: &Move) -> bool;
    fn plan(&self, ws: &Workspace, mv: &Move) -> MovePlan;
}

// ─────────────────────────── Rust module-path resolver ───────────────────────────

/// Reference-safe resolver for Rust files inside a `src/` tree. Handles the modern idiom
/// exactly: `crate::a::b` absolute-path rewriting + `mod` declaration relocation. Flags
/// (does not silently mishandle) `super::`/`self::` relative paths — the documented
/// first-cut boundary the SCIP resolver will close.
pub struct RustModuleResolver;

impl ReferenceResolver for RustModuleResolver {
    fn claims(&self, mv: &Move) -> bool {
        mv.from.extension().and_then(|e| e.to_str()) == Some("rs")
            && mv.from.components().any(|c| c.as_os_str() == "src")
    }

    fn plan(&self, ws: &Workspace, mv: &Move) -> MovePlan {
        let mut plan = MovePlan {
            mv: Some(mv.clone()),
            ..Default::default()
        };

        let (from_segs, to_segs) = match (mod_segments(&mv.from), mod_segments(&mv.to)) {
            (Some(f), Some(t)) => (f, t),
            _ => {
                plan.warnings.push(format!(
                    "cannot derive a module path for {} → {}; emitting bare rename only",
                    mv.from.display(),
                    mv.to.display()
                ));
                return plan;
            }
        };
        if from_segs.is_empty() || to_segs.is_empty() {
            plan.warnings.push(
                "move touches a crate root (lib.rs/main.rs); refusing — that is a crate \
                 restructure, not a module move"
                    .into(),
            );
            return plan;
        }

        // ── 1. `mod` declaration relocation ───────────────────────────────────────
        let src_dir = src_dir_of(&mv.from).expect("claims() guaranteed a src/ ancestor");
        let (from_parent, from_name) = from_segs.split_at(from_segs.len() - 1);
        let (to_parent, to_name) = to_segs.split_at(to_segs.len() - 1);
        let from_name = &from_name[0];
        let to_name = &to_name[0];

        match declaring_file(ws, &src_dir, from_parent) {
            Some(df) => match find_mod_decl(&ws.files[&df], from_name) {
                Some((lno, line)) => plan.edits.push(Edit {
                    path: df,
                    line: lno,
                    before: line,
                    after: String::new(),
                    reason: format!("remove `mod {from_name};` from its old parent"),
                    kind: EditKind::Delete,
                }),
                None => plan.warnings.push(format!(
                    "expected `mod {from_name};` in {} but did not find it (macro-generated \
                     module, or already absent?)",
                    df.display()
                )),
            },
            None => plan.warnings.push(format!(
                "could not locate the module file that declares `mod {from_name};`"
            )),
        }

        let mod_vis = "";
        match declaring_file(ws, &src_dir, to_parent) {
            Some(tf) => {
                let insert_after = last_mod_line(&ws.files[&tf]);
                plan.edits.push(Edit {
                    path: tf,
                    line: insert_after,
                    before: String::new(),
                    after: format!("{mod_vis}mod {to_name};"),
                    reason: format!("declare `mod {to_name};` in its new parent"),
                    kind: EditKind::Insert,
                });
            }
            None => plan.warnings.push(format!(
                "the new parent module for `{}` does not exist yet — create it and add \
                 `mod {to_name};` (path: {})",
                to_segs.join("::"),
                expected_parent_path(&src_dir, to_parent).display()
            )),
        }

        // ── 2. `crate::`-absolute path rewrites across the whole workspace ─────────
        let old_qual = format!("crate::{}", from_segs.join("::"));
        let new_qual = format!("crate::{}", to_segs.join("::"));
        for (path, content) in &ws.files {
            for (i, line) in content.lines().enumerate() {
                if let Some(rewritten) = replace_path_prefix(line, &old_qual, &new_qual) {
                    plan.edits.push(Edit {
                        path: path.clone(),
                        line: i + 1,
                        before: line.to_string(),
                        after: rewritten,
                        reason: format!("rewrite `{old_qual}` → `{new_qual}`"),
                        kind: EditKind::Replace,
                    });
                }
            }
        }

        // ── 3. honest boundary: relative paths the first-cut resolver won't touch ──
        let rel_hits: usize = ws
            .files
            .values()
            .map(|c| {
                c.matches(&format!("super::{from_name}")).count()
                    + c.matches(&format!("self::{from_name}")).count()
            })
            .sum();
        if rel_hits > 0 {
            plan.warnings.push(format!(
                "{rel_hits} relative reference(s) to `{from_name}` via super::/self:: — the \
                 module-path resolver rewrites only `crate::`-absolute paths; review these \
                 by hand (the SCIP resolver will resolve them, ADR 0134)"
            ));
        }
        if let Some(moved) = ws.files.get(&mv.from) {
            if moved.contains("super::") || moved.contains("self::") {
                plan.warnings.push(format!(
                    "{} itself uses super::/self:: — its module path changed, so those \
                     relative paths may now resolve differently; review",
                    mv.from.display()
                ));
            }
        }

        plan.edits
            .sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        plan
    }
}

/// The `crate::`-relative module segments a file contributes, e.g. `src/a/b.rs` → `[a, b]`,
/// `src/a/mod.rs` → `[a]`, `src/lib.rs` → `[]`. `None` if the path is not a `.rs` file
/// under a `src/` dir.
fn mod_segments(rel: &Path) -> Option<Vec<String>> {
    let comps: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect();
    let src_idx = comps.iter().position(|c| c == "src")?;
    let mut segs: Vec<String> = comps[src_idx + 1..].to_vec();
    let last = segs.last_mut()?;
    *last = last.strip_suffix(".rs")?.to_string();
    if matches!(
        segs.last().map(String::as_str),
        Some("lib" | "main" | "mod")
    ) {
        segs.pop();
    }
    Some(segs)
}

/// The path prefix up to and including `src` for a file under a `src/` tree.
fn src_dir_of(rel: &Path) -> Option<PathBuf> {
    let comps: Vec<_> = rel.components().collect();
    let idx = comps.iter().position(|c| c.as_os_str() == "src")?;
    Some(comps[..=idx].iter().collect())
}

/// Where `parent_segs` is declared: crate root (`lib.rs`/`main.rs`) for the empty parent,
/// else the flat `foo.rs` or nested `foo/mod.rs` form — whichever exists in the workspace.
fn declaring_file(ws: &Workspace, src_dir: &Path, parent_segs: &[String]) -> Option<PathBuf> {
    if parent_segs.is_empty() {
        for cand in ["lib.rs", "main.rs"] {
            let p = src_dir.join(cand);
            if ws.files.contains_key(&p) {
                return Some(p);
            }
        }
        return None;
    }
    let mut base = src_dir.to_path_buf();
    for s in &parent_segs[..parent_segs.len() - 1] {
        base.push(s);
    }
    let last = parent_segs.last().unwrap();
    let flat = base.join(format!("{last}.rs"));
    if ws.files.contains_key(&flat) {
        return Some(flat);
    }
    let nested = base.join(last).join("mod.rs");
    if ws.files.contains_key(&nested) {
        return Some(nested);
    }
    None
}

/// The flat-form path we'd *expect* a missing parent module to live at (for the warning).
fn expected_parent_path(src_dir: &Path, parent_segs: &[String]) -> PathBuf {
    if parent_segs.is_empty() {
        return src_dir.join("lib.rs");
    }
    let mut base = src_dir.to_path_buf();
    for s in &parent_segs[..parent_segs.len() - 1] {
        base.push(s);
    }
    base.join(format!("{}.rs", parent_segs.last().unwrap()))
}

/// Find a `mod <name>;` declaration line (any visibility), returning (1-indexed line, text).
fn find_mod_decl(content: &str, name: &str) -> Option<(usize, String)> {
    for (i, line) in content.lines().enumerate() {
        if is_mod_decl(line, name) {
            return Some((i + 1, line.to_string()));
        }
    }
    None
}

/// True iff `line` declares module `name` (`mod n;`, `pub mod n;`, `pub(crate) mod n;`),
/// not a `mod n { … }` inline module (those don't move with a file).
fn is_mod_decl(line: &str, name: &str) -> bool {
    let t = line.trim();
    let t = t.strip_prefix("pub").map(str::trim_start).unwrap_or(t);
    let t = if t.starts_with("(") {
        // strip a `(crate)` / `(super)` visibility qualifier
        t.find(')').map(|i| t[i + 1..].trim_start()).unwrap_or(t)
    } else {
        t
    };
    let Some(rest) = t.strip_prefix("mod ") else {
        return false;
    };
    let rest = rest.trim();
    rest == format!("{name};") || rest == format!("{name} ;")
}

/// The 1-indexed line of the last `mod …;` declaration (new `mod` decls go after it to keep
/// them grouped); 0 if there are none (insert at top).
fn last_mod_line(content: &str) -> usize {
    content
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub("))
                && l.trim_end().ends_with(';')
        })
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
}

/// Rewrite every occurrence of path-prefix `old` → `new` in `line`, respecting path
/// boundaries (a match must be followed by `::` or a non-identifier char, and preceded by a
/// non-identifier char), so `crate::a::b` matches in `use crate::a::b::Foo;` but
/// `crate::ab` does not. Returns the rewritten line, or `None` if nothing matched.
fn replace_path_prefix(line: &str, old: &str, new: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    let mut hit = false;
    while i < line.len() {
        if line[i..].starts_with(old) {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let end = i + old.len();
            let after_ok =
                end >= line.len() || line[end..].starts_with("::") || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                out.push_str(new);
                i = end;
                hit = true;
                continue;
            }
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    hit.then_some(out)
}

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

// ─────────────────────────────── diff rendering ───────────────────────────────

impl MovePlan {
    /// Render as a human-readable dry-run diff (git-style rename header + per-file hunks).
    pub fn render_diff(&self) -> String {
        let mut out = String::new();
        if let Some(mv) = &self.mv {
            out.push_str(&format!(
                "rename {} → {}\n",
                mv.from.display(),
                mv.to.display()
            ));
        }
        if self.edits.is_empty() && self.warnings.is_empty() {
            out.push_str("  (no reference rewrites needed)\n");
        }

        // group edits by file, preserving the pre-sorted line order
        let mut by_file: BTreeMap<&Path, Vec<&Edit>> = BTreeMap::new();
        for e in &self.edits {
            by_file.entry(&e.path).or_default().push(e);
        }
        for (path, edits) in &by_file {
            out.push_str(&format!("\n--- {}\n", path.display()));
            for e in edits {
                match e.kind {
                    EditKind::Replace => {
                        out.push_str(&format!("  @{} · {}\n", e.line, e.reason));
                        out.push_str(&format!("  - {}\n", e.before.trim_end()));
                        out.push_str(&format!("  + {}\n", e.after.trim_end()));
                    }
                    EditKind::Delete => {
                        out.push_str(&format!("  @{} · {}\n", e.line, e.reason));
                        out.push_str(&format!("  - {}\n", e.before.trim_end()));
                    }
                    EditKind::Insert => {
                        let anchor = if e.line == 0 {
                            "top".to_string()
                        } else {
                            format!("after {}", e.line)
                        };
                        out.push_str(&format!("  @{anchor} · {}\n", e.reason));
                        out.push_str(&format!("  + {}\n", e.after.trim_end()));
                    }
                }
            }
        }

        if !self.warnings.is_empty() {
            out.push_str("\n⚠ warnings (manual review — the resolver will not touch these):\n");
            for w in &self.warnings {
                out.push_str(&format!("  · {w}\n"));
            }
        }
        out
    }

    /// The count of files this plan would edit (excluding the moved file itself).
    pub fn touched_files(&self) -> usize {
        let mut s: std::collections::BTreeSet<&Path> = std::collections::BTreeSet::new();
        for e in &self.edits {
            s.insert(&e.path);
        }
        s.len()
    }
}

/// Dispatch a move to the first resolver that claims it (module-path today; SCIP later).
pub fn plan_move(ws: &Workspace, mv: &Move) -> MovePlan {
    let resolvers: [&dyn ReferenceResolver; 1] = [&RustModuleResolver];
    for r in resolvers {
        if r.claims(mv) {
            return r.plan(ws, mv);
        }
    }
    MovePlan {
        mv: Some(mv.clone()),
        edits: Vec::new(),
        warnings: vec![format!(
            "no resolver claims {} — bare rename only (no reference rewrites computed)",
            mv.from.display()
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny crate: `lib.rs` declares `mod a` + `mod util`; `a.rs` declares `mod b`;
    /// `util.rs` references `crate::a::b::Widget`. Moving `a/b.rs` → `util/b.rs` must:
    /// remove `mod b;` from a.rs, add it to util.rs, and rewrite the `crate::a::b` path.
    fn crate_ws() -> Workspace {
        Workspace::from_pairs(
            ".",
            [
                ("src/lib.rs", "mod a;\nmod util;\n"),
                ("src/a.rs", "pub mod b;\n\npub fn hi() {}\n"),
                ("src/a/b.rs", "pub struct Widget;\n"),
                (
                    "src/util.rs",
                    "use crate::a::b::Widget;\n\npub fn make() -> Widget { Widget }\n",
                ),
            ],
        )
    }

    #[test]
    fn relocates_mod_decl_and_rewrites_paths() {
        let ws = crate_ws();
        let mv = Move {
            from: "src/a/b.rs".into(),
            to: "src/util/b.rs".into(),
        };
        let plan = plan_move(&ws, &mv);

        // mod b removed from a.rs
        assert!(plan
            .edits
            .iter()
            .any(|e| e.path == Path::new("src/a.rs")
                && e.kind == EditKind::Delete
                && e.before.contains("mod b")));
        // mod b added to util.rs
        assert!(plan
            .edits
            .iter()
            .any(|e| e.path == Path::new("src/util.rs")
                && e.kind == EditKind::Insert
                && e.after.contains("mod b")));
        // crate::a::b → crate::util::b in util.rs
        assert!(plan
            .edits
            .iter()
            .any(|e| e.path == Path::new("src/util.rs")
                && e.kind == EditKind::Replace
                && e.after.contains("crate::util::b::Widget")));
    }

    #[test]
    fn path_boundary_is_respected() {
        // `crate::ab` must NOT match the `crate::a` prefix.
        assert_eq!(
            replace_path_prefix("use crate::ab::X;", "crate::a", "crate::z"),
            None
        );
        assert_eq!(
            replace_path_prefix("use crate::a::X;", "crate::a", "crate::z"),
            Some("use crate::z::X;".to_string())
        );
        assert_eq!(
            replace_path_prefix("let x: crate::a = y;", "crate::a", "crate::z"),
            Some("let x: crate::z = y;".to_string())
        );
    }

    #[test]
    fn warns_on_missing_parent_module() {
        let ws = crate_ws();
        // move into a parent module that doesn't exist yet
        let mv = Move {
            from: "src/a/b.rs".into(),
            to: "src/nope/b.rs".into(),
        };
        let plan = plan_move(&ws, &mv);
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("does not exist yet")));
    }

    #[test]
    fn refuses_crate_root_move() {
        let ws = crate_ws();
        let mv = Move {
            from: "src/lib.rs".into(),
            to: "src/core.rs".into(),
        };
        let plan = plan_move(&ws, &mv);
        assert!(plan.warnings.iter().any(|w| w.contains("crate root")));
    }

    #[test]
    fn mod_segments_handles_forms() {
        assert_eq!(
            mod_segments(Path::new("src/a/b.rs")),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            mod_segments(Path::new("src/a/mod.rs")),
            Some(vec!["a".into()])
        );
        assert_eq!(mod_segments(Path::new("src/lib.rs")), Some(vec![]));
        assert_eq!(mod_segments(Path::new("src/main.rs")), Some(vec![]));
    }
}
