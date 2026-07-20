//! The uniform finding model (ADR 0134) — one typed result stream for every analyzer, with
//! text / JSON / SARIF renderers so Bonsai can drive a real CI gate, not just a terminal.
//!
//! Every leanness signal (duplicate, near-duplicate, empty structure, dead code, misplacement)
//! and every `check` violation (broken invariant, ratchet regression) becomes a [`Finding`]:
//! a rule id, a severity, a message, a primary [`Location`], optional related locations, and an
//! optional suggested fix. This is the Tricorder discipline — a single analyzer-agnostic shape
//! that renders identically everywhere and carries a false-positive-aware severity.
//!
//! Renderers:
//!   - [`Report::to_text`]  — compact human list (the default is still the rich per-command UI);
//!   - [`Report::to_json`]  — a flat machine list (easy to post-process / diff);
//!   - [`Report::to_sarif`] — SARIF 2.1.0, the interchange format GitHub code scanning, GitLab,
//!     and most review tools ingest to render inline annotations.

use serde::Serialize;

/// Severity of a finding. Maps to SARIF `error`/`warning`/`note` and gates the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// SARIF result level.
    pub fn sarif_level(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
        }
    }
    fn glyph(self) -> &'static str {
        match self {
            Severity::Error => "✗",
            Severity::Warning => "▲",
            Severity::Info => "·",
        }
    }
}

/// A source location: a repo-relative file, optionally a 1-indexed line.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl Location {
    pub fn file(file: impl Into<String>) -> Self {
        Location {
            file: file.into(),
            line: None,
        }
    }
    pub fn at(file: impl Into<String>, line: u32) -> Self {
        Location {
            file: file.into(),
            line: Some(line),
        }
    }
    fn render(&self) -> String {
        match self.line {
            Some(l) => format!("{}:{}", self.file, l),
            None => self.file.clone(),
        }
    }
}

/// One analyzer result. `rule` is a stable id (see [`RULES`]) used as the SARIF `ruleId`.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Finding {
    pub fn new(
        rule: &str,
        severity: Severity,
        message: impl Into<String>,
        location: Location,
    ) -> Self {
        Finding {
            rule: rule.to_string(),
            severity,
            message: message.into(),
            location,
            related: Vec::new(),
            fix: None,
        }
    }
    pub fn with_related(mut self, related: Vec<Location>) -> Self {
        self.related = related;
        self
    }
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// The rule catalog — one entry per analyzer, surfaced in the SARIF `tool.driver.rules` so a
/// consumer can show a description and link findings back to the rule that produced them.
pub const RULES: &[(&str, &str)] = &[
    (
        "duplicate",
        "Byte-identical files within one repository (redundant copies).",
    ),
    (
        "near-duplicate",
        "Highly similar (but not identical) files — likely divergent copies.",
    ),
    (
        "clone-function",
        "A function body copy-pasted across files (Type-1/2 clone).",
    ),
    (
        "empty-dir",
        "A directory with no tracked files (dead structure).",
    ),
    (
        "dead-code",
        "A symbol unreachable from the public API, `main`, tests, or a trait impl.",
    ),
    (
        "misplaced",
        "A file used exclusively from one other directory — belongs there.",
    ),
    (
        "structure-cycle",
        "A dependency cycle in the real code graph (mutually-entangled modules).",
    ),
    (
        "tree-invariant",
        "A structural invariant of the capability tree is violated.",
    ),
    (
        "leanness-ratchet",
        "A leanness metric regressed past its committed baseline.",
    ),
];

/// A collected set of findings plus the tool version, ready to render.
#[derive(Debug, Clone)]
pub struct Report {
    pub tool_version: String,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn new(findings: Vec<Finding>) -> Self {
        Report {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            findings,
        }
    }

    /// True if any finding is error-severity (drives a fail-closed exit code).
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Counts by severity, for a summary line.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut e = 0;
        let mut w = 0;
        let mut i = 0;
        for f in &self.findings {
            match f.severity {
                Severity::Error => e += 1,
                Severity::Warning => w += 1,
                Severity::Info => i += 1,
            }
        }
        (e, w, i)
    }

    /// Compact human list (one line per finding + a summary). The per-command UIs stay the
    /// default; this is the `--format text` of the finding stream itself.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            out.push_str(&format!(
                "{} [{}] {} — {}\n",
                f.severity.glyph(),
                f.rule,
                f.location.render(),
                f.message
            ));
            for r in &f.related {
                out.push_str(&format!("      ↔ {}\n", r.render()));
            }
            if let Some(fix) = &f.fix {
                out.push_str(&format!("      ↳ fix: {fix}\n"));
            }
        }
        let (e, w, i) = self.counts();
        out.push_str(&format!("\n{e} error(s), {w} warning(s), {i} info\n"));
        out
    }

    /// A flat JSON list of findings (plus the tool version) — the friendly machine format.
    pub fn to_json(&self) -> String {
        let doc = serde_json::json!({
            "tool": "bonsai",
            "version": self.tool_version,
            "findings": self.findings,
        });
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    }

    /// SARIF 2.1.0 — the static-analysis interchange format for CI code scanning.
    pub fn to_sarif(&self) -> String {
        let rules: Vec<serde_json::Value> = RULES
            .iter()
            .map(|(id, desc)| {
                serde_json::json!({
                    "id": id,
                    "shortDescription": { "text": desc },
                })
            })
            .collect();

        let results: Vec<serde_json::Value> = self
            .findings
            .iter()
            .map(|f| {
                let mut region = serde_json::Map::new();
                if let Some(l) = f.location.line {
                    region.insert("startLine".into(), serde_json::json!(l.max(1)));
                }
                let mut phys = serde_json::json!({
                    "artifactLocation": { "uri": f.location.file },
                });
                if !region.is_empty() {
                    phys["region"] = serde_json::Value::Object(region);
                }
                let related: Vec<serde_json::Value> = f
                    .related
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "physicalLocation": { "artifactLocation": { "uri": r.file } }
                        })
                    })
                    .collect();
                let mut result = serde_json::json!({
                    "ruleId": f.rule,
                    "level": f.severity.sarif_level(),
                    "message": { "text": message_with_fix(f) },
                    "locations": [ { "physicalLocation": phys } ],
                });
                if !related.is_empty() {
                    result["relatedLocations"] = serde_json::Value::Array(related);
                }
                result
            })
            .collect();

        let doc = serde_json::json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [ {
                "tool": { "driver": {
                    "name": "bonsai",
                    "version": self.tool_version,
                    "informationUri": "https://github.com/QuiteTall/bonsai",
                    "rules": rules,
                } },
                "results": results,
            } ],
        });
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Append the suggested fix into the SARIF message text (SARIF `fixes` are more elaborate; the
/// message suffix keeps the hint visible in every consumer without a full fix descriptor).
fn message_with_fix(f: &Finding) -> String {
    match &f.fix {
        Some(fix) => format!("{} (fix: {fix})", f.message),
        None => f.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report::new(vec![
            Finding::new(
                "duplicate",
                Severity::Warning,
                "2 identical copies",
                Location::file("a/x.rs"),
            )
            .with_related(vec![Location::file("b/x.rs")])
            .with_fix("keep one, replace the rest with a reference"),
            Finding::new(
                "dead-code",
                Severity::Error,
                "fn unused is never called",
                Location::at("src/lib.rs", 42),
            ),
        ])
    }

    #[test]
    fn severity_and_counts() {
        let r = sample();
        assert!(r.has_errors());
        assert_eq!(r.counts(), (1, 1, 0));
    }

    #[test]
    fn json_is_valid_and_contains_findings() {
        let v: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        assert_eq!(v["tool"], "bonsai");
        assert_eq!(v["findings"].as_array().unwrap().len(), 2);
        assert_eq!(v["findings"][0]["rule"], "duplicate");
        assert_eq!(v["findings"][0]["related"][0]["file"], "b/x.rs");
    }

    #[test]
    fn sarif_is_valid_2_1_0_shape() {
        let v: serde_json::Value = serde_json::from_str(&sample().to_sarif()).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "bonsai");
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        // dead-code finding carries a line region and error level
        let dead = results.iter().find(|r| r["ruleId"] == "dead-code").unwrap();
        assert_eq!(dead["level"], "error");
        assert_eq!(
            dead["locations"][0]["physicalLocation"]["region"]["startLine"],
            42
        );
        // duplicate finding carries a related location + fix in the message
        let dup = results.iter().find(|r| r["ruleId"] == "duplicate").unwrap();
        assert!(dup["message"]["text"].as_str().unwrap().contains("fix:"));
        assert_eq!(
            dup["relatedLocations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "b/x.rs"
        );
    }
}
