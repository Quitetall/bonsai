//! The "forgotten" fold (ADR 0134) — code that *exists but was forgotten*, the failure mode
//! dead-code detection misses. Dead code is reachable-but-unused; forgotten code is worse:
//!
//!   - **unwired module** — a `.rs` file under a `src/` tree that **no `mod` declaration**
//!     includes. It isn't dead, it isn't even *compiled* — it fell out of the build and nobody
//!     noticed. The purest "forgotten."
//!   - **undocumented public API** — a module-level `pub` item with no doc comment: shipped to
//!     callers with no word on what it does.
//!
//! Both are source-level scans over the already-loaded [`Workspace`](crate::moves::Workspace)
//! `.rs` text (no SCIP needed, so they work anywhere), and both are advisory (warning/info) —
//! they surface what was forgotten without hard-blocking, the point being that *every part is
//! thought of* rather than silently rotting.

use crate::finding::{Finding, Location, Severity};
use crate::moves::Workspace;
use std::collections::BTreeSet;

/// Run both forgotten-code scans over the workspace.
pub fn evaluate(ws: &Workspace) -> Vec<Finding> {
    let mut out = Vec::new();
    unwired_modules(ws, &mut out);
    undocumented_pub(ws, &mut out);
    out
}

/// Every module name mentioned in a `mod <name>` declaration anywhere in the workspace. A file
/// whose module name never appears here is not wired into the build.
fn declared_modules(ws: &Workspace) -> BTreeSet<String> {
    let mut declared = BTreeSet::new();
    for text in ws.files.values() {
        for line in text.lines() {
            if let Some(name) = mod_name(line) {
                declared.insert(name);
            }
        }
    }
    declared
}

/// The module name a line declares: `mod x;`, `pub mod x;`, `pub(crate) mod x;`, `mod x {` — or
/// `None`. (An inline `mod x {}` still *names* `x`, which is enough to consider a file wired.)
fn mod_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t.strip_prefix("pub").map(str::trim_start).unwrap_or(t);
    let t = if t.starts_with('(') {
        t.find(')').map(|i| t[i + 1..].trim_start()).unwrap_or(t)
    } else {
        t
    };
    let rest = t.strip_prefix("mod ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Flag `.rs` files under a `src/` tree that no `mod` declaration wires in. Conservative: crate
/// roots + aggregators (`lib`/`main`/`mod`/`build`) and cargo's auto-discovered trees
/// (`bin`/`examples`/`benches`/`tests`) are always considered wired, and a file is only flagged
/// when its module name appears in *no* `mod` declaration at all (so a `#[path]` or cfg-gated
/// module that's mentioned somewhere is never a false positive).
fn unwired_modules(ws: &Workspace, out: &mut Vec<Finding>) {
    let declared = declared_modules(ws);
    for path in ws.files.keys() {
        let rel = path.to_string_lossy();
        let comps: Vec<&str> = rel.split('/').collect();
        if !comps.contains(&"src") {
            continue; // only the compiled module tree; not scripts/fixtures
        }
        if comps
            .iter()
            .any(|c| matches!(*c, "bin" | "examples" | "benches" | "tests"))
        {
            continue; // cargo auto-discovers these — no `mod` needed
        }
        let stem = comps
            .last()
            .and_then(|f| f.strip_suffix(".rs"))
            .unwrap_or("");
        if matches!(stem, "lib" | "main" | "mod" | "build") {
            continue; // crate roots + directory aggregators are wired by definition
        }
        if !declared.contains(stem) {
            out.push(
                Finding::new(
                    "forgotten-unwired",
                    Severity::Warning,
                    format!("`{rel}` is under src/ but no `mod {stem};` wires it in — not compiled, forgotten"),
                    Location::file(rel.to_string()),
                )
                .with_fix(format!("add `mod {stem};` to its parent module, or remove the file")),
            );
        }
    }
}

/// Flag module-level `pub` API items (column 0) with no `///` doc comment. Skips `pub(crate)` /
/// `pub(super)` (not public API), `pub use` re-exports, indented items (impl methods carry their
/// doc on the trait), and anything from the first `#[cfg(test)]` region on.
fn undocumented_pub(ws: &Workspace, out: &mut Vec<Finding>) {
    const KINDS: &[&str] = &[
        "pub fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub const ",
        "pub type ",
        "pub static ",
        "pub mod ",
    ];
    for (path, text) in &ws.files {
        let rel = path.to_string_lossy();
        let lines: Vec<&str> = text.lines().collect();
        let test_from = lines
            .iter()
            .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
            .unwrap_or(lines.len());
        for (i, line) in lines.iter().enumerate().take(test_from) {
            // module-level only: the item starts at column 0 (no leading whitespace)
            if line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }
            if !KINDS.iter().any(|k| line.starts_with(k)) {
                continue;
            }
            // walk up past stacked attributes to the line that would carry the doc
            let mut j = i;
            while j > 0 {
                let prev = lines[j - 1].trim_start();
                if prev.starts_with("#[") || prev.starts_with("#!") {
                    j -= 1;
                } else {
                    break;
                }
            }
            let documented = j > 0 && {
                let prev = lines[j - 1].trim_start();
                prev.starts_with("///") || prev.starts_with("/**") || prev.starts_with("//!")
            };
            if !documented {
                let item = line
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push(
                    Finding::new(
                        "forgotten-undoc",
                        Severity::Info,
                        format!("public `{item}` has no doc comment"),
                        Location::at(rel.to_string(), (i + 1) as u32),
                    )
                    .with_fix("add a `///` doc, or narrow visibility if it isn't public API"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_unwired_module_only() {
        let ws = Workspace::from_pairs(
            ".",
            [
                ("src/lib.rs", "pub mod wired;\n"),
                ("src/wired.rs", "pub fn a() {}\n"),
                ("src/orphan.rs", "pub fn b() {}\n"), // no `mod orphan;` anywhere → forgotten
                ("src/bin/tool.rs", "fn main() {}\n"), // cargo-discovered → not flagged
            ],
        );
        let f = evaluate(&ws);
        let unwired: Vec<&str> = f
            .iter()
            .filter(|x| x.rule == "forgotten-unwired")
            .map(|x| x.location.file.as_str())
            .collect();
        assert_eq!(
            unwired,
            vec!["src/orphan.rs"],
            "only the truly unwired file: {f:?}"
        );
    }

    #[test]
    fn flags_undocumented_pub_api() {
        let ws = Workspace::from_pairs(
            ".",
            [(
                "src/lib.rs",
                "/// documented.\npub fn good() {}\n\npub fn bad() {}\n\n#[inline]\npub fn attr_bad() {}\n\n/// doc\n#[inline]\npub fn attr_good() {}\n",
            )],
        );
        let f = evaluate(&ws);
        let undoc: Vec<&str> = f
            .iter()
            .filter(|x| x.rule == "forgotten-undoc")
            .map(|x| x.message.as_str())
            .collect();
        assert_eq!(undoc.len(), 2, "bad + attr_bad only: {f:?}");
        assert!(undoc.iter().all(|m| m.contains("pub fn")));
    }

    #[test]
    fn impl_methods_and_test_items_are_not_flagged() {
        let ws = Workspace::from_pairs(
            ".",
            [(
                "src/lib.rs",
                "pub struct S;\nimpl S {\n    pub fn method(&self) {}\n}\n#[cfg(test)]\nmod t {\n    pub fn helper() {}\n}\n",
            )],
        );
        // S is undocumented (flagged); the indented impl method + the test-region fn are not.
        let f = evaluate(&ws);
        let undoc: Vec<&str> = f
            .iter()
            .filter(|x| x.rule == "forgotten-undoc")
            .map(|x| x.message.as_str())
            .collect();
        assert_eq!(undoc.len(), 1, "only `pub struct S`: {undoc:?}");
        assert!(undoc[0].contains("pub struct"));
    }
}
