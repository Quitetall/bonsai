//! The Python↔Rust FFI stitcher (ADR 0134) — the one boundary no off-the-shelf tool
//! crosses, and permanent core infra for LamQuant (Python-first research that never leaves
//! the codec even as the edge migrates to Rust).
//!
//! It links **PyO3 exports** on the Rust side (`#[pyfunction]` / `#[pyclass]` / `#[pymodule]`)
//! to their **import sites** on the Python side (`import m` / `from m import x`). SCIP indexes
//! each language in isolation and never joins them; this scanner stitches the two graphs at
//! the `#[pymodule]` name — the actual ABI seam — so a Rust export and its Python callers
//! become one connected component (no dead Python left behind when the edge moves to Rust).
//!
//! First cut is source-pattern based (regex-grade line scanning on both sides), deliberately
//! conservative: it reports the seam and which exported names each Python site pulls in.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Function,
    Class,
    Module,
}

/// A PyO3 symbol exported from Rust to Python.
#[derive(Debug, Clone)]
pub struct PyExport {
    pub py_name: String,
    pub kind: ExportKind,
    pub file: String,
    pub line: usize,
    /// The `#[pymodule]` this export belongs to (the import name Python sees).
    pub module: Option<String>,
}

/// A Python import statement.
#[derive(Debug, Clone)]
pub struct PyImport {
    pub module: String,
    pub names: Vec<String>,
    pub file: String,
    pub line: usize,
}

/// A stitched edge: a Rust `#[pymodule]` and a Python site that imports it.
#[derive(Debug, Clone)]
pub struct FfiEdge {
    pub module: String,
    pub rust_file: String,
    pub py_file: String,
    pub py_line: usize,
    /// Exported names the Python side actually pulls in (empty = whole-module `import m`).
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FfiGraph {
    pub exports: Vec<PyExport>,
    pub imports: Vec<PyImport>,
    pub edges: Vec<FfiEdge>,
}

/// Scan a repo for the FFI boundary. Reads `.rs` (PyO3 exports) + `.py` (imports), then
/// stitches them at the `#[pymodule]` name.
pub fn scan(root: &Path) -> FfiGraph {
    let mut g = FfiGraph::default();
    let mut module_file: BTreeMap<String, String> = BTreeMap::new();

    for rel in crate::walk::files_with_ext(root, &[], &["rs", "py"]) {
        let relstr = rel.to_string_lossy().to_string();
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        match rel.extension().and_then(|e| e.to_str()) {
            Some("rs") => scan_rust(&text, &relstr, &mut g.exports, &mut module_file),
            Some("py") => scan_python(&text, &relstr, &mut g.imports),
            _ => {}
        }
    }

    // stitch: a Python import whose module last-segment matches a Rust #[pymodule] name.
    let modules: BTreeSet<String> = module_file.keys().cloned().collect();
    for imp in &g.imports {
        let last = imp.module.rsplit('.').next().unwrap_or(&imp.module);
        if let Some(rust_file) = module_file.get(last) {
            g.edges.push(FfiEdge {
                module: last.to_string(),
                rust_file: rust_file.clone(),
                py_file: imp.file.clone(),
                py_line: imp.line,
                names: imp.names.clone(),
            });
        } else if modules.contains(&imp.module) {
            // `from pkg import module` where `module` is the extension
            if let Some(rust_file) = module_file.get(&imp.module) {
                g.edges.push(FfiEdge {
                    module: imp.module.clone(),
                    rust_file: rust_file.clone(),
                    py_file: imp.file.clone(),
                    py_line: imp.line,
                    names: imp.names.clone(),
                });
            }
        }
    }
    g
}

/// Extract PyO3 exports. A `#[py*]` attribute arms an export that the next `fn`/`struct`
/// consumes; `#[pyo3(name = "X")]` overrides the exported name (stacked attributes are fine).
fn scan_rust(
    text: &str,
    file: &str,
    exports: &mut Vec<PyExport>,
    module_file: &mut BTreeMap<String, String>,
) {
    let mut pending: Option<ExportKind> = None;
    let mut name_override: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim_start();
        if t.starts_with("#[pyfunction") {
            pending = Some(ExportKind::Function);
        } else if t.starts_with("#[pyclass") {
            pending = Some(ExportKind::Class);
        } else if t.starts_with("#[pymodule") {
            pending = Some(ExportKind::Module);
        } else if let Some(n) = parse_pyo3_name(t) {
            name_override = Some(n);
        } else if let Some(kind) = pending {
            // consume at the next item definition
            let ident = if kind == ExportKind::Class {
                after_keyword(t, "struct").or_else(|| after_keyword(t, "enum"))
            } else {
                after_keyword(t, "fn")
            };
            if let Some(rust_name) = ident {
                let py_name = name_override.take().unwrap_or_else(|| rust_name.clone());
                if kind == ExportKind::Module {
                    module_file
                        .entry(py_name.clone())
                        .or_insert_with(|| file.to_string());
                }
                exports.push(PyExport {
                    py_name,
                    kind,
                    file: file.to_string(),
                    line: i + 1,
                    module: None,
                });
                pending = None;
            }
        }
    }
}

/// `#[pyo3(name = "foo")]` → `foo`.
fn parse_pyo3_name(t: &str) -> Option<String> {
    let rest = t.strip_prefix("#[pyo3(")?;
    let idx = rest.find("name")?;
    let after = &rest[idx + 4..];
    let q1 = after.find('"')? + 1;
    let q2 = after[q1..].find('"')? + q1;
    Some(after[q1..q2].to_string())
}

/// The identifier following `keyword ` on a line, up to a non-identifier char.
fn after_keyword(t: &str, kw: &str) -> Option<String> {
    let pat = format!("{kw} ");
    let idx = t.find(&pat)?;
    // ensure keyword boundary (start of line, or preceded by non-ident like `pub `)
    let before_ok = idx == 0 || !t.as_bytes()[idx - 1].is_ascii_alphanumeric();
    if !before_ok {
        return None;
    }
    let rest = &t[idx + pat.len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Extract Python `import`/`from … import …` statements (single-line forms).
fn scan_python(text: &str, file: &str, imports: &mut Vec<PyImport>) {
    for (i, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix("from ") {
            if let Some((module, names)) = rest.split_once(" import ") {
                let names = names
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .split(',')
                    .map(|n| n.split(" as ").next().unwrap_or(n).trim().to_string())
                    .filter(|n| !n.is_empty() && n != "*")
                    .collect();
                imports.push(PyImport {
                    module: module.trim().to_string(),
                    names,
                    file: file.to_string(),
                    line: i + 1,
                });
            }
        } else if let Some(rest) = t.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.split(" as ").next().unwrap_or(part).trim();
                if !module.is_empty() {
                    imports.push(PyImport {
                        module: module.to_string(),
                        names: Vec::new(),
                        file: file.to_string(),
                        line: i + 1,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn probe(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("bonsai_ffi_probe_{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("rust")).unwrap();
        fs::create_dir_all(base.join("py")).unwrap();
        base
    }

    #[test]
    fn stitches_pymodule_to_python_import() {
        let base = probe("stitch");
        fs::write(
            base.join("rust/lib.rs"),
            r#"
#[pyfunction]
fn encode(x: i32) -> i32 { x }

#[pyo3(name = "decode_fast")]
#[pyfunction]
fn decode(x: i32) -> i32 { x }

#[pymodule]
fn lamquant_lml(m: &PyModule) -> PyResult<()> { Ok(()) }
"#,
        )
        .unwrap();
        fs::write(
            base.join("py/train.py"),
            "import os\nfrom lamquant_lml import encode, decode_fast\n",
        )
        .unwrap();

        let g = scan(&base);
        // exports: encode (fn), decode_fast (renamed fn), lamquant_lml (module)
        assert!(g
            .exports
            .iter()
            .any(|e| e.py_name == "encode" && e.kind == ExportKind::Function));
        assert!(
            g.exports.iter().any(|e| e.py_name == "decode_fast"),
            "{:?}",
            g.exports
        );
        assert!(g
            .exports
            .iter()
            .any(|e| e.py_name == "lamquant_lml" && e.kind == ExportKind::Module));
        // edge: train.py ↔ lamquant_lml, pulling encode + decode_fast
        let edge = g.edges.iter().find(|e| e.module == "lamquant_lml");
        assert!(edge.is_some(), "no FFI edge: {:?}", g.edges);
        let edge = edge.unwrap();
        assert!(edge.names.contains(&"encode".to_string()));
        assert!(edge.py_file.ends_with("train.py"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn no_edge_without_a_matching_module() {
        let base = probe("noedge");
        fs::write(base.join("rust/lib.rs"), "#[pyfunction]\nfn f() {}\n").unwrap();
        fs::write(base.join("py/a.py"), "import numpy as np\n").unwrap();
        let g = scan(&base);
        assert!(g.edges.is_empty(), "spurious edge: {:?}", g.edges);
        let _ = fs::remove_dir_all(&base);
    }
}
