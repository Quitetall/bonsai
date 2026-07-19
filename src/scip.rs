//! The SCIP substrate (ADR 0134, Stratum 1) — parse a `.scip` index into a queryable
//! [`CodeGraph`]: symbol definitions, symbol→symbol reference edges (attributed to the
//! enclosing definition), and the derived file→file dependency DAG.
//!
//! This is the machine-derived code model the authored capability tree anchors into. It
//! upgrades Bonsai from path/regex heuristics to exact, compiler-grade facts — the seam the
//! precise move resolver, the dead-code fold, and the Python↔Rust FFI stitcher all build on.
//!
//! Generation is external (`rust-analyzer scip .` / `scip-python`); this module only reads.

use protobuf::Message;
use scip::types::Index;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A half-open source range, normalized to `(start_line, start_col, end_line, end_col)`
/// (SCIP encodes single-line ranges as 3 ints, multi-line as 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub sl: i32,
    pub sc: i32,
    pub el: i32,
    pub ec: i32,
}

impl Range {
    fn from_scip(r: &[i32]) -> Option<Range> {
        match r.len() {
            3 => Some(Range { sl: r[0], sc: r[1], el: r[0], ec: r[2] }),
            n if n >= 4 => Some(Range { sl: r[0], sc: r[1], el: r[2], ec: r[3] }),
            _ => None,
        }
    }
    /// Does this range enclose the point `(line, col)` (inclusive on both ends)?
    fn contains(&self, line: i32, col: i32) -> bool {
        let after_start = (line, col) >= (self.sl, self.sc);
        let before_end = (line, col) <= (self.el, self.ec);
        after_start && before_end
    }
    /// Rough size for "innermost" tie-breaking (smaller = tighter enclosure).
    fn span(&self) -> (i32, i32) {
        (self.el - self.sl, self.ec - self.sc)
    }
}

/// A symbol defined within the indexed crate.
#[derive(Debug, Clone)]
pub struct Sym {
    pub display: String,
    pub kind: i32,
    pub file: String,
    pub def: Range,
    pub enclosing: Range,
    /// True if this symbol implements a trait method (SCIP `is_implementation`) — reached
    /// via dynamic dispatch, which SCIP does not record as a reference edge, so it must be
    /// treated as a liveness root or it looks falsely dead.
    pub is_impl: bool,
}

/// The parsed code model: local definitions + reference edges + file dependency DAG.
#[derive(Debug, Clone, Default)]
pub struct CodeGraph {
    /// Local symbol id → its definition.
    pub symbols: BTreeMap<String, Sym>,
    /// Local symbol → the local symbols it references (attributed by enclosure).
    pub refs: BTreeMap<String, BTreeSet<String>>,
    /// File → files it depends on (references a symbol defined there).
    pub file_deps: BTreeMap<String, BTreeSet<String>>,
    /// Total reference occurrences seen (for stats).
    pub occurrences: usize,
}

const ROLE_DEFINITION: i32 = 1; // SymbolRole::Definition (bit 0)
const KIND_MODULE: i32 = 29; // SymbolInformation.Kind::Module

impl CodeGraph {
    /// Parse a `.scip` protobuf index from disk.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let index = Index::parse_from_bytes(&bytes)?;
        Ok(Self::from_index(&index))
    }

    /// Build the graph from an in-memory SCIP index (also the test entry point).
    pub fn from_index(index: &Index) -> Self {
        let mut g = CodeGraph::default();

        // Pass 1 — collect local definitions + per-file enclosure tables.
        // enclosers[file] = list of (symbol, enclosing_range) for definitions in that file.
        let mut enclosers: BTreeMap<String, Vec<(String, Range)>> = BTreeMap::new();
        for doc in &index.documents {
            let file = doc.relative_path.clone();
            for occ in &doc.occurrences {
                let is_def = occ.symbol_roles & ROLE_DEFINITION != 0;
                if !is_def || occ.symbol.is_empty() || is_local_scope(&occ.symbol) {
                    continue;
                }
                let Some(def) = Range::from_scip(&occ.range) else { continue };
                let enclosing = Range::from_scip(&occ.enclosing_range).unwrap_or(def);
                // display_name/kind/is_impl come from the doc's symbol table when present.
                let info = doc.symbols.iter().find(|s| s.symbol == occ.symbol);
                let display = info.map(|s| s.display_name.clone()).unwrap_or_default();
                let kind = info.map(|s| s.kind.value()).unwrap_or_default();
                let is_impl = info
                    .map(|s| s.relationships.iter().any(|r| r.is_implementation))
                    .unwrap_or(false);
                g.symbols.insert(
                    occ.symbol.clone(),
                    Sym { display, kind, file: file.clone(), def, enclosing, is_impl },
                );
                enclosers.entry(file.clone()).or_default().push((occ.symbol.clone(), enclosing));
            }
        }
        // Innermost-first so attribution picks the tightest enclosing definition.
        for list in enclosers.values_mut() {
            list.sort_by_key(|(_, r)| r.span());
        }

        // Pass 2 — attribute each reference occurrence to its enclosing local symbol.
        for doc in &index.documents {
            let file = &doc.relative_path;
            let table = enclosers.get(file);
            for occ in &doc.occurrences {
                if occ.symbol_roles & ROLE_DEFINITION != 0 {
                    continue; // definitions aren't references
                }
                g.occurrences += 1;
                // only edges *between local symbols* matter for the intra-crate graph
                let Some(target) = g.symbols.get(&occ.symbol) else { continue };
                let target_file = target.file.clone();
                let Some(start) = Range::from_scip(&occ.range) else { continue };
                if let Some(list) = table {
                    if let Some((from_sym, _)) =
                        list.iter().find(|(_, r)| r.contains(start.sl, start.sc))
                    {
                        if from_sym != &occ.symbol {
                            g.refs.entry(from_sym.clone()).or_default().insert(occ.symbol.clone());
                        }
                    }
                }
                // Module targets are `mod X;`/`use path::to::module` *containment/import*
                // edges, not coupling — excluding them keeps the dependency DAG (and the
                // misplacement fold that reads its reverse edges) about real usage.
                if *file != target_file && target.kind != KIND_MODULE {
                    g.file_deps.entry(file.clone()).or_default().insert(target_file);
                }
            }
        }
        g
    }

    /// The dead-code entry set: every symbol whose definition is **public** (its def line
    /// starts with `pub`), plus `fn main`. Reachability from these roots is "live"; the rest
    /// is internal dead code. Pub-as-root is deliberately conservative — it never reports a
    /// public API item as dead (which would be a false positive the user would rightly hate).
    pub fn public_roots(&self, root: &Path) -> BTreeSet<String> {
        let mut roots = BTreeSet::new();
        let mut cache: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, sym) in &self.symbols {
            // `main` + SCIP-declared impls are always live roots
            if sym.is_impl || sym.display == "main" {
                roots.insert(id.clone());
                continue;
            }
            let lines = cache
                .entry(sym.file.clone())
                .or_insert_with(|| read_lines(root, &sym.file));
            let sl = sym.def.sl.max(0) as usize;
            if let Some(line) = lines.get(sl) {
                let t = line.trim_start();
                if t.starts_with("pub ") || t.starts_with("pub(") {
                    roots.insert(id.clone());
                    continue;
                }
            }
            // rust-analyzer emits no `is_implementation` relationships, so detect a
            // trait-impl method from its enclosing `impl Trait for Type` header: such a
            // method is reachable via dynamic dispatch (a ref SCIP records against the
            // *trait* method, not this impl), so treat it as a live root.
            if enclosing_impl_is_trait(lines, sl) {
                roots.insert(id.clone());
            }
        }
        roots
    }

    /// Internal symbols unreachable from the public API + `main` + trait impls = dead code.
    /// Symbols inside a `#[cfg(test)]` region are excluded (they are live via the test
    /// harness, not the API). `kinds`, when non-empty, restricts to those SCIP Kind values.
    /// The result is deliberately conservative — pub/impl/test are never reported dead.
    pub fn dead_symbols(&self, root: &Path, kinds: &[i32]) -> Vec<&Sym> {
        let reach = self.reachable(&self.public_roots(root));
        let mut test_from: BTreeMap<String, Option<usize>> = BTreeMap::new();
        let mut dead: Vec<&Sym> = self
            .symbols
            .iter()
            .filter(|(id, _)| !reach.contains(*id))
            .map(|(_, s)| s)
            .filter(|s| kinds.is_empty() || kinds.contains(&s.kind))
            .filter(|s| {
                let cutoff = test_from
                    .entry(s.file.clone())
                    .or_insert_with(|| test_region_start(root, &s.file));
                // keep only symbols defined BEFORE any #[cfg(test)] region
                cutoff.map(|line| (s.def.sl.max(0) as usize) < line).unwrap_or(true)
            })
            .collect();
        dead.sort_by(|a, b| (&a.file, a.def.sl).cmp(&(&b.file, b.def.sl)));
        dead
    }

    /// Misplacement (the coupling "belongs-in" signal, ADR 0134): a file whose symbols are
    /// referenced **exclusively** from one *other* directory belongs in that directory. Uses
    /// the file-dependency DAG's reverse edges. Deliberately conservative — it only fires
    /// when every dependent sits in a single directory that isn't the file's own, so a shared
    /// utility used from many places is never flagged.
    pub fn misplacements(&self) -> Vec<Misplacement> {
        let mut rdeps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (from, tos) in &self.file_deps {
            for to in tos {
                rdeps.entry(to.clone()).or_default().insert(from.clone());
            }
        }
        let mut out = Vec::new();
        for (file, dependents) in &rdeps {
            let cur = dir_of(file);
            let dirs: BTreeSet<&str> = dependents.iter().map(|d| dir_of(d)).collect();
            if dirs.len() == 1 {
                let suggested = *dirs.iter().next().unwrap();
                if suggested != cur {
                    out.push(Misplacement {
                        file: file.clone(),
                        current_dir: cur.to_string(),
                        suggested_dir: suggested.to_string(),
                        dependents: dependents.len(),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.file.cmp(&b.file));
        out
    }

    /// Symbols reachable from `entry` symbols by following reference edges (BFS closure).
    pub fn reachable(&self, entry: &BTreeSet<String>) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = entry.clone();
        let mut stack: Vec<String> = entry.iter().cloned().collect();
        while let Some(s) = stack.pop() {
            if let Some(outs) = self.refs.get(&s) {
                for o in outs {
                    if seen.insert(o.clone()) {
                        stack.push(o.clone());
                    }
                }
            }
        }
        seen
    }
}

/// A file whose symbols are used only from a single other directory (a relocation suggestion).
#[derive(Debug, Clone)]
pub struct Misplacement {
    pub file: String,
    pub current_dir: String,
    pub suggested_dir: String,
    pub dependents: usize,
}

/// The directory portion of a repo-relative path (`""` for a top-level file).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// SCIP marks function-local symbols with a `local ` prefix — those are never dead-code or
/// cross-file dependencies, so we drop them from the graph.
fn is_local_scope(symbol: &str) -> bool {
    symbol.starts_with("local ")
}

/// Walk up from a definition line to its nearest enclosing `impl …` header and report
/// whether it is a *trait* impl (`impl Trait for Type`). Trait-impl methods are dispatch
/// entry points — live even with zero recorded reference edges.
fn enclosing_impl_is_trait(lines: &[String], def_line: usize) -> bool {
    let mut i = def_line.min(lines.len().saturating_sub(1));
    loop {
        let t = lines[i].trim_start();
        if t.starts_with("impl ") || t.starts_with("impl<") {
            // `impl Trait for Type` has ` for ` before the opening brace / where-clause.
            let head = t.split('{').next().unwrap_or(t);
            return head.contains(" for ");
        }
        if i == 0 {
            return false;
        }
        i -= 1;
    }
}

fn read_lines(root: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(root.join(file))
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default()
}

/// The 0-indexed line of the first `#[cfg(test)]` attribute in a file (the conventional
/// single test-module boundary); `None` if the file has no test region. Everything at/after
/// it is test code, excluded from dead-code analysis (live via the harness, not the API).
fn test_region_start(root: &Path, file: &str) -> Option<usize> {
    read_lines(root, file)
        .iter()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document, Occurrence};

    fn occ(symbol: &str, range: Vec<i32>, def: bool, enc: Vec<i32>) -> Occurrence {
        let mut o = Occurrence::new();
        o.symbol = symbol.to_string();
        o.range = range;
        o.symbol_roles = if def { ROLE_DEFINITION } else { 0 };
        o.enclosing_range = enc;
        o
    }

    /// Two files: `a.rs` defines `foo`; `b.rs` defines `bar`, whose body references `foo`.
    /// Expect: edge bar→foo, and file_deps b.rs→a.rs.
    #[test]
    fn attributes_reference_to_enclosing_symbol() {
        let mut a = Document::new();
        a.relative_path = "a.rs".into();
        // foo defined at line 0, body spans lines 0..5
        a.occurrences.push(occ("foo#", vec![0, 3, 0, 6], true, vec![0, 0, 5, 0]));

        let mut b = Document::new();
        b.relative_path = "b.rs".into();
        // bar defined lines 0..5; reference to foo at line 2
        b.occurrences.push(occ("bar#", vec![0, 3, 0, 6], true, vec![0, 0, 5, 0]));
        b.occurrences.push(occ("foo#", vec![2, 8, 2, 11], false, vec![]));

        let mut idx = Index::new();
        idx.documents.push(a);
        idx.documents.push(b);

        let g = CodeGraph::from_index(&idx);
        assert!(g.symbols.contains_key("foo#") && g.symbols.contains_key("bar#"));
        assert!(g.refs.get("bar#").map(|s| s.contains("foo#")).unwrap_or(false), "{:?}", g.refs);
        assert!(g.file_deps.get("b.rs").map(|s| s.contains("a.rs")).unwrap_or(false));
        // foo is referenced; bar is not → reachable from {bar} includes foo
        let entry: BTreeSet<String> = ["bar#".to_string()].into_iter().collect();
        let reach = g.reachable(&entry);
        assert!(reach.contains("foo#") && reach.contains("bar#"));
    }

    #[test]
    fn module_containment_edges_excluded_from_coupling() {
        // b.rs only does `mod m;` (a reference to a Module-kind symbol defined in a.rs).
        // That is containment, not coupling → no file-dep edge, so a.rs is not "misplaced".
        let mut a = Document::new();
        a.relative_path = "a.rs".into();
        a.occurrences.push(occ("m#", vec![0, 4, 0, 5], true, vec![0, 0, 3, 0]));
        let mut minfo = scip::types::SymbolInformation::new();
        minfo.symbol = "m#".into();
        minfo.kind = scip::types::symbol_information::Kind::Module.into();
        a.symbols.push(minfo);

        let mut b = Document::new();
        b.relative_path = "b.rs".into();
        b.occurrences.push(occ("root#", vec![0, 0, 0, 4], true, vec![0, 0, 5, 0]));
        b.occurrences.push(occ("m#", vec![1, 4, 1, 5], false, vec![])); // `mod m;`

        let mut idx = Index::new();
        idx.documents.push(a);
        idx.documents.push(b);
        let g = CodeGraph::from_index(&idx);
        assert!(g.file_deps.is_empty(), "module edge leaked into coupling: {:?}", g.file_deps);
        assert!(g.misplacements().is_empty());
    }

    #[test]
    fn misplacement_flags_single_consumer_dir() {
        // util/helper.rs is referenced only from feature/*.rs → belongs under feature/.
        let mut g = CodeGraph::default();
        g.file_deps
            .entry("feature/a.rs".into())
            .or_default()
            .insert("util/helper.rs".into());
        g.file_deps
            .entry("feature/b.rs".into())
            .or_default()
            .insert("util/helper.rs".into());
        let m = g.misplacements();
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].file, "util/helper.rs");
        assert_eq!(m[0].suggested_dir, "feature");

        // add a same-dir consumer → no longer exclusively external → not flagged
        g.file_deps
            .entry("util/other.rs".into())
            .or_default()
            .insert("util/helper.rs".into());
        assert!(g.misplacements().is_empty(), "shared util must not be flagged");
    }

    #[test]
    fn drops_local_scope_symbols() {
        let mut d = Document::new();
        d.relative_path = "a.rs".into();
        d.occurrences.push(occ("local 1", vec![1, 0, 1, 3], true, vec![]));
        let mut idx = Index::new();
        idx.documents.push(d);
        let g = CodeGraph::from_index(&idx);
        assert!(g.symbols.is_empty());
    }
}
