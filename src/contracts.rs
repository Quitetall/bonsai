//! The architecture gate (ADR 0134) — Bonsai forcing additions to *do what their level needs,
//! and no more*, automatically.
//!
//! A [`Contract`](crate::config::Contract) governs one level (a path). It encodes three fitness
//! functions that the `check` gate runs fail-closed on every commit (through the installed hook),
//! so a programmer adding code cannot land a non-conformant addition:
//!
//!   - **grow-through-anchor-traits** — a file that *triggers* the level's kind (`when_defines`)
//!     must fulfill its anchor (`must_impl`): a new wire format has no license to exist without
//!     `impl Codec`. The addition earns its place only by satisfying the level's contract.
//!   - **no-more-than-the-level-needs** — `forbid`den substrings (a lower-level escape hatch)
//!     may not appear under the path.
//!   - **sealed seams** — a `sealed` abstraction (a trait/type) must persist: you extend it with
//!     a new impl a layer down, you cannot replace `Codec` with `LosslessCodec`. The seam is
//!     frozen; growth happens *beneath* it.
//!
//! Checks are source-level (the same pragmatic grep-grade approach as the FFI stitcher and the
//! dead-code header scan) and reuse the already-loaded [`Workspace`](crate::moves::Workspace)
//! `.rs` text, so no extra read pass.

use crate::config::Contract;
use crate::finding::{Finding, Location, Severity};
use crate::moves::Workspace;

/// Evaluate every contract over the workspace's Rust sources, returning error-severity findings
/// (empty = every level's contract holds). Reuses `ws.files` — no filesystem access here.
pub fn evaluate(ws: &Workspace, contracts: &[Contract]) -> Vec<Finding> {
    let mut out = Vec::new();
    for c in contracts {
        let level = if c.level.is_empty() {
            c.path.as_str()
        } else {
            c.level.as_str()
        };
        for (path, text) in &ws.files {
            let rel = path.to_string_lossy();
            if !rel.starts_with(&c.path) {
                continue;
            }
            anchor(c, level, &rel, text, &mut out);
            forbidden(c, level, &rel, text, &mut out);
        }
        sealed(ws, c, level, &mut out);
    }
    out
}

/// grow-through-anchor-traits: a triggered file must implement the anchor trait.
fn anchor(c: &Contract, level: &str, rel: &str, text: &str, out: &mut Vec<Finding>) {
    let Some(tr) = &c.must_impl else { return };
    // trigger: an explicit marker, or (absent) every file under the path.
    let triggered = match &c.when_defines {
        Some(marker) => text.contains(marker),
        None => true,
    };
    if triggered && !implements(text, tr) {
        let because = c
            .when_defines
            .as_deref()
            .map(|m| format!("defines `{m}` but"))
            .unwrap_or_else(|| "is at this level but".to_string());
        out.push(
            Finding::new(
                "contract-anchor",
                Severity::Error,
                format!(
                    "{level}: {rel} {because} has no `impl {tr}` — an addition here must fulfill the {tr} contract"
                ),
                Location::file(rel.to_string()),
            )
            .with_fix(format!("implement `{tr}` for the new type, or move it out of {}", c.path)),
        );
    }
}

/// no-more-than-the-level-needs: forbidden reach under the path.
fn forbidden(c: &Contract, level: &str, rel: &str, text: &str, out: &mut Vec<Finding>) {
    for pat in &c.forbid {
        if let Some(line) = first_hit_line(text, pat) {
            out.push(
                Finding::new(
                    "contract-forbid",
                    Severity::Error,
                    format!(
                        "{level}: forbidden `{pat}` under {} — not permitted at this level",
                        c.path
                    ),
                    Location::at(rel.to_string(), line),
                )
                .with_fix("route through the level's abstraction instead of the escape hatch"),
            );
        }
    }
}

/// sealed seams: the abstraction's definition must still exist somewhere in the repo.
fn sealed(ws: &Workspace, c: &Contract, level: &str, out: &mut Vec<Finding>) {
    let Some(seal) = &c.sealed else { return };
    let present = ws.files.values().any(|t| defines_type(t, seal));
    if !present {
        out.push(
            Finding::new(
                "contract-seal",
                Severity::Error,
                format!(
                    "{level}: sealed abstraction `{seal}` was removed or renamed — extend the seam with a new impl, don't replace it"
                ),
                Location::file(c.path.clone()),
            )
            .with_fix(format!("restore `{seal}`; add capability as a new impl beneath it")),
        );
    }
}

/// True if `text` contains an `impl <trait> for …` (any generics/where-clause form).
fn implements(text: &str, tr: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        // `impl Codec for`, `impl<T> Codec for`, `impl Codec<X> for`
        if let Some(rest) = t.strip_prefix("impl") {
            let rest = rest.trim_start();
            // skip a generic parameter list `<...>` immediately after impl
            let rest = rest
                .strip_prefix('<')
                .and_then(|r| r.split_once('>'))
                .map(|(_, r)| r.trim_start())
                .unwrap_or(rest);
            (rest.starts_with(tr)) && rest[tr.len()..].trim_start().split_once("for").is_some()
        } else {
            false
        }
    })
}

/// True if `text` defines a `trait`/`struct`/`enum`/`type` named exactly `name`.
fn defines_type(text: &str, name: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start().trim_start_matches("pub").trim_start();
        for kw in ["trait ", "struct ", "enum ", "type "] {
            if let Some(rest) = t.strip_prefix(kw) {
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if ident == name {
                    return true;
                }
            }
        }
        false
    })
}

/// The 1-indexed line of the first occurrence of `pat`, if any.
fn first_hit_line(text: &str, pat: &str) -> Option<u32> {
    text.lines()
        .position(|l| l.contains(pat))
        .map(|i| (i + 1) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Contract;

    fn contract() -> Contract {
        Contract {
            path: "codec/src".into(),
            level: "codec".into(),
            when_defines: Some("WireMagic".into()),
            must_impl: Some("Codec".into()),
            forbid: vec!["std::mem::transmute".into()],
            sealed: Some("Codec".into()),
        }
    }

    #[test]
    fn anchor_flags_trigger_without_impl() {
        let ws = Workspace::from_pairs(
            ".",
            [
                ("codec/src/trait.rs", "pub trait Codec { fn go(&self); }\n"),
                (
                    "codec/src/new_fmt.rs",
                    "const M: &[u8] = b\"WireMagic\";\npub struct NewFmt;\n",
                ),
            ],
        );
        let f = evaluate(&ws, &[contract()]);
        assert!(
            f.iter().any(|x| x.rule == "contract-anchor"),
            "a wire format without impl Codec must be flagged: {f:?}"
        );
    }

    #[test]
    fn anchor_satisfied_when_impl_present() {
        let ws = Workspace::from_pairs(
            ".",
            [
                ("codec/src/trait.rs", "pub trait Codec {}\n"),
                (
                    "codec/src/ok_fmt.rs",
                    "// WireMagic here\npub struct OkFmt;\nimpl Codec for OkFmt {}\n",
                ),
            ],
        );
        let f = evaluate(&ws, &[contract()]);
        assert!(
            !f.iter().any(|x| x.rule == "contract-anchor"),
            "impl present → OK: {f:?}"
        );
    }

    #[test]
    fn sealed_seam_removal_is_flagged() {
        // the Codec trait no longer exists anywhere → sealed violation
        let ws = Workspace::from_pairs(".", [("codec/src/x.rs", "pub struct Thing;\n")]);
        let f = evaluate(&ws, &[contract()]);
        assert!(
            f.iter().any(|x| x.rule == "contract-seal"),
            "removing the sealed Codec seam must be flagged: {f:?}"
        );
    }

    #[test]
    fn forbidden_reach_is_flagged() {
        let ws = Workspace::from_pairs(
            ".",
            [
                ("codec/src/trait.rs", "pub trait Codec {}\n"),
                ("codec/src/hack.rs", "// WireMagic\nimpl Codec for H {}\nfn f() { std::mem::transmute::<u8,i8>(0); }\n"),
            ],
        );
        let f = evaluate(&ws, &[contract()]);
        assert!(
            f.iter()
                .any(|x| x.rule == "contract-forbid" && x.location.line == Some(3)),
            "forbidden transmute must be flagged at its line: {f:?}"
        );
    }

    #[test]
    fn only_governs_declared_path() {
        // a WireMagic outside codec/src is not this contract's concern
        let ws = Workspace::from_pairs(
            ".",
            [
                ("codec/src/trait.rs", "pub trait Codec {}\n"),
                ("elsewhere/z.rs", "const M: &[u8] = b\"WireMagic\";\n"),
            ],
        );
        let f = evaluate(&ws, &[contract()]);
        assert!(
            !f.iter().any(|x| x.rule == "contract-anchor"),
            "out-of-path file ignored: {f:?}"
        );
    }
}
