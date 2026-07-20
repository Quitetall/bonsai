//! Bonsai CLI (ADR 0134) — `cargo fmt` for your directory structure, plus the unified
//! docs+code capability tree. Phase-0 scaffold: `tree` is real; the reference-safe move
//! engine (`diff`/`apply`) is the next, hardest-part-first build.

use anyhow::{Context, Result};
use bonsai::blueprint::{
    Blueprint, BlueprintLock, Lifecycle, ScaffoldLanguage, ShapeDiff, WitnessResult,
};
use bonsai::config::Config;
use bonsai::facts::{facts_from_blueprint, facts_from_scip, BqlQuery, FactStore, SqliteFactStore};
use bonsai::finding::{Finding, Location, Report, Severity};
use bonsai::model::{Kind, NodeId, Plane};
use bonsai::moves::{plan_move, Edit, Move, MovePlan, Workspace};
use bonsai::tree::Tree;
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Output format for the finding stream (`lean`/`check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// The rich, human-readable per-command report (default).
    Text,
    /// A flat JSON list of findings.
    Json,
    /// SARIF 2.1.0 for CI code scanning.
    Sarif,
}

/// What `bonsai hook` should do to the automatic pre-commit gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum HookAction {
    /// Write a git pre-commit hook that runs `bonsai check` (default).
    Install,
    /// Report whether the hook is installed.
    Status,
    /// Remove the bonsai-managed hook.
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BlueprintLanguage {
    Rust,
    Python,
    C,
    TypeScript,
}

impl From<BlueprintLanguage> for ScaffoldLanguage {
    fn from(value: BlueprintLanguage) -> Self {
        match value {
            BlueprintLanguage::Rust => Self::Rust,
            BlueprintLanguage::Python => Self::Python,
            BlueprintLanguage::C => Self::C,
            BlueprintLanguage::TypeScript => Self::TypeScript,
        }
    }
}

#[derive(Subcommand)]
enum BlueprintAction {
    /// Parse and validate a declarative architecture shape.
    Validate { blueprint: PathBuf },
    /// Classify a change as implementation-only or a new logical shape.
    Diff {
        old: PathBuf,
        new: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Execute all declared contract witnesses and save their results.
    Verify {
        blueprint: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = "bonsai.witnesses.json")]
        out: PathBuf,
    },
    /// Freeze a locked, verified architecture shape.
    Lock {
        blueprint: PathBuf,
        #[arg(long)]
        results: PathBuf,
        #[arg(long, default_value = "bonsai.blueprint.lock.json")]
        out: PathBuf,
    },
    /// Fail when a blueprint no longer matches its lock.
    Check {
        blueprint: PathBuf,
        #[arg(long)]
        lock: PathBuf,
    },
    /// Generate non-overwriting contract stubs for each logical slot.
    Scaffold {
        blueprint: PathBuf,
        #[arg(long, value_enum)]
        language: BlueprintLanguage,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum GraphAction {
    /// Create an immutable fact snapshot from one or more architecture blueprints.
    Snapshot {
        #[arg(long, default_value = ".bonsai/facts.db")]
        db: PathBuf,
        #[arg(long)]
        blueprint: Vec<PathBuf>,
        /// SCIP compiler index to normalize into symbol and file dependency facts (repeatable).
        #[arg(long)]
        scip: Vec<PathBuf>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "ref", default_value = "main")]
        reference: String,
    },
    /// Run a bounded Bonsai Query Language dependency or impact query.
    Query {
        #[arg(long, default_value = ".bonsai/facts.db")]
        db: PathBuf,
        #[arg(long, default_value = "main")]
        snapshot: String,
        #[arg(long)]
        bql: String,
    },
    /// Export all typed facts in a snapshot as JSON.
    Export {
        #[arg(long, default_value = ".bonsai/facts.db")]
        db: PathBuf,
        #[arg(long, default_value = "main")]
        snapshot: String,
    },
    /// Execute a read-only GraphQL query over the deterministic fact graph.
    Graphql {
        #[arg(long, default_value = ".bonsai/facts.db")]
        db: PathBuf,
        #[arg(long)]
        query: String,
    },
}

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
    /// Author, prove, lock, and scaffold declarative architecture shapes.
    Blueprint {
        #[command(subcommand)]
        action: BlueprintAction,
    },
    /// Build and query immutable, provenance-bearing repository fact snapshots.
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },
    /// Generate `bonsai.toml` by auto-deriving the directory capability tree.
    Init {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Overwrite an existing bonsai.toml.
        #[arg(long)]
        force: bool,
        /// Extra directory names to skip (repeatable) — vendored/scratch/sequestered trees.
        #[arg(long = "skip")]
        skip: Vec<String>,
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
        /// Disable the incremental scan cache (.bonsai/scan-cache.json).
        #[arg(long)]
        no_cache: bool,
        /// Output format: text (default), json, or sarif (CI code scanning).
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
    },
    /// Fail-closed gate: tree invariants + leanness ratchet (CI / pre-commit).
    Check {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Disable the incremental scan cache (.bonsai/scan-cache.json).
        #[arg(long)]
        no_cache: bool,
        /// Output format: text (default), json, or sarif (CI code scanning).
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// Scope file-level findings to git-staged files (the addition being committed). Repo-wide
        /// gates (ratchet, invariants, abstraction bounds) still apply. Ideal for the pre-commit hook.
        #[arg(long)]
        staged: bool,
        /// Scope file-level findings to files changed since <ref> (e.g. `origin/main`). Combines
        /// with `--staged`.
        #[arg(long)]
        since: Option<String>,
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
    /// Placement oracle: where does `<file>` belong? Ranks the directories it couples to (SCIP
    /// coupling for an indexed file; `crate::` import-scan for a new one) and, if it's misplaced,
    /// shows the reference-safe move to its home. The "Bonsai knows where new code goes" query.
    Place {
        /// The file to place (repo-relative or absolute; may be a new, not-yet-indexed file).
        file: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
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
    /// Install the automatic pre-commit gate: after this, every commit runs `bonsai check`
    /// (placement, layering, contracts, ratchet) with zero friction — devs never invoke it.
    Hook {
        #[arg(value_enum, default_value = "install")]
        action: HookAction,
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Blueprint { action } => cmd_blueprint(action),
        Cmd::Graph { action } => cmd_graph(action),
        Cmd::Init { root, force, skip } => cmd_init(&root, force, &skip),
        Cmd::Tree { root, files, depth } => cmd_tree(&root, files, depth),
        Cmd::Stat { root, top } => cmd_stat(&root, top),
        Cmd::Diff { from, to, root } => cmd_move(&root, &from, &to, false),
        Cmd::Apply {
            from,
            to,
            root,
            write,
        } => cmd_move(&root, &from, &to, write),
        Cmd::Lean {
            root,
            save,
            no_cache,
            format,
        } => cmd_lean(&root, save, !no_cache, format),
        Cmd::Check {
            root,
            no_cache,
            format,
            staged,
            since,
        } => cmd_check(&root, !no_cache, format, staged, since.as_deref()),
        Cmd::Docs { root } => cmd_docs(&root),
        Cmd::Index {
            root,
            scip,
            generate,
        } => cmd_index(&root, scip.as_deref(), generate),
        Cmd::Ffi { root } => cmd_ffi(&root),
        Cmd::Tidy { root, write } => cmd_tidy(&root, write),
        Cmd::Place { file, root } => cmd_place(&root, &file),
        Cmd::Hook { action, root } => cmd_hook(&root, action),
    }
}

fn load_blueprint(path: &Path) -> Result<Blueprint> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading blueprint {}", path.display()))?;
    Blueprint::from_toml(&text).with_context(|| format!("parsing blueprint {}", path.display()))
}

fn require_valid(blueprint: &Blueprint) -> Result<()> {
    let errors = blueprint.validate();
    anyhow::ensure!(
        errors.is_empty(),
        "invalid blueprint:\n- {}",
        errors.join("\n- ")
    );
    Ok(())
}

fn cmd_blueprint(action: BlueprintAction) -> Result<()> {
    match action {
        BlueprintAction::Validate { blueprint } => {
            let value = load_blueprint(&blueprint)?;
            require_valid(&value)?;
            println!("{} {}", value.meta.id, value.shape_digest());
        }
        BlueprintAction::Diff { old, new, json } => {
            let old = load_blueprint(&old)?;
            let new = load_blueprint(&new)?;
            require_valid(&old)?;
            require_valid(&new)?;
            let diff = ShapeDiff::between(&old, &new);
            if json {
                println!("{}", serde_json::to_string_pretty(&diff)?);
            } else {
                println!(
                    "{:?}: {} -> {}",
                    diff.class, diff.old_digest, diff.new_digest
                );
            }
        }
        BlueprintAction::Verify {
            blueprint,
            root,
            out,
        } => {
            let value = load_blueprint(&blueprint)?;
            require_valid(&value)?;
            let results = value.run_witnesses(&root);
            std::fs::write(&out, serde_json::to_string_pretty(&results)? + "\n")
                .with_context(|| format!("writing witness results {}", out.display()))?;
            for result in &results {
                println!(
                    "{} {}{}",
                    if result.passed { "PASS" } else { "FAIL" },
                    result.id,
                    if result.detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", result.detail)
                    }
                );
            }
            anyhow::ensure!(
                results.iter().all(|result| result.passed),
                "witnesses failed"
            );
        }
        BlueprintAction::Lock {
            blueprint,
            results,
            out,
        } => {
            let value = load_blueprint(&blueprint)?;
            let results: Vec<WitnessResult> = serde_json::from_str(
                &std::fs::read_to_string(&results)
                    .with_context(|| format!("reading witness results {}", results.display()))?,
            )?;
            let lock = BlueprintLock::create(&value, &results)?;
            std::fs::write(&out, lock.to_json()?)
                .with_context(|| format!("writing blueprint lock {}", out.display()))?;
            println!("locked {} at {}", lock.blueprint_id, lock.shape_digest);
        }
        BlueprintAction::Check { blueprint, lock } => {
            let value = load_blueprint(&blueprint)?;
            let lock = BlueprintLock::from_json(
                &std::fs::read_to_string(&lock)
                    .with_context(|| format!("reading blueprint lock {}", lock.display()))?,
            )?;
            let errors = lock.check(&value);
            anyhow::ensure!(
                errors.is_empty(),
                "blueprint lock failed:\n- {}",
                errors.join("\n- ")
            );
            println!("{}: locked shape intact", value.meta.id);
        }
        BlueprintAction::Scaffold {
            blueprint,
            language,
            out,
        } => {
            let value = load_blueprint(&blueprint)?;
            require_valid(&value)?;
            let files = value.scaffold_files(language.into());
            let existing: Vec<_> = files
                .iter()
                .map(|file| out.join(&file.relative_path))
                .filter(|path| path.exists())
                .collect();
            anyhow::ensure!(
                existing.is_empty(),
                "refusing to overwrite scaffold files: {}",
                existing
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            std::fs::create_dir_all(&out)
                .with_context(|| format!("creating scaffold directory {}", out.display()))?;
            for file in files {
                let path = out.join(file.relative_path);
                std::fs::write(&path, file.content)
                    .with_context(|| format!("writing scaffold {}", path.display()))?;
                println!("created {}", path.display());
            }
        }
    }
    Ok(())
}

fn open_fact_store(path: &Path) -> Result<SqliteFactStore> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating fact database directory {}", parent.display()))?;
    }
    SqliteFactStore::open(path).with_context(|| format!("opening fact database {}", path.display()))
}

fn cmd_graph(action: GraphAction) -> Result<()> {
    match action {
        GraphAction::Snapshot {
            db,
            blueprint,
            scip,
            parent,
            reference,
        } => {
            anyhow::ensure!(
                !blueprint.is_empty() || !scip.is_empty(),
                "graph snapshot needs at least one --blueprint or --scip input"
            );
            let store = open_fact_store(&db)?;
            let parent_id = parent
                .as_deref()
                .map(|value| store.resolve_snapshot(value))
                .transpose()?;
            let mut facts = Vec::new();
            for path in blueprint {
                let value = load_blueprint(&path)?;
                require_valid(&value)?;
                facts.extend(facts_from_blueprint(&value, &path.to_string_lossy()));
            }
            for path in scip {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading SCIP index {}", path.display()))?;
                let digest = format!("b3:{}", blake3::hash(&bytes).to_hex());
                let graph = bonsai::scip::CodeGraph::from_file(&path)
                    .with_context(|| format!("parsing SCIP index {}", path.display()))?;
                facts.extend(facts_from_scip(&graph, &path.to_string_lossy(), &digest));
            }
            let snapshot = store.create_snapshot(parent_id.as_deref(), &facts)?;
            store.set_ref(&reference, &snapshot.id)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        GraphAction::Query { db, snapshot, bql } => {
            let store = open_fact_store(&db)?;
            let snapshot = store.resolve_snapshot(&snapshot)?;
            let query = BqlQuery::parse(&bql)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.query(&snapshot, &query)?)?
            );
        }
        GraphAction::Export { db, snapshot } => {
            let store = open_fact_store(&db)?;
            let snapshot = store.resolve_snapshot(&snapshot)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.facts(&snapshot)?)?
            );
        }
        GraphAction::Graphql { db, query } => {
            let store = open_fact_store(&db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bonsai::graphql::execute_query(store, &query))?
            );
        }
    }
    Ok(())
}

const HOOK_MARK: &str = "# bonsai-managed pre-commit hook";

/// Install / inspect / remove the automatic pre-commit gate — the friction-free enforcement
/// layer. Once installed, `bonsai check` runs on every commit, so a non-conformant addition
/// (misplaced, upward-coupled, contract-breaking, or leanness-regressing) is blocked before it
/// lands, without any dev remembering to run a command.
fn cmd_hook(root: &Path, action: HookAction) -> Result<()> {
    let hook = git_hooks_dir(root)?.join("pre-commit");
    let is_ours = |p: &Path| {
        std::fs::read_to_string(p)
            .map(|s| s.contains(HOOK_MARK))
            .unwrap_or(false)
    };
    match action {
        HookAction::Install => {
            // the bonsai-managed block, appended verbatim (idempotent via HOOK_MARK). Guarded by
            // `command -v bonsai` so a contributor without bonsai installed isn't blocked — they're
            // covered by CI; the local hook is the fast guard for those who have it.
            let block = format!(
                "\n{HOOK_MARK}\n# Blocks a commit that regresses structure, leanness, or a level contract.\n# --staged scopes file-level findings to the addition; repo-wide gates still apply.\nif command -v bonsai >/dev/null 2>&1; then\n    bonsai check --staged --root \"$(git rev-parse --show-toplevel)\" || exit 1\nfi\n"
            );
            if hook.exists() && is_ours(&hook) {
                println!("bonsai hook: already installed at {}", hook.display());
            } else if hook.exists() {
                // COMPOSE into an existing (non-bonsai) hook — don't clobber the contributor's.
                let mut existing = std::fs::read_to_string(&hook).unwrap_or_default();
                if !existing.ends_with('\n') {
                    existing.push('\n');
                }
                std::fs::write(&hook, format!("{existing}{block}"))
                    .with_context(|| format!("appending to {}", hook.display()))?;
                set_executable(&hook)?;
                println!(
                    "bonsai hook: appended the gate to your existing {}",
                    hook.display()
                );
            } else {
                std::fs::write(&hook, format!("#!/bin/sh{block}"))
                    .with_context(|| format!("writing {}", hook.display()))?;
                set_executable(&hook)?;
                println!("bonsai hook: installed → {}", hook.display());
            }
            println!("  every commit now runs `bonsai check --staged` (placement · layering · contracts · ratchet).");
        }
        HookAction::Status => {
            if hook.exists() && is_ours(&hook) {
                println!("bonsai hook: INSTALLED at {}", hook.display());
            } else if hook.exists() {
                println!(
                    "bonsai hook: a NON-bonsai pre-commit hook is present at {}",
                    hook.display()
                );
            } else {
                println!("bonsai hook: not installed — run `bonsai hook install`");
            }
        }
        HookAction::Uninstall => {
            if !hook.exists() || !is_ours(&hook) {
                println!("bonsai hook: nothing to remove (no bonsai-managed hook).");
            } else {
                let text = std::fs::read_to_string(&hook).unwrap_or_default();
                if let Some(pos) = text.find(HOOK_MARK) {
                    let head = text[..pos].trim_end();
                    if head.is_empty() || head == "#!/bin/sh" {
                        std::fs::remove_file(&hook)?; // the file was entirely ours
                        println!("bonsai hook: removed {}", hook.display());
                    } else {
                        // strip only our appended block, keep the contributor's hook
                        std::fs::write(&hook, format!("{head}\n"))?;
                        println!(
                            "bonsai hook: removed the bonsai block from {}",
                            hook.display()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolve the git hooks directory for `root` (handles submodules + worktrees, where `.git` is a
/// file pointing elsewhere) via `git rev-parse --git-path hooks`.
fn git_hooks_dir(root: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .context("running `git rev-parse --git-path hooks` (is git installed?)")?;
    if !out.status.success() {
        anyhow::bail!("not a git repository at {}", root.display());
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).ok();
    Ok(dir)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod +x {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(()) // git on Windows treats the hook as executable by content
}

/// The telos capstone: turn leanness signals into ready-to-apply refactors. Today it drives
/// the **misplacement** fold → a **reference-safe move** (the novel engine) for each file
/// used only from one other directory. Propose-by-default; `--write` executes (the structural
/// half of refactoring, done for you). Sequester-of-dead-code is the next tidy action.
fn cmd_tidy(root: &Path, write: bool) -> Result<()> {
    let indices = bonsai::scip::discover_indices(root);
    if indices.is_empty() {
        anyhow::bail!(
            "no *.scip index under {} — run `bonsai index --generate` first",
            root.display()
        );
    }
    let g = bonsai::scip::CodeGraph::from_files(&indices)
        .with_context(|| format!("parsing {} SCIP index(es)", indices.len()))?;
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
        let mv = Move {
            from: PathBuf::from(&m.file),
            to: PathBuf::from(&to),
        };
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

/// The placement oracle: answer "where does `<file>` belong?" from its coupling. Prefers the
/// exact SCIP graph signal; falls back to a `crate::`-import scan for a new, unindexed file.
fn cmd_place(root: &Path, file: &Path) -> Result<()> {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    // 1) strong signal: the SCIP coupling graph (works for an indexed file)
    if let Some(g) = load_graph_opt(root) {
        if let Some(p) = bonsai::place::place_existing(&g, &rel_str) {
            println!("bonsai place — {rel_str}");
            print_placement(&p);
            if !p.well_placed() {
                if let Some(home) = p.recommended() {
                    let base = rel_str.rsplit('/').next().unwrap_or(&rel_str);
                    let to = if home.is_empty() {
                        base.to_string()
                    } else {
                        format!("{home}/{base}")
                    };
                    show_move_hint(root, &rel_str, &to)?;
                }
            }
            return Ok(());
        }
    }

    // 2) fallback: import-scan a new / not-yet-indexed file
    let abs = root.join(rel);
    let content = std::fs::read_to_string(&abs).with_context(|| {
        format!(
            "{} is not in a SCIP index and not readable — index the repo (`bonsai index --generate`) or pass a real file",
            abs.display()
        )
    })?;
    let rs: Vec<String> = bonsai::walk::files_with_ext(root, &[], &["rs"])
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let map = bonsai::place::module_dir_map(&rs);
    let cur = rel_str.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    match bonsai::place::place_new(&content, cur, &map) {
        Some(p) => {
            println!("bonsai place — {rel_str}  (new/unindexed: inferred from `crate::` imports)");
            print_placement(&p);
        }
        None => println!(
            "bonsai place — {rel_str}: no `crate::` coupling found. Place by intent, or run \
             `bonsai index --generate` for the exact signal."
        ),
    }
    Ok(())
}

/// Render a placement recommendation (signal source, current dir, home, coupling breakdown).
fn print_placement(p: &bonsai::place::Placement) {
    let show = |d: &str| {
        if d.is_empty() {
            ".".to_string()
        } else {
            format!("{d}/")
        }
    };
    println!(
        "  signal: {}",
        if p.from_graph {
            "SCIP coupling (exact)"
        } else {
            "crate:: import scan (heuristic)"
        }
    );
    if !p.file.is_empty() {
        println!("  currently in: {}", show(&p.current_dir));
    }
    if let Some(home) = p.recommended() {
        if p.well_placed() {
            println!("  ✓ already in its coupled home: {}", show(home));
        } else {
            println!("  → belongs in: {}", show(home));
        }
    }
    println!("  coupling by directory:");
    for (dir, n) in p.ranked.iter().take(6) {
        println!("    {n:>3}×  {}", show(dir));
    }
}

/// Print the one-line reference-safe move a misplaced file needs to reach its home.
fn show_move_hint(root: &Path, from: &str, to: &str) -> Result<()> {
    let ws = Workspace::from_dir(root)?;
    let mv = Move {
        from: PathBuf::from(from),
        to: PathBuf::from(to),
    };
    if ws.files.contains_key(&mv.from) {
        let plan = plan_move(&ws, &mv);
        println!(
            "\n  reference-safe move to home:\n    bonsai apply {from} {to} --write   ({} edit(s), {} warning(s))",
            plan.edits.len(),
            plan.warnings.len()
        );
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

    // An explicit --scip pins one index; otherwise merge every *.scip under the root (polyglot).
    let indices: Vec<PathBuf> = match scip {
        Some(p) => vec![p.to_path_buf()],
        None => bonsai::scip::discover_indices(root),
    };
    if indices.is_empty() {
        anyhow::bail!(
            "no *.scip index under {} — run with --generate, or point --scip at an index",
            root.display()
        );
    }
    println!(
        "bonsai index: merging {} index(es) [{}]",
        indices.len(),
        indices
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let g = bonsai::scip::CodeGraph::from_files(&indices)
        .with_context(|| format!("parsing {} SCIP index(es)", indices.len()))?;

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
        let name = g
            .symbols
            .get(sym)
            .map(|s| s.display.as_str())
            .filter(|s| !s.is_empty());
        println!("    {n:>3}×  {}", name.unwrap_or(sym));
    }

    // dead code: unreachable from the public API + main (all kinds, for inspection)
    let dead = g.dead_symbols(root, &[]);
    println!("  dead (unreachable from pub API + main): {}", dead.len());
    for s in dead.iter().take(20) {
        let name = if s.display.is_empty() {
            "?"
        } else {
            &s.display
        };
        println!(
            "    kind {:>2}  {}:{}  {name}",
            s.kind,
            s.file,
            s.def.sl + 1
        );
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

/// Load + merge every `*.scip` index at `root` (for dead-code + misplacement findings across a
/// polyglot repo); `None` when no index exists — those signals stay unmeasured, never faked.
fn load_graph_opt(root: &Path) -> Option<bonsai::scip::CodeGraph> {
    let indices = bonsai::scip::discover_indices(root);
    if indices.is_empty() {
        return None;
    }
    bonsai::scip::CodeGraph::from_files(&indices).ok()
}

/// Turn a leanness report (+ near-dups + optional code graph) into the uniform finding stream.
/// This is the single mapping from Bonsai's signals to typed [`Finding`]s for JSON/SARIF.
fn lean_findings(
    root: &Path,
    r: &bonsai::lean::LeanReport,
    near: &[bonsai::lean::NearDup],
    graph: Option<&bonsai::scip::CodeGraph>,
    ws: Option<&Workspace>,
    clones: &[bonsai::clones::CloneGroup],
) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(ws) = ws {
        out.extend(bonsai::forgotten::evaluate(ws)); // unwired + undocumented ("nothing forgotten")
    }
    for g in clones {
        let (file, name, line) = &g.sites[0];
        let others: Vec<Location> = g.sites[1..]
            .iter()
            .map(|(f, _, l)| Location::at(f.clone(), *l))
            .collect();
        out.push(
            Finding::new(
                "clone-function",
                Severity::Warning,
                format!(
                    "`{name}` ({} lines) is copy-pasted across {} sites",
                    g.lines,
                    g.sites.len()
                ),
                Location::at(file.clone(), *line),
            )
            .with_related(others)
            .with_fix("extract the shared function to one home and call it from each site"),
        );
    }
    for g in &r.dup_groups {
        let related = g.files[1..].iter().map(Location::file).collect();
        out.push(
            Finding::new(
                "duplicate",
                Severity::Warning,
                format!(
                    "{} byte-identical copies ({} lines each)",
                    g.files.len(),
                    g.loc
                ),
                Location::file(g.files[0].clone()),
            )
            .with_related(related)
            .with_fix("keep one copy; replace the rest with a reference/import"),
        );
    }
    for nd in near {
        out.push(
            Finding::new(
                "near-duplicate",
                Severity::Info,
                format!("{:.0}% similar to {}", nd.similarity * 100.0, nd.b),
                Location::file(nd.a.clone()),
            )
            .with_related(vec![Location::file(nd.b.clone())])
            .with_fix("reconcile the copies or extract the shared part"),
        );
    }
    for d in &r.empty_dirs {
        out.push(
            Finding::new(
                "empty-dir",
                Severity::Info,
                "empty directory (no tracked files)",
                Location::file(d.clone()),
            )
            .with_fix("remove, or populate"),
        );
    }
    if let Some(g) = graph {
        for s in g.dead_symbols(root, &[]) {
            let name = if s.display.is_empty() {
                "<symbol>"
            } else {
                &s.display
            };
            out.push(
                Finding::new(
                    "dead-code",
                    Severity::Warning,
                    format!("`{name}` unreachable from pub API / main / tests"),
                    Location::at(s.file.clone(), (s.def.sl + 1).max(1) as u32),
                )
                .with_fix("sequester (dead code is moved to deprecated/, never deleted)"),
            );
        }
        for scc in g.dependency_cycles() {
            let head = scc.first().cloned().unwrap_or_default();
            out.push(
                Finding::new(
                    "structure-cycle",
                    Severity::Warning,
                    format!("dependency cycle among {} modules: {}", scc.len(), scc.join(" → ")),
                    Location::file(head),
                )
                .with_fix("break the cycle: extract the shared piece to a lower layer, or invert one edge"),
            );
        }
        for m in g.misplacements() {
            out.push(
                Finding::new(
                    "misplaced",
                    Severity::Info,
                    format!(
                        "used only from {}/ ({} dependents)",
                        m.suggested_dir, m.dependents
                    ),
                    Location::file(m.file.clone()),
                )
                .with_fix(format!(
                    "bonsai tidy --write → reference-safe move into {}/",
                    m.suggested_dir
                )),
            );
        }
    }
    out
}

fn cmd_lean(root: &Path, save: bool, cache: bool, format: Format) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    // one parallel content pass feeds both the exact-dup report and near-duplicates.
    // (near-dup needs shingles, which need a read, so the cache warms but doesn't skip reads
    // here; it still refreshes so the next `check` is fast.)
    let mut scfg = bonsai::scan::ScanConfig::with_shingles().skipping(&cfg.bonsai.skip);
    if cache {
        scfg = scfg.with_cache(bonsai::scan::ScanConfig::default_cache_path(root));
    }
    let scan = bonsai::scan::Scan::run(root, &scfg);
    let mut r = bonsai::lean::analyze_from_scan(&tree, &scan);
    let near = bonsai::lean::near_duplicates_from_scan(&scan, 0.85);
    // measure SCIP-backed signals if an index is present (moves them out of "deferred")
    let graph = load_graph_opt(root);
    if let Some(g) = &graph {
        r.dead_code = Some(g.dead_symbols(root, &[]).len());
        r.misplaced = Some(g.misplacements().len());
        r.deferred
            .retain(|d| !d.starts_with("dead-code") && !d.starts_with("misplacement"));
    }
    // the "forgotten" fold + function-clone detection read the sources, honoring the skip list
    // (so intentionally-redundant/kept trees don't pollute the report).
    let ws = Workspace::from_dir_ext_skip(root, &["rs"], &cfg.bonsai.skip).ok();
    let forgotten = ws
        .as_ref()
        .map(bonsai::forgotten::evaluate)
        .unwrap_or_default();
    // clone detection spans Rust + Python (the polyglot "duplicated" signal); its own workspace.
    let clones = Workspace::from_dir_ext_skip(root, &["rs", "py"], &cfg.bonsai.skip)
        .ok()
        .map(|w| bonsai::clones::function_clones(&w, 6))
        .unwrap_or_default();

    if format != Format::Text {
        let report = Report::new(lean_findings(
            root,
            &r,
            &near,
            graph.as_ref(),
            ws.as_ref(),
            &clones,
        ));
        match format {
            Format::Json => println!("{}", report.to_json()),
            Format::Sarif => println!("{}", report.to_sarif()),
            Format::Text => unreachable!(),
        }
    } else {
        println!(
            "bonsai lean — {} files, {} composites, depth {}",
            r.files, r.composites, r.max_depth
        );
        if scan.cache_hits > 0 || scan.over_cap > 0 || scan.unreadable > 0 {
            println!(
                "  scan           : {} cache hit(s), {} over-cap, {} unreadable",
                scan.cache_hits, scan.over_cap, scan.unreadable
            );
        }
        if let Some(n) = r.dead_code {
            println!("  dead symbols   : {n}  (SCIP-measured)");
        }
        if let Some(n) = r.misplaced {
            println!("  misplaced files: {n}  (SCIP-measured)");
        }
        if let Some(g) = &graph {
            let cyc = g.dependency_cycles();
            if !cyc.is_empty() {
                println!(
                    "  dependency cycles: {}  (mutually-entangled module groups)",
                    cyc.len()
                );
                for scc in cyc.iter().take(4) {
                    println!("    ⟲ {}", scc.join(" → "));
                }
            }
        }
        let unwired = forgotten
            .iter()
            .filter(|f| f.rule == "forgotten-unwired")
            .count();
        let undoc = forgotten
            .iter()
            .filter(|f| f.rule == "forgotten-undoc")
            .count();
        if unwired > 0 || undoc > 0 {
            println!(
                "  forgotten      : {unwired} unwired module(s), {undoc} undocumented pub item(s)"
            );
            for f in forgotten
                .iter()
                .filter(|f| f.rule == "forgotten-unwired")
                .take(6)
            {
                println!("    ⚠ {}", f.location.file);
            }
        }
        if !clones.is_empty() {
            println!(
                "  function clones: {} group(s) (copy-pasted logic)",
                clones.len()
            );
            for g in clones.iter().take(6) {
                let names: Vec<&str> = g.sites.iter().map(|(_, n, _)| n.as_str()).collect();
                println!(
                    "    ⧉ {} lines ×{}: {}",
                    g.lines,
                    g.sites.len(),
                    names.join(", ")
                );
            }
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
        if r.cross_repo_mirrors > 0 {
            println!(
                "  cross-repo mirrors: {} (identical across sub-repos — NOT counted as waste)",
                r.cross_repo_mirrors
            );
        }
        println!(
            "  near-duplicates: {} pair(s) (≥0.85 similar, not identical)",
            near.len()
        );
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

/// Rules that are repo-wide by nature — they bypass diff-scoping (a leanness regression or a
/// too-deep tree is a whole-repo fact, not attributable to one changed file).
const GLOBAL_RULES: &[&str] = &[
    "tree-invariant",
    "leanness-ratchet",
    "blueprint-lock",
    "abstraction-layers",
    "abstraction-depth",
];

/// The set of files git reports changed (`--staged` = index vs HEAD; `--since <ref>` = vs ref);
/// their union. `None` if no scope was requested; empty set if git failed (nothing scoped in).
fn changed_files(
    root: &Path,
    staged: bool,
    since: Option<&str>,
) -> Option<std::collections::BTreeSet<String>> {
    if !staged && since.is_none() {
        return None;
    }
    let mut set = std::collections::BTreeSet::new();
    let run = |args: &[&str]| -> Vec<String> {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    if staged {
        set.extend(run(&["diff", "--cached", "--name-only"]));
    }
    if let Some(r) = since {
        set.extend(run(&["diff", "--name-only", r]));
    }
    Some(set)
}

fn check_blueprint_locks(root: &Path) -> Vec<Finding> {
    let mut paths = Vec::new();
    let root_blueprint = root.join("bonsai.blueprint.toml");
    if root_blueprint.exists() {
        paths.push(root_blueprint);
    }
    let directory = root.join(".bonsai/blueprints");
    if let Ok(entries) = std::fs::read_dir(&directory) {
        paths.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("toml")),
        );
    }
    paths.sort();

    let mut findings = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let blueprint = match load_blueprint(&path) {
            Ok(value) => value,
            Err(error) => {
                findings.push(Finding::new(
                    "blueprint-lock",
                    Severity::Error,
                    error.to_string(),
                    Location::file(&relative),
                ));
                continue;
            }
        };
        let validation = blueprint.validate();
        for error in validation {
            findings.push(Finding::new(
                "blueprint-lock",
                Severity::Error,
                error,
                Location::file(&relative),
            ));
        }
        if blueprint.meta.lifecycle != Lifecycle::Locked {
            continue;
        }
        let lock_path = path.with_extension("lock.json");
        let lock = std::fs::read_to_string(&lock_path)
            .with_context(|| format!("reading required lock {}", lock_path.display()))
            .and_then(|text| BlueprintLock::from_json(&text));
        match lock {
            Ok(lock) => {
                let lock_errors = lock.check(&blueprint);
                for error in &lock_errors {
                    findings.push(Finding::new(
                        "blueprint-lock",
                        Severity::Error,
                        error.clone(),
                        Location::file(&relative),
                    ));
                }
                // Execute only the exact witness contract authorized by the lock. A changed
                // command is rejected above before repository-authored shell is invoked.
                if lock_errors.is_empty() {
                    for result in blueprint
                        .run_witnesses(root)
                        .into_iter()
                        .filter(|result| !result.passed)
                    {
                        findings.push(Finding::new(
                            "blueprint-lock",
                            Severity::Error,
                            format!("witness '{}' failed: {}", result.id, result.detail),
                            Location::file(&relative),
                        ));
                    }
                }
            }
            Err(error) => findings.push(Finding::new(
                "blueprint-lock",
                Severity::Error,
                error.to_string(),
                Location::file(&relative),
            )),
        }
    }
    findings
}

fn cmd_check(
    root: &Path,
    cache: bool,
    format: Format,
    staged: bool,
    since: Option<&str>,
) -> Result<()> {
    let cfg = load_or_derive(root)?;
    let tree = cfg.into_tree(Some(root))?;
    let graph = load_graph_opt(root); // shared by the ratchet + the conformance rules
    let mut findings: Vec<Finding> = Vec::new();

    // 1. structural invariants of the capability tree
    for e in tree.check() {
        findings.push(Finding::new(
            "tree-invariant",
            Severity::Error,
            e,
            Location::file(LEAN_BASELINE),
        ));
    }

    // 2. leanness ratchet — only if a baseline is committed (opt-in per repo)
    let baseline_path = root.join(LEAN_BASELINE);
    if baseline_path.exists() {
        let baseline: bonsai::lean::LeanBaseline =
            serde_json::from_str(&std::fs::read_to_string(&baseline_path)?)
                .with_context(|| format!("parsing {LEAN_BASELINE}"))?;
        // the fast gate: hashes + line counts, incremental cache, no shingles; the config skip
        // list excludes intentionally-redundant/kept trees (bench baselines, run logs, …).
        let mut scfg = bonsai::scan::ScanConfig::gate().skipping(&cfg.bonsai.skip);
        if cache {
            scfg = scfg.with_cache(bonsai::scan::ScanConfig::default_cache_path(root));
        }
        let scan = bonsai::scan::Scan::run(root, &scfg);
        let mut report = bonsai::lean::analyze_from_scan(&tree, &scan);
        report.dead_code = graph.as_ref().map(|g| g.dead_symbols(root, &[]).len());
        report.misplaced = graph.as_ref().map(|g| g.misplacements().len());
        for reg in baseline.regressions(&report) {
            findings.push(Finding::new(
                "leanness-ratchet",
                Severity::Error,
                reg,
                Location::file(LEAN_BASELINE),
            ));
        }
    }

    // 3. conformance rules — the admission gate (layering, bounded growth, placement)
    if let Some(rules) = &cfg.rules {
        findings.extend(bonsai::rules::evaluate(&tree, graph.as_ref(), rules));
    }

    // 4. architecture contracts — force each level to fulfill its anchor, forbid escape hatches,
    //    and keep sealed seams frozen (reads the .rs sources once, only when contracts exist)
    if !cfg.contracts.is_empty() {
        let ws = Workspace::from_dir_ext_skip(root, &["rs"], &cfg.bonsai.skip)
            .with_context(|| format!("scanning sources at {}", root.display()))?;
        findings.extend(bonsai::contracts::evaluate(&ws, &cfg.contracts));
    }

    // 5. conventionally discovered shape locks make the existing hook and CI command protect
    //    declarative architecture without requiring a second gate.
    findings.extend(check_blueprint_locks(root));

    // 6. the migration valve: grandfather declared/in-flight exceptions (allow-list) so an
    //    intentional change doesn't fight the gate — but report what was exempted (never silent).
    let exempted = if let Some(allow) = cfg.rules.as_ref().map(|r| &r.allow) {
        let (kept, exempted) = bonsai::rules::apply_allow(findings, allow);
        findings = kept;
        exempted
    } else {
        Vec::new()
    };

    // 7. inline `// bonsai:allow` — the line-level valve for a genuine false positive at source.
    let (kept, suppressed) = bonsai::rules::apply_inline_suppression(root, findings);
    findings = kept;

    // 8. diff-scoping: with --staged/--since, keep only findings on changed files (plus the
    //    repo-wide gates), so the pre-commit hook judges the addition, not pre-existing debt.
    let scoped = changed_files(root, staged, since);
    if let Some(changed) = &scoped {
        findings.retain(|f| {
            GLOBAL_RULES.contains(&f.rule.as_str()) || changed.contains(&f.location.file)
        });
    }

    let report = Report::new(findings);

    // Machine formats: emit the finding stream, then fail closed if any error exists
    // (CI gets both a SARIF/JSON report and a non-zero exit).
    if format != Format::Text {
        match format {
            Format::Json => println!("{}", report.to_json()),
            Format::Sarif => println!("{}", report.to_sarif()),
            Format::Text => unreachable!(),
        }
        if report.has_errors() {
            std::process::exit(1);
        }
        return Ok(());
    }

    // The exempted list is the audit trail — always surfaced, never silent.
    if !exempted.is_empty() {
        println!(
            "  ⓘ {} finding(s) exempted by the allow-list (migration valve):",
            exempted.len()
        );
        for f in exempted.iter().take(8) {
            println!("      [{}] {}", f.rule, f.location.file);
        }
    }
    if suppressed > 0 {
        println!("  ⓘ {suppressed} finding(s) waived inline (// bonsai:allow)");
    }
    if let Some(changed) = &scoped {
        println!(
            "  ⓘ diff-scoped to {} changed file(s) (repo-wide gates still apply)",
            changed.len()
        );
    }

    // Warnings (e.g. premature-abstraction) are reported but do NOT fail the gate — only
    // error-severity findings do. This keeps advisory smells from blocking a commit.
    let (errors, warnings, _) = report.counts();
    for f in &report.findings {
        let glyph = if f.severity == Severity::Error {
            "✗"
        } else {
            "▲"
        };
        println!("  {glyph} [{}] {} — {}", f.rule, f.location.file, f.message);
    }
    if errors == 0 {
        let note = if warnings > 0 {
            format!(" ({warnings} warning(s), non-blocking)")
        } else {
            String::new()
        };
        println!(
            "bonsai check: OK — {} nodes, invariants + ratchet + conformance hold{note}",
            tree.nodes.len()
        );
        Ok(())
    } else {
        anyhow::bail!("bonsai check: {errors} violation(s)");
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

fn cmd_init(root: &Path, force: bool, skip: &[String]) -> Result<()> {
    let cfg_path = root.join("bonsai.toml");
    if cfg_path.exists() && !force {
        anyhow::bail!(
            "{} already exists (pass --force to overwrite)",
            cfg_path.display()
        );
    }
    let cfg = Config::from_dir_with_skip(root, skip).context("deriving structure")?;
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

/// Apply a plan to disk: rewrite references (byte-exact, CRLF-preserving), then move the file.
/// Each file's edits are validated up front — a stale plan aborts with no partial write — and
/// every write is atomic (temp file + rename) so a crash mid-apply can't leave a torn file.
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
        let out = bonsai::moves::apply_edits(&text, &edits).map_err(|bad| {
            anyhow::anyhow!(
                "stale plan: {} references out-of-range line(s) {:?} (re-run the plan)",
                abs.display(),
                bad
            )
        })?;
        atomic_write(&abs, out.as_bytes()).with_context(|| format!("writing {}", abs.display()))?;
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

/// Write `bytes` to `path` atomically: a sibling temp file, fsync-free, then a rename over the
/// target (atomic on POSIX within one filesystem). A crash leaves either the old or new file
/// intact, never a half-written one.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.bonsai-tmp.{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp); // don't leak the temp on failure
            Err(e)
        }
    }
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
            if let Some(text) = bonsai::walk::read_text_capped(&root.join(anchor), 2_000_000) {
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
