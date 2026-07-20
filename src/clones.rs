//! Function-level clone detection (ADR 0134) — the "duplicated" signal below file granularity.
//!
//! File-level exact/near-dup misses the common case: the *same function* copy-pasted into two
//! files that are otherwise different (a validation routine, a parser, a conversion). This fold
//! extracts each function body, normalizes away comments/whitespace, and groups bodies with an
//! identical normalized form — a Type-1/lightly-Type-2 clone. Trivial bodies (getters, one-liners)
//! are filtered by a minimum-line threshold, and `#[cfg(test)]` regions are excluded (test helpers
//! duplicate legitimately). Source-level, reuses the loaded [`Workspace`](crate::moves::Workspace)
//! `.rs` text (no SCIP), advisory by default.

use crate::moves::Workspace;
use crate::scan::fnv1a;
use std::collections::BTreeMap;

/// A group of functions with byte-identical normalized bodies (a copy-paste clone).
#[derive(Debug, Clone)]
pub struct CloneGroup {
    /// `(file, function-name, 1-indexed line)` for each clone in the group.
    pub sites: Vec<(String, String, u32)>,
    /// Normalized body line count (the size of the duplicated logic).
    pub lines: usize,
}

/// One extracted function: where it is and its normalized body.
struct Func {
    file: String,
    name: String,
    line: u32,
    norm_hash: u64,
    norm_lines: usize,
}

/// Detect duplicated functions across the workspace whose normalized body is ≥ `min_lines`.
/// Returns clone groups sorted by size (largest first), deterministically.
pub fn function_clones(ws: &Workspace, min_lines: usize) -> Vec<CloneGroup> {
    let mut funcs: Vec<Func> = Vec::new();
    for (path, text) in &ws.files {
        let file = path.to_string_lossy().to_string();
        extract_functions(&file, text, min_lines, &mut funcs);
    }
    // group by normalized-body hash
    let mut by_hash: BTreeMap<u64, Vec<&Func>> = BTreeMap::new();
    for f in &funcs {
        by_hash.entry(f.norm_hash).or_default().push(f);
    }
    let mut groups: Vec<CloneGroup> = by_hash
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|g| {
            let mut sites: Vec<(String, String, u32)> = g
                .iter()
                .map(|f| (f.file.clone(), f.name.clone(), f.line))
                .collect();
            sites.sort();
            CloneGroup {
                sites,
                lines: g[0].norm_lines,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        b.sites
            .len()
            .cmp(&a.sites.len())
            .then(b.lines.cmp(&a.lines))
    });
    groups
}

/// Extract each top-level/impl `fn` body from `text`, brace-matched, and record the normalized
/// form when it is ≥ `min_lines`. Excludes the `#[cfg(test)]` region. Brace counting strips line
/// comments and string/char literals first, so a `{` inside a string doesn't skew the depth.
fn extract_functions(file: &str, text: &str, min_lines: usize, out: &mut Vec<Func>) {
    let lines: Vec<&str> = text.lines().collect();
    let test_from = lines
        .iter()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
        .unwrap_or(lines.len());

    let mut i = 0;
    while i < test_from {
        let Some(name) = fn_name(lines[i]) else {
            i += 1;
            continue;
        };
        // find the opening brace (this line or a following one), brace-match to the close, and
        // capture ONLY the lines strictly between them — the body, excluding the signature (so a
        // renamed clone still matches) and the closing-brace line.
        let mut depth = 0i32;
        let mut open_line: Option<usize> = None;
        let mut body: Vec<String> = Vec::new();
        let start_line = i;
        let mut j = i;
        'scan: while j < lines.len() {
            let code = strip_strings_and_comments(lines[j]);
            for ch in code.chars() {
                if ch == '{' {
                    depth += 1;
                    if open_line.is_none() {
                        open_line = Some(j);
                    }
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            match open_line {
                Some(ol) => {
                    if depth <= 0 {
                        break 'scan; // closing line — stop, don't capture it
                    }
                    if j > ol {
                        body.push(normalize_line(lines[j]));
                    }
                }
                None if j > start_line + 40 => break, // no brace within a sane window — not a fn
                None => {}
            }
            j += 1;
        }
        if open_line.is_some() && depth <= 0 {
            let norm: Vec<String> = body.into_iter().filter(|l| !l.is_empty()).collect();
            if norm.len() >= min_lines {
                let joined = norm.join("\n");
                out.push(Func {
                    file: file.to_string(),
                    name,
                    line: (start_line + 1) as u32,
                    norm_hash: fnv1a(joined.as_bytes()),
                    norm_lines: norm.len(),
                });
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
}

/// The function name a line declares (`fn foo(`, `pub fn foo(`, `pub(crate) async fn foo<...>(`),
/// or `None`. Requires the `(` to confirm it's a function, not `fn` in prose.
fn fn_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let idx = t.find("fn ")?;
    // `fn ` must be at the start or preceded by a keyword/space (not part of an identifier)
    if idx > 0 && t.as_bytes()[idx - 1].is_ascii_alphanumeric() {
        return None;
    }
    let after = &t[idx + 3..];
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // must have a `(` after the name (and optional generics) somewhere on the line
    after.contains('(').then_some(name)
}

/// Strip `//` line comments and the contents of `"…"` / `'…'` literals so brace counting is
/// robust (a `{`/`}` inside a string or comment must not affect depth).
fn strip_strings_and_comments(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_str: Option<char> = None;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        match in_str {
            Some(delim) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == delim {
                    in_str = None;
                }
            }
            None => {
                if c == '/' && chars.peek() == Some(&'/') {
                    break; // line comment → ignore the rest
                }
                if c == '"' || c == '\'' {
                    in_str = Some(c);
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Normalize a body line for clone comparison: drop a trailing `//` comment and trim. (Keeps the
/// code content so identical logic with different indentation/comments still matches.)
fn normalize_line(line: &str) -> String {
    let code = match line.find("//") {
        // avoid cutting inside a string — only treat // as a comment if not obviously in quotes
        Some(pos) if line[..pos].matches('"').count().is_multiple_of(2) => &line[..pos],
        _ => line,
    };
    code.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_copy_pasted_function_across_files() {
        let body = "let mut total = 0;\nfor x in items {\n    total += x * 2;\n    total -= 1;\n}\nreturn total;";
        let ws = Workspace::from_pairs(
            ".",
            [
                ("a.rs", format!("pub fn compute(items: &[i32]) -> i32 {{\n{body}\n}}\n").as_str()),
                // same body, different function name + file → a clone
                ("b.rs", format!("fn tally(items: &[i32]) -> i32 {{\n{body}\n}}\n").as_str()),
                // a distinct function → not in the group
                ("c.rs", "fn other() -> i32 {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    a + b + c + d\n}\n"),
            ],
        );
        let groups = function_clones(&ws, 5);
        assert_eq!(groups.len(), 1, "one clone group: {groups:?}");
        assert_eq!(groups[0].sites.len(), 2);
        let names: Vec<&str> = groups[0].sites.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"compute") && names.contains(&"tally"));
    }

    #[test]
    fn trivial_and_test_functions_are_not_clones() {
        let ws = Workspace::from_pairs(
            ".",
            [
                // two trivial getters (below min_lines) — not flagged
                ("a.rs", "pub fn x(&self) -> u8 { self.x }\n"),
                ("b.rs", "pub fn y(&self) -> u8 { self.x }\n"),
                // identical bodies but inside #[cfg(test)] — excluded
                ("c.rs", "#[cfg(test)]\nmod t {\n fn h() {\n  let a=1;\n  let b=2;\n  let c=3;\n  let d=4;\n  let e=5;\n }\n}\n"),
                ("d.rs", "#[cfg(test)]\nmod t {\n fn g() {\n  let a=1;\n  let b=2;\n  let c=3;\n  let d=4;\n  let e=5;\n }\n}\n"),
            ],
        );
        assert!(
            function_clones(&ws, 5).is_empty(),
            "trivial + test fns must not clone"
        );
    }

    #[test]
    fn braces_in_strings_dont_break_matching() {
        // the `}` in the string must not close the function early
        let ws = Workspace::from_pairs(
            ".",
            [
                ("a.rs", "fn f() {\n    let s = \"a } b { c\";\n    let x = 1;\n    let y = 2;\n    let z = 3;\n    println!(\"{}\", s);\n}\n"),
                ("b.rs", "fn g() {\n    let s = \"a } b { c\";\n    let x = 1;\n    let y = 2;\n    let z = 3;\n    println!(\"{}\", s);\n}\n"),
            ],
        );
        let groups = function_clones(&ws, 5);
        assert_eq!(groups.len(), 1, "string-brace-safe clone: {groups:?}");
    }
}
