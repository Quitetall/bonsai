//! Declarative architecture blueprints: author a logical shape, bind implementations,
//! prove its contracts, then lock the shape against accidental drift.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blueprint {
    #[serde(rename = "blueprint")]
    pub meta: BlueprintMeta,
    #[serde(default, rename = "slot")]
    pub slots: Vec<Slot>,
    #[serde(default, rename = "port")]
    pub ports: Vec<Port>,
    #[serde(default, rename = "flow")]
    pub flows: Vec<Flow>,
    #[serde(default, rename = "implementation")]
    pub implementations: Vec<Implementation>,
    #[serde(default, rename = "adapter")]
    pub adapters: Vec<CompatibilityAdapter>,
    #[serde(default, rename = "witness")]
    pub witnesses: Vec<Witness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlueprintMeta {
    pub id: String,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    #[default]
    Draft,
    Locked,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: String,
    #[serde(default)]
    pub responsibility: String,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub allow_fusion: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    pub id: String,
    pub slot: String,
    pub direction: Direction,
    pub schema: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub compatibility: CompatibilityPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityPolicy {
    #[default]
    Internal,
    Versioned,
    Permanent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub stateful: bool,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Implementation {
    pub id: String,
    pub slots: Vec<String>,
    #[serde(default)]
    pub bindings: Vec<String>,
    #[serde(default)]
    pub variant_of: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityAdapter {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub binding: String,
    #[serde(default)]
    pub witnesses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub id: String,
    pub kind: WitnessKind,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub covers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WitnessKind {
    Static,
    Typecheck,
    Schema,
    Golden,
    Property,
    Differential,
    Abi,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessResult {
    pub id: String,
    pub passed: bool,
    #[serde(default)]
    pub detail: String,
}

impl WitnessResult {
    pub fn passed(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            passed: true,
            detail: String::new(),
        }
    }

    pub fn failed(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            passed: false,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldLanguage {
    Rust,
    Python,
    C,
    TypeScript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldFile {
    pub relative_path: String,
    pub content: String,
}

impl Blueprint {
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Validate authored shape without reading source or running witnesses.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.meta.id.trim().is_empty() {
            errors.push("blueprint id must not be empty".into());
        }
        unique(
            "slot",
            self.slots.iter().map(|slot| slot.id.as_str()),
            &mut errors,
        );
        unique(
            "port",
            self.ports.iter().map(|port| port.id.as_str()),
            &mut errors,
        );
        unique(
            "implementation",
            self.implementations.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        unique(
            "adapter",
            self.adapters.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        unique(
            "witness",
            self.witnesses.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        for (kind, id) in self
            .slots
            .iter()
            .map(|item| ("slot", item.id.as_str()))
            .chain(self.ports.iter().map(|item| ("port", item.id.as_str())))
        {
            if !safe_id(id) {
                errors.push(format!(
                    "{kind} id '{id}' must use only letters, digits, '.', '-', or '_'"
                ));
            }
        }

        let slots: BTreeSet<&str> = self.slots.iter().map(|slot| slot.id.as_str()).collect();
        let ports: BTreeMap<&str, &Port> = self
            .ports
            .iter()
            .map(|port| (port.id.as_str(), port))
            .collect();
        let witnesses: BTreeSet<&str> = self.witnesses.iter().map(|w| w.id.as_str()).collect();

        for port in &self.ports {
            if !slots.contains(port.slot.as_str()) {
                errors.push(format!(
                    "port '{}': slot '{}' does not exist",
                    port.id, port.slot
                ));
            }
            if port.schema.trim().is_empty() {
                errors.push(format!("port '{}': schema must not be empty", port.id));
            }
        }
        for flow in &self.flows {
            match ports.get(flow.from.as_str()) {
                None => errors.push(format!("flow: source port '{}' does not exist", flow.from)),
                Some(port) if port.direction != Direction::Output => errors.push(format!(
                    "flow: source port '{}' is not an output",
                    flow.from
                )),
                _ => {}
            }
            match ports.get(flow.to.as_str()) {
                None => errors.push(format!("flow: target port '{}' does not exist", flow.to)),
                Some(port) if port.direction != Direction::Input => {
                    errors.push(format!("flow: target port '{}' is not an input", flow.to))
                }
                _ => {}
            }
        }
        for implementation in &self.implementations {
            if implementation.slots.is_empty() {
                errors.push(format!("implementation '{}': no slots", implementation.id));
            }
            for slot in &implementation.slots {
                if !slots.contains(slot.as_str()) {
                    errors.push(format!(
                        "implementation '{}': slot '{}' does not exist",
                        implementation.id, slot
                    ));
                }
            }
            if implementation.slots.len() > 1 {
                for slot in &implementation.slots {
                    if self
                        .slots
                        .iter()
                        .find(|candidate| candidate.id == *slot)
                        .is_some_and(|candidate| !candidate.allow_fusion)
                    {
                        errors.push(format!(
                            "implementation '{}': fused slot '{}' does not allow fusion",
                            implementation.id, slot
                        ));
                    }
                }
                let fused: BTreeSet<&str> =
                    implementation.slots.iter().map(String::as_str).collect();
                let has_differential_witness = self.witnesses.iter().any(|witness| {
                    witness.kind == WitnessKind::Differential
                        && fused
                            .iter()
                            .all(|slot| witness.covers.iter().any(|covered| covered == slot))
                });
                if !has_differential_witness {
                    errors.push(format!(
                        "implementation '{}': fused slots require a differential witness covering every slot",
                        implementation.id
                    ));
                }
            }
        }
        let adapter_sources: BTreeSet<&str> = self
            .adapters
            .iter()
            .map(|adapter| adapter.from.as_str())
            .collect();
        for adapter in &self.adapters {
            if !ports.contains_key(adapter.from.as_str()) {
                errors.push(format!(
                    "adapter '{}': from port '{}' does not exist",
                    adapter.id, adapter.from
                ));
            }
            if !ports.contains_key(adapter.to.as_str()) {
                errors.push(format!(
                    "adapter '{}': to port '{}' does not exist",
                    adapter.id, adapter.to
                ));
            }
            if adapter_sources.contains(adapter.to.as_str()) {
                errors.push(format!(
                    "adapter '{}': adapter chain through '{}' is forbidden; adapt directly to the current port",
                    adapter.id, adapter.to
                ));
            }
            if adapter.witnesses.is_empty() {
                errors.push(format!(
                    "adapter '{}': no compatibility witnesses",
                    adapter.id
                ));
            }
            for witness in &adapter.witnesses {
                if !witnesses.contains(witness.as_str()) {
                    errors.push(format!(
                        "adapter '{}': witness '{}' does not exist",
                        adapter.id, witness
                    ));
                }
            }
        }
        let coverable: BTreeSet<&str> =
            slots.iter().copied().chain(ports.keys().copied()).collect();
        for witness in &self.witnesses {
            for covered in &witness.covers {
                if !coverable.contains(covered.as_str()) {
                    errors.push(format!(
                        "witness '{}': target '{}' does not exist",
                        witness.id, covered
                    ));
                }
            }
        }

        if has_unmarked_cycle(self, &ports) {
            errors.push("flow cycle must include an explicitly stateful flow".into());
        }
        errors.sort();
        errors.dedup();
        errors
    }

    /// Digest only logical shape. Implementations, witnesses, description, and lifecycle are
    /// deliberately excluded so compatible implementation evolution keeps one shape identity.
    pub fn shape_digest(&self) -> String {
        let mut slots: Vec<_> = self
            .slots
            .iter()
            .map(|slot| {
                let mut invariants = slot.invariants.clone();
                invariants.sort();
                (slot.id.as_str(), invariants, slot.allow_fusion)
            })
            .collect();
        let mut ports = self.ports.clone();
        let mut flows = self.flows.clone();
        slots.sort_by(|a, b| a.0.cmp(b.0));
        ports.sort_by(|a, b| a.id.cmp(&b.id));
        flows.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
        let bytes = serde_json::to_vec(&(slots, ports, flows)).expect("shape is serializable");
        format!("b3:{}", blake3::hash(&bytes).to_hex())
    }

    /// Run explicit, repository-authored witness commands from the requested repository root.
    pub fn run_witnesses(&self, root: &Path) -> Vec<WitnessResult> {
        self.witnesses
            .iter()
            .map(|witness| {
                if witness.command.trim().is_empty() {
                    return WitnessResult::failed(&witness.id, "witness command is empty");
                }
                match Command::new("sh")
                    .args(["-c", &witness.command])
                    .current_dir(root)
                    .status()
                {
                    Ok(status) if status.success() => WitnessResult::passed(&witness.id),
                    Ok(status) => {
                        WitnessResult::failed(&witness.id, format!("command exited with {status}"))
                    }
                    Err(error) => WitnessResult::failed(&witness.id, error.to_string()),
                }
            })
            .collect()
    }

    pub fn scaffold_files(&self, language: ScaffoldLanguage) -> Vec<ScaffoldFile> {
        self.slots
            .iter()
            .map(|slot| scaffold_slot(self, slot, language))
            .collect()
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

fn snake(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn pascal(id: &str) -> String {
    id.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn scaffold_slot(blueprint: &Blueprint, slot: &Slot, language: ScaffoldLanguage) -> ScaffoldFile {
    let ports = blueprint
        .ports
        .iter()
        .filter(|port| port.slot == slot.id)
        .collect::<Vec<_>>();
    let contracts = ports
        .iter()
        .map(|port| format!("{} {:?}: {}", port.id, port.direction, port.schema))
        .collect::<Vec<_>>()
        .join("\n");
    let inputs = ports
        .iter()
        .copied()
        .filter(|port| port.direction == Direction::Input)
        .collect::<Vec<_>>();
    let outputs = ports
        .iter()
        .copied()
        .filter(|port| port.direction == Direction::Output)
        .collect::<Vec<_>>();
    let name = pascal(&slot.id);
    let stem = snake(&slot.id);
    let (extension, content) = match language {
        ScaffoldLanguage::Rust => {
            let rust_fields = |selected: &[&Port]| -> String {
                if selected.is_empty() {
                    "    // This side of the slot has no ports.\n".into()
                } else {
                    selected
                        .iter()
                        .map(|port| format!("    pub {}: Vec<u8>,\n", snake(&port.id)))
                        .collect::<String>()
                }
            };
            let schema_constants = ports
                .iter()
                .map(|port| {
                    format!(
                        "pub const {}_SCHEMA: &str = {};\n",
                        snake(&port.id).to_ascii_uppercase(),
                        serde_json::to_string(&port.schema).expect("schema is serializable")
                    )
                })
                .collect::<String>();
            let schemas = ports
                .iter()
                .map(|port| format!("{}_SCHEMA", snake(&port.id).to_ascii_uppercase()))
                .collect::<Vec<_>>()
                .join(", ");
            (
                "rs",
                format!(
                    "//! Generated typed contract scaffold for slot `{}`.\n//! Port payloads are serialized bytes governed by the schema constants below.\n//! {}\n\n{schema_constants}\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct {name}Input {{\n{}}}\n\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct {name}Output {{\n{}}}\n\npub trait {name} {{\n    type Error;\n\n    fn execute(&mut self, input: {name}Input) -> Result<{name}Output, Self::Error>;\n}}\n\n#[cfg(test)]\nmod contract_tests {{\n    use super::*;\n\n    pub fn assert_implementation<T: {name}>() {{}}\n\n    #[test]\n    fn schema_contract_is_well_formed() {{\n        let schemas: &[&str] = &[{schemas}];\n        assert!(schemas.iter().all(|schema| !schema.is_empty()));\n    }}\n}}\n",
                    slot.id,
                    contracts.replace('\n', "\n//! "),
                    rust_fields(&inputs),
                    rust_fields(&outputs),
                ),
            )
        }
        ScaffoldLanguage::Python => {
            let python_fields = |selected: &[&Port]| -> String {
                if selected.is_empty() {
                    "    pass\n".into()
                } else {
                    selected
                        .iter()
                        .map(|port| format!("    {}: bytes\n", snake(&port.id)))
                        .collect::<String>()
                }
            };
            let schema_constants = ports
                .iter()
                .map(|port| {
                    format!(
                        "{}_SCHEMA = {}\n",
                        snake(&port.id).to_ascii_uppercase(),
                        serde_json::to_string(&port.schema).expect("schema is serializable")
                    )
                })
                .collect::<String>();
            (
                "py",
                format!(
                    "\"\"\"Generated typed contract scaffold for slot `{}`.\n{}\n\"\"\"\nfrom dataclasses import dataclass\nfrom typing import Protocol\n\n{schema_constants}\n@dataclass(frozen=True)\nclass {name}Input:\n{}\n@dataclass(frozen=True)\nclass {name}Output:\n{}\nclass {name}(Protocol):\n    def execute(self, input: {name}Input) -> {name}Output: ...\n\n\ndef test_schema_contract_is_well_formed() -> None:\n    assert all(({}))\n",
                    slot.id,
                    contracts,
                    python_fields(&inputs),
                    python_fields(&outputs),
                    ports
                        .iter()
                        .map(|port| snake(&port.id).to_ascii_uppercase() + "_SCHEMA")
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        }
        ScaffoldLanguage::C => {
            let c_fields = |selected: &[&Port]| -> String {
                if selected.is_empty() {
                    "    unsigned char _unused;\n".into()
                } else {
                    selected
                        .iter()
                        .map(|port| {
                            format!(
                                "    const unsigned char *{}_data;\n    size_t {}_len;\n",
                                snake(&port.id),
                                snake(&port.id)
                            )
                        })
                        .collect::<String>()
                }
            };
            let upper = stem.to_ascii_uppercase();
            let schema_constants = ports
                .iter()
                .map(|port| {
                    format!(
                        "#define BONSAI_{}_{}_SCHEMA {}\n",
                        upper,
                        snake(&port.id).to_ascii_uppercase(),
                        serde_json::to_string(&port.schema).expect("schema is serializable")
                    )
                })
                .collect::<String>();
            (
                "h",
                format!(
                    "/* Generated typed contract scaffold for slot `{}`.\n * {}\n */\n#ifndef BONSAI_{upper}_H\n#define BONSAI_{upper}_H\n#include <stddef.h>\n\n{schema_constants}\ntypedef struct {{\n{}}} {stem}_input;\n\ntypedef struct {{\n{}}} {stem}_output;\n\nint {stem}_execute(void *context, const {stem}_input *input, {stem}_output *output);\n\n#endif\n",
                    slot.id,
                    contracts.replace('\n', "\n * "),
                    c_fields(&inputs),
                    c_fields(&outputs)
                ),
            )
        }
        ScaffoldLanguage::TypeScript => {
            let ts_fields = |selected: &[&Port]| {
                selected
                    .iter()
                    .map(|port| format!("  {}: Uint8Array;\n", snake(&port.id)))
                    .collect::<String>()
            };
            let schema_constants = ports
                .iter()
                .map(|port| {
                    format!(
                        "export const {}_SCHEMA = {} as const;\n",
                        snake(&port.id).to_ascii_uppercase(),
                        serde_json::to_string(&port.schema).expect("schema is serializable")
                    )
                })
                .collect::<String>();
            (
                "ts",
                format!(
                    "/** Generated typed contract scaffold for slot `{}`.\n * {}\n */\n{schema_constants}\nexport interface {name}Input {{\n{}}}\n\nexport interface {name}Output {{\n{}}}\n\nexport interface {name} {{\n  execute(input: {name}Input): Promise<{name}Output>;\n}}\n",
                    slot.id,
                    contracts.replace('\n', "\n * "),
                    ts_fields(&inputs),
                    ts_fields(&outputs)
                ),
            )
        }
    };
    ScaffoldFile {
        relative_path: format!("{stem}.{extension}"),
        content,
    }
}

fn unique<'a>(kind: &str, ids: impl Iterator<Item = &'a str>, errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            errors.push(format!("{kind} id must not be empty"));
        } else if !seen.insert(id) {
            errors.push(format!("duplicate {kind} id '{id}'"));
        }
    }
}

/// A cycle remaining after stateful edges are removed is undeclared combinational recursion.
fn has_unmarked_cycle(blueprint: &Blueprint, ports: &BTreeMap<&str, &Port>) -> bool {
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for flow in blueprint.flows.iter().filter(|flow| !flow.stateful) {
        if let (Some(from), Some(to)) = (ports.get(flow.from.as_str()), ports.get(flow.to.as_str()))
        {
            edges
                .entry(from.slot.as_str())
                .or_default()
                .push(to.slot.as_str());
        }
    }
    fn visit<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        active: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> bool {
        if active.contains(node) {
            return true;
        }
        if done.contains(node) {
            return false;
        }
        active.insert(node);
        if edges
            .get(node)
            .is_some_and(|next| next.iter().any(|child| visit(child, edges, active, done)))
        {
            return true;
        }
        active.remove(node);
        done.insert(node);
        false
    }
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    edges
        .keys()
        .any(|node| visit(node, &edges, &mut active, &mut done))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeClass {
    NoChange,
    ImplementationOnly,
    NewShapeRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeDiff {
    pub class: ChangeClass,
    pub old_digest: String,
    pub new_digest: String,
}

impl ShapeDiff {
    pub fn between(old: &Blueprint, new: &Blueprint) -> Self {
        let old_digest = old.shape_digest();
        let new_digest = new.shape_digest();
        let class = if old_digest != new_digest {
            ChangeClass::NewShapeRequired
        } else if old.implementations != new.implementations || old.adapters != new.adapters {
            ChangeClass::ImplementationOnly
        } else {
            ChangeClass::NoChange
        };
        Self {
            class,
            old_digest,
            new_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlueprintLock {
    pub schema_version: u32,
    pub blueprint_id: String,
    pub shape_digest: String,
    pub permanent_ports: Vec<String>,
    pub witnesses: Vec<Witness>,
}

impl BlueprintLock {
    pub fn create(blueprint: &Blueprint, results: &[WitnessResult]) -> anyhow::Result<Self> {
        let errors = blueprint.validate();
        anyhow::ensure!(
            errors.is_empty(),
            "invalid blueprint: {}",
            errors.join("; ")
        );
        anyhow::ensure!(
            blueprint.meta.lifecycle == Lifecycle::Locked,
            "blueprint lifecycle must be locked before creating a lock"
        );
        let by_id: BTreeMap<&str, &WitnessResult> = results
            .iter()
            .map(|result| (result.id.as_str(), result))
            .collect();
        for witness in &blueprint.witnesses {
            let result = by_id
                .get(witness.id.as_str())
                .ok_or_else(|| anyhow::anyhow!("witness '{}' was not run", witness.id))?;
            anyhow::ensure!(
                result.passed,
                "witness '{}' failed: {}",
                witness.id,
                result.detail
            );
        }
        let mut permanent_ports: Vec<String> = blueprint
            .ports
            .iter()
            .filter(|port| port.compatibility == CompatibilityPolicy::Permanent)
            .map(|port| port.id.clone())
            .collect();
        permanent_ports.sort();
        let mut witnesses = blueprint.witnesses.clone();
        witnesses.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self {
            schema_version: 2,
            blueprint_id: blueprint.meta.id.clone(),
            shape_digest: blueprint.shape_digest(),
            permanent_ports,
            witnesses,
        })
    }

    pub fn check(&self, blueprint: &Blueprint) -> Vec<String> {
        let mut errors = blueprint.validate();
        if self.schema_version != 2 {
            errors.push(format!(
                "unsupported blueprint lock schema {}; recreate the lock with this Bonsai version",
                self.schema_version
            ));
        }
        if self.blueprint_id != blueprint.meta.id {
            errors.push(format!(
                "lock belongs to '{}' not '{}'",
                self.blueprint_id, blueprint.meta.id
            ));
        }
        let current = blueprint.shape_digest();
        if self.shape_digest != current {
            errors.push(format!(
                "locked shape changed: {} -> {}; create a new blueprint identity",
                self.shape_digest, current
            ));
        }
        let mut permanent: Vec<String> = blueprint
            .ports
            .iter()
            .filter(|port| port.compatibility == CompatibilityPolicy::Permanent)
            .map(|port| port.id.clone())
            .collect();
        permanent.sort();
        if permanent != self.permanent_ports {
            errors.push("permanent port set changed".into());
        }
        let mut witnesses = blueprint.witnesses.clone();
        witnesses.sort_by(|left, right| left.id.cmp(&right.id));
        if witnesses != self.witnesses {
            errors.push("witness contract changed".into());
        }
        errors.sort();
        errors.dedup();
        errors
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(text)?)
    }
}
