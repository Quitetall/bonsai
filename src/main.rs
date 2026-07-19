//! Bonsai CLI (ADR 0134) — `cargo fmt` for your directory structure, plus the unified
//! docs+code capability tree. Phase-0 scaffold: `tree` is real; the reference-safe move
//! engine (`diff`/`apply`) is the next, hardest-part-first build.

use anyhow::{Context, Result};
use bonsai::config::Config;
use bonsai::model::{Kind, NodeId, Plane};
use bonsai::moves::{plan_move, Edit, EditKind, Move, MovePlan, Workspace};
use bonsai::tree::Tree;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "bonsai",
    version,
    about = "One recursive atom/parent engine for code structure AND docs (ADR 0134)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate `bonsai.toml` by auto-deriving the directory capability tree.
    Init {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Overwrite an existing bonsai.toml.
        #[arg(long)]
        force: bool,
    },
    /// Render the annotated capability tree from the model (loaded or auto-derived).
    Tree {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Expand file atoms (default: composites + a subtree file count).
        #[arg(long)]
        files: bool,
        /// Max depth to render (deeper nodes collapse into their count).
        #[arg(long)]
        depth: Option<usize>,
    },
    /// Print subtree rollups (file counts, complexity) — the catamorphism, on real data.
    Stat {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Show the top-N most complex subtrees.
        #[arg(long, default_value = "15")]
        top: usize,
    },
    /// Show the reference-safe move-plan for `<from> <to>` as a dry-run diff. Mutates nothing.
    Diff {
        from: PathBuf,
        to: PathBuf,
        /// Repo root the workspace is scanned from (default: cwd).
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Apply the move-plan for `<from> <to>` (rewriting references). Dry-run unless `--write`.
    Apply {
        from: PathBuf,
        to: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        write: bool,
    },
    /// Report leanness: exact-dup files, empty structure, score (propose-by-default).
    Lean {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Refresh the committed ratchet baseline (bonsai.lean.json).
        #[arg(long)]
        save: bool,
    },
    /// Fail-closed gate: tree invariants + leanness ratchet (CI / pre-commit).
    Check {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Docs plane: run the recursive compose executor (byte-identity gate + recursion).
    Docs {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Stitch the Python↔Rust FFI boundary (PyO3 exports ↔ Python imports) + print edges.
    Ffi {
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Propose (or apply) structural refactors from the leanness signals — misplaced files
    /// become reference-safe moves. Dry-run unless `--write`. Needs a SCIP index.
    Tidy {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        write: bool,
    },
    /// Parse a SCIP index into the code graph (symbols, ref edges, file DAG) + print stats.
    Index {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Path to the `.scip` file (default: `<root>/index.scip`).
        #[arg(long)]
        scip: Option<PathBuf>,
        /// Generate the index first via `rust-analyzer scip .`.
        #[arg(long)]
        generate: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Init { root, force } => cmd_init(&root, force),
        Cmd::Tree { root, files, depth } => cmd_tree(&root, files, depth),
        Cmd::Stat { root, top } => cmd_stat(&root, top),
        Cmd::Diff { from, to, root } => cmd_move(&root, &from, &to, false),
        Cmd::Apply {
            from,
            to,
            root,
            write,
        } => cmd_move(&root, &from, &to, write),
        Cmd::Lean { root, save } => cmd_lean(&root, save),
        Cmd::Check { root } => cmd_check(&root),
        Cmd::Docs { root } => cmd_docs(&root),
        Cmd::Index { root, scip, generate } => cmd_index(&root, scip.as_deref(), generate),
        Cmd::Ffi { root } => cmd_ffi(&root),
        Cmd::Tidy { root, write } => cmd_tidy(&root, write),
    }
}

/// The telos capstone: turn leanness signals into ready-to-apply refactors. Today it drives
/// the **misplacement** fold → a **reference-safe move** (the novel engine) for each file
/// used only from one other directory. Propose-by-default; `--write` executes (the structural
/// half of refactoring, done for you). Sequester-of-dead-code is the next tidy action.
fn cmd_tidy(root: &Path, write: bool) -> Result<()> {
    let scip = root.join("index.scip");
    if !scip.exists() {
        anyhow::bail!("no {} — run `bonsai index --generate` first", scip.display());
    }
    let g = bonsai::scip::CodeGraph::from_file(&scip)
        .with_context(|| format!("parsing {}", scip.display()))?;
    let mis = g.misplacements();
    if mis.is_empty() {
        println!("bonsai tidy: nothing to tidy — no misplaced files.");
        return Ok(());
    }

    println!(
        "bonsai tidy: {} misplacement(s) → reference-safe move proposal(s)\n",
        mis.len()
    );
    let mut applied = 0;
    for m in &mis {
        let base = m.file.rsplit_once('/').map(|(_, b)| b).unwrap_or(&m.file);
        let to = if m.suggested_dir.is_empty() {
            base.to_string()
        } else {
            format!("{}/{}", m.suggested_dir, base)
        };
        // recompute against fresh disk state so sequential applies stay correct
        let ws = Workspace::from_dir(root)?;
        let mv = Move { from: PathBuf::from(&m.file), to: PathBuf::from(&to) };
        if !ws.files.contains_key(&mv.from) {
            continue; // already moved by an earlier proposal
        }
        let plan = plan_move(&ws, &mv);
        println!(
            "▸ {} used only from {}/ ({} dependents)",
            m.file, m.suggested_dir, m.dependents
        );
        print!("{}", plan.render_diff());
        if write && plan.warnings.is_empty() {
            apply_plan(root, &plan)?;
            applied += 1;
            println!("  → applied");
        } else if write {
            println!("  → skipped (has warnings — review by hand)");
        }
        println!();
    }
    if write {
        println!("applied {applied}/{} proposal(s).", mis.len());
    } else {
        println!("(dry-run — pass `--write` to apply the warning-free proposals)");
    }
    Ok(())
}

/// Stitch and report the Python↔Rust FFI boundary — the permanent cross-language edges SCIP
/// can't see (it indexes each language alone). Nothing else joins these graphs.
fn cmd_ffi(root: &Path) -> Result<()> {
    let g = bonsai::ffi::scan(root);
    let modules = g
        .exports
        .iter()
        .filter(|e| matches!(e.kind, bonsai::ffi::ExportKind::Module))
        .count();
    println!(
        "bonsai ffi — {} PyO3 exports ({} module(s)), {} Python imports, {} stitched edge(s)",
        g.exports.len(),
        modules,
        g.imports.len(),
        g.edges.len()
    );
    // group edges by module
    let mut by_mod: BTreeMap<&str, Vec<&bonsai::ffi::FfiEdge>> = BTreeMap::new();
    for e in &g.edges {
        by_mod.entry(&e.module).or_default().push(e);
    }
    for (module, edges) in &by_mod {
        let rust = edges.first().map(|e| e.rust_file.as_str()).unwrap_or("?");
        println!("\n  ⟷ {module}  (Rust: {rust})");
        for e in edges {
            let names = if e.names.is_empty() {
                "*".to_string()
            } else {
                e.names.join(", ")
            };
            println!("     {}:{}  imports [{}]", e.py_file, e.py_line, names);
        }
    }
    if g.edges.is_empty() {
        println!("  (no FFI boundary found — no #[pymodule] imported from Python under this root)");
    }
    Ok(())
}

/// Parse (optionally generate) a SCIP index into the code graph and report stats. The
/// Stratum-1 substrate: exact symbol defs + reference edges + the file dependency DAG.
fn cmd_index(root: &Path, scip: Option<&Path>, generate: bool) -> Result<()> {
    let scip_path = scip
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("index.scip"));

    if generate {
        println!("bonsai index: rust-analyzer scip {}", root.display());
        let status = std::process::Command::new("rust-analyzer")
            .arg("scip")
            .arg(".")
            .current_dir(root)
            .status()
            .context("running `rust-analyzer scip .` (is rust-analyzer installed?)")?;
        if !status.success() {
            anyhow::bail!("rust-analyzer scip failed");
        }
    }
    if !scip_path.exists() {
        anyhow::bail!(
            "{} not found — run with --generate, or point --scip at an index",
            scip_path.display()
        );
    }

    let g = bonsai::scip::CodeGraph::from_file(&scip_path)
        .with_context(|| format!("parsing {}", scip_path.display()))?;

    let files: std::collections::BTreeSet<&String> = g.symbols.values().map(|s| &s.file).collect();
    let edges: usize = g.refs.values().map(|s| s.len()).sum();
    let dep_edges: usize = g.file_deps.values().map(|s| s.len()).sum();
    println!(
        "bonsai index — {} symbols across {} files, {} ref edges, {} refs seen",
        g.symbols.len(),
        files.len(),
        edges,
        g.occurrences
    );
    println!("  file dependency edges: {dep_edges}");

    // most-referenced local symbols (fan-in) — where the code concentrates its use
    let mut fan_in: BTreeMap<String, usize> = BTreeMap::new();
    for outs in g.refs.values() {
        for o in outs {
            *fan_in.entry(o.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(&String, usize)> = fan_in.iter().map(|(k, v)| (k, *v)).collect();
    ranked.sort_by_key(|(_, v)| std::cmp::Reverse(*v));
    println!("  top fan-in symbols:");
    for (sym, n) in ranked.into_iter().take(8) {
        let name = g.symbols.get(sym).map(|s| s.display.as_str()).filter(|s| !s.is_empty());
        println!("    {n:>3}×  {}", name.unwrap_or(sym));
    }

    // dead code: unreachable from the public API + main (all kinds, for inspection)
    let dead = g.dead_symbols(root, &[]);
    println!("  dead (unreachable from pub API + main): {}", dead.len());
    for s in dead.iter().take(20) {
        let name = if s.display.is_empty() { "?" } else { &s.display };
        println!("    kind {:>2}  {}:{}  {name}", s.kind, s.file, s.def.sl + 1);
    }
    Ok(())
}

/// Run the docs-plane compose executor (a first-class member of the same tree). The docs
/// are already Bonsai nodes (`plane = docs`); this drives the recursive fold that proves
/// byte-identity vs the legacy output + the composite-as-child recursion (ADR 0134).
fn cmd_docs(root: &Path) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let compose = cfg.docs.unwrap_or_default().compose;
    let parts: Vec<&str> = compose.split_whitespace().collect();
    let Some((prog, args)) = parts.split_first() else {
        anyhow::bail!("empty docs.compose command");
    };
    println!("bonsai docs: {compose}");
    let status = std::process::Command::new(prog)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("running docs compose: {compose}"))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("docs compose failed (exit {:?})", status.code());
    }
}

const LEAN_BASELINE: &str = "bonsai.lean.json";

fn cmd_lean(root: &Path, save: bool) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    let mut r = bonsai::lean::analyze(&tree, root);
    // measure SCIP-backed signals if an index is present (moves them out of "deferred")
    r.dead_code = bonsai::lean::dead_code_count(root);
    r.misplaced = bonsai::lean::misplacement_count(root);
    if r.dead_code.is_some() {
        r.deferred.retain(|d| !d.starts_with("dead-code"));
    }
    if r.misplaced.is_some() {
        r.deferred.retain(|d| !d.starts_with("misplacement"));
    }

    println!(
        "bonsai lean — {} files, {} composites, depth {}",
        r.files, r.composites, r.max_depth
    );
    if let Some(n) = r.dead_code {
        println!("  dead symbols   : {n}  (SCIP-measured)");
    }
    if let Some(n) = r.misplaced {
        println!("  misplaced files: {n}  (SCIP-measured)");
    }
    println!(
        "  leanness score : {:.4}  (1.0 = no measured waste)",
        r.score()
    );
    println!(
        "  duplicate files: {} across {} group(s), {} redundant LOC",
        r.dup_files(),
        r.dup_groups.len(),
        r.wasted_loc()
    );
    let near = bonsai::lean::near_duplicates(&tree, root, 0.85);
    println!("  near-duplicates: {} pair(s) (≥0.85 similar, not identical)", near.len());
    for nd in near.iter().take(8) {
        println!("    {:.0}%  {}  ~  {}", nd.similarity * 100.0, nd.a, nd.b);
    }
    println!("  empty dirs     : {}", r.empty_dirs.len());
    for g in r.dup_groups.iter().take(12) {
        println!("    ×{}  {}", g.files.len(), g.files.join("  ≡  "));
    }
    if !r.empty_dirs.is_empty() {
        println!(
            "  empty: {}{}",
            r.empty_dirs
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if r.empty_dirs.len() > 8 { " …" } else { "" }
        );
    }
    if r.deferred.is_empty() {
        println!("  deferred       : none — every signal measured");
    } else {
        println!("  deferred (unmeasured — never faked as 0):");
        for d in &r.deferred {
            println!("    · {d}");
        }
    }

    if save {
        let baseline = bonsai::lean::LeanBaseline::from_report(&r);
        std::fs::write(
            root.join(LEAN_BASELINE),
            serde_json::to_string_pretty(&baseline)?,
        )
        .with_context(|| format!("writing {LEAN_BASELINE}"))?;
        println!("\nsaved ratchet baseline → {LEAN_BASELINE}");
    }
    Ok(())
}

fn cmd_check(root: &Path) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    let mut errs = tree.check();

    // leanness ratchet: only enforced if a baseline is committed (opt-in per repo)
    let baseline_path = root.join(LEAN_BASELINE);
    if baseline_path.exists() {
        let baseline: bonsai::lean::LeanBaseline =
            serde_json::from_str(&std::fs::read_to_string(&baseline_path)?)
                .with_context(|| format!("parsing {LEAN_BASELINE}"))?;
        let mut report = bonsai::lean::analyze(&tree, root);
        report.dead_code = bonsai::lean::dead_code_count(root);
        report.misplaced = bonsai::lean::misplacement_count(root);
        for reg in baseline.regressions(&report) {
            errs.push(format!("leanness ratchet: {reg}"));
        }
    }

    if errs.is_empty() {
        println!(
            "bonsai check: OK — {} nodes, invariants + ratchet hold",
            tree.nodes.len()
        );
        Ok(())
    } else {
        for e in &errs {
            println!("  ✗ {e}");
        }
        anyhow::bail!("bonsai check: {} violation(s)", errs.len());
    }
}

/// Load `bonsai.toml` if present, else auto-derive the model from the filesystem. The
/// "automatic configuration" the user gets for free; the file, once written, is the
/// "manual configuration for putting anything anywhere."
fn load_or_derive(root: &Path) -> Result<Config> {
    let cfg_path = root.join("bonsai.toml");
    if cfg_path.exists() {
        let text = std::fs::read_to_string(&cfg_path)
            .with_context(|| format!("reading {}", cfg_path.display()))?;
        Config::from_toml(&text).with_context(|| format!("parsing {}", cfg_path.display()))
    } else {
        Config::from_dir(root).context("auto-deriving config from the filesystem")
    }
}

fn cmd_init(root: &Path, force: bool) -> Result<()> {
    let cfg_path = root.join("bonsai.toml");
    if cfg_path.exists() && !force {
        anyhow::bail!(
            "{} already exists (pass --force to overwrite)",
            cfg_path.display()
        );
    }
    let cfg = Config::from_dir(root).context("deriving structure")?;
    let n = cfg.nodes.len();
    std::fs::write(&cfg_path, cfg.to_toml()?)
        .with_context(|| format!("writing {}", cfg_path.display()))?;
    println!(
        "bonsai init: wrote {} with {n} directory node(s)",
        cfg_path.display()
    );
    println!("edit it to reparent anything (a node's `parent` is its authored home).");
    Ok(())
}

/// The reference-safe move: scan the workspace, plan the rewrites, print the dry-run diff.
/// With `write`, apply the edits + the file move (the ONLY mutating path in Bonsai).
fn cmd_move(root: &Path, from: &Path, to: &Path, write: bool) -> Result<()> {
    let ws = Workspace::from_dir(root)
        .with_context(|| format!("scanning workspace at {}", root.display()))?;

    // Paths are stored repo-relative; accept either form on the CLI.
    let rel = |p: &Path| -> PathBuf { p.strip_prefix(root).unwrap_or(p).to_path_buf() };
    let mv = Move {
        from: rel(from),
        to: rel(to),
    };

    if !ws.files.contains_key(&mv.from) && from.extension().and_then(|e| e.to_str()) == Some("rs") {
        anyhow::bail!(
            "{} is not a scanned .rs file under {} (nothing to move)",
            mv.from.display(),
            root.display()
        );
    }

    let plan = plan_move(&ws, &mv);
    print!("{}", plan.render_diff());
    println!(
        "\n{} edit(s) across {} file(s), {} warning(s)",
        plan.edits.len(),
        plan.touched_files(),
        plan.warnings.len()
    );

    if !write {
        println!("(dry-run — pass `--write` to apply)");
        return Ok(());
    }
    apply_plan(root, &plan)?;
    println!("applied.");
    Ok(())
}

/// Apply a plan to disk: rewrite references line-by-line, then move the file. Edits are
/// applied per file from the bottom up so earlier line numbers stay valid.
fn apply_plan(root: &Path, plan: &MovePlan) -> Result<()> {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<PathBuf, Vec<&Edit>> = BTreeMap::new();
    for e in &plan.edits {
        by_file.entry(e.path.clone()).or_default().push(e);
    }
    for (path, edits) in by_file {
        let abs = root.join(&path);
        let text =
            std::fs::read_to_string(&abs).with_context(|| format!("reading {}", abs.display()))?;
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        // apply deletes/replaces high-line-first; collect inserts to splice after.
        let mut sorted = edits.clone();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.line));
        for e in &sorted {
            match e.kind {
                EditKind::Replace => lines[e.line - 1] = e.after.clone(),
                EditKind::Delete => {
                    lines.remove(e.line - 1);
                }
                EditKind::Insert => lines.insert(e.line, e.after.clone()),
            }
        }
        let mut out = lines.join("\n");
        if text.ends_with('\n') {
            out.push('\n');
        }
        std::fs::write(&abs, out).with_context(|| format!("writing {}", abs.display()))?;
    }
    if let Some(mv) = &plan.mv {
        let (src, dst) = (root.join(&mv.from), root.join(&mv.to));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::rename(&src, &dst)
            .with_context(|| format!("moving {} → {}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// Render the capability tree from the model. Composites show a subtree file count (a
/// catamorphism); file atoms appear only with `--files`. This is the v1 "nice tree."
fn cmd_tree(root: &Path, show_files: bool, depth: Option<usize>) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    let counts = file_counts(&tree);
    let root_id = tree.root.clone();
    let name = leaf_name(&root_id);
    println!(
        "▸ {name}  ({} files)",
        counts.get(&root_id).copied().unwrap_or(0)
    );
    render_children(&tree, &root_id, "", show_files, depth, 1, &counts);
    Ok(())
}

/// Recursively render a node's children with box-drawing connectors.
fn render_children(
    tree: &Tree,
    id: &NodeId,
    prefix: &str,
    show_files: bool,
    max_depth: Option<usize>,
    depth: usize,
    counts: &BTreeMap<NodeId, usize>,
) {
    let Some(node) = tree.nodes.get(id) else {
        return;
    };
    let kids: Vec<&NodeId> = node
        .children
        .iter()
        .filter(|c| {
            let is_atom = tree
                .nodes
                .get(*c)
                .map(|n| n.kind == Kind::Atom)
                .unwrap_or(false);
            show_files || !is_atom
        })
        .collect();
    for (i, cid) in kids.iter().enumerate() {
        let last = i + 1 == kids.len();
        let (branch, cont) = if last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        let child = &tree.nodes[*cid];
        let nm = leaf_name(cid);
        match child.kind {
            Kind::Composite => {
                let count = counts.get(*cid).copied().unwrap_or(0);
                let facet = facet_tag(child);
                let collapsed = max_depth.map(|m| depth >= m).unwrap_or(false);
                let hint = if collapsed && count > 0 { " …" } else { "" };
                println!("{prefix}{branch}▸ {nm}  ({count} files){facet}{hint}");
                if !collapsed {
                    render_children(
                        tree,
                        cid,
                        &format!("{prefix}{cont}"),
                        show_files,
                        max_depth,
                        depth + 1,
                        counts,
                    );
                }
            }
            Kind::Atom => {
                println!("{prefix}{branch}{nm}{}", facet_tag(child));
            }
        }
    }
}

/// The catamorphism made concrete: file count of each subtree (atoms → 1, composites → Σ).
fn file_counts(tree: &Tree) -> BTreeMap<NodeId, usize> {
    tree.fold(&|node, kids: &[usize]| match node.kind {
        Kind::Atom => 1,
        Kind::Composite => kids.iter().sum(),
    })
}

fn leaf_name(id: &str) -> &str {
    id.rsplit_once('/').map(|(_, n)| n).unwrap_or(id)
}

fn facet_tag(node: &bonsai::model::Node) -> String {
    let mut bits = Vec::new();
    if node.plane == Plane::Docs {
        bits.push("docs".to_string());
    }
    if let Some(lang) = node.facets.get("language") {
        bits.push(lang.clone());
    }
    if bits.is_empty() {
        String::new()
    } else {
        format!("  [{}]", bits.join(" "))
    }
}

/// `bonsai stat`: rank subtrees by a relative complexity score (a catamorphism over LOC +
/// fan-out). Deterministic, relative — no dev-hours. Surfaces where complexity concentrates.
fn cmd_stat(root: &Path, top: usize) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    let counts = file_counts(&tree);

    // per-atom raw size (LOC) for anchored files; composites roll up.
    let loc = atom_loc(&tree, root);
    let scores = tree.fold(&|node, kids: &[f64]| match node.kind {
        Kind::Atom => *loc.get(&node.id).unwrap_or(&0) as f64,
        // subtree complexity = children's mass, mildly penalized by branching breadth
        // (a wide composite is more to hold in your head than a deep-but-narrow one).
        Kind::Composite => {
            let mass: f64 = kids.iter().sum();
            let breadth = (node.children.len() as f64).max(1.0);
            mass * (1.0 + breadth.ln() / 10.0)
        }
    });

    let total = scores.get(&tree.root).copied().unwrap_or(0.0).max(1.0);
    let mut ranked: Vec<(&NodeId, f64)> = scores
        .iter()
        .filter(|(id, _)| **id != tree.root && tree.nodes[*id].kind == Kind::Composite)
        .map(|(id, s)| (id, *s))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!(
        "bonsai stat — {} nodes, {} files\n",
        tree.nodes.len(),
        counts[&tree.root]
    );
    println!("  {:>6}  {:>6}  {:>5}   subtree", "cplx%", "files", "loc");
    for (id, score) in ranked.into_iter().take(top) {
        let pct = 100.0 * score / total;
        let files = counts.get(id).copied().unwrap_or(0);
        let subtree_loc: usize = descendant_atoms(&tree, id)
            .iter()
            .map(|a| loc.get(a).copied().unwrap_or(0))
            .sum();
        println!("  {pct:>5.1}%  {files:>6}  {subtree_loc:>5}   {id}");
    }
    Ok(())
}

/// LOC (line count) for each anchored file atom, read from disk.
fn atom_loc(tree: &Tree, root: &Path) -> BTreeMap<NodeId, usize> {
    let mut out = BTreeMap::new();
    for node in tree.nodes.values() {
        if node.kind != Kind::Atom {
            continue;
        }
        if let Some(anchor) = node.anchors.first() {
            if let Ok(text) = std::fs::read_to_string(root.join(anchor)) {
                out.insert(node.id.clone(), text.lines().count());
            }
        }
    }
    out
}

/// All atom ids under `id` (for subtree LOC totals).
fn descendant_atoms(tree: &Tree, id: &NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![id.clone()];
    while let Some(cur) = stack.pop() {
        if let Some(n) = tree.nodes.get(&cur) {
            if n.kind == Kind::Atom {
                out.push(cur.clone());
            } else {
                stack.extend(n.children.iter().cloned());
            }
        }
    }
    out
}
