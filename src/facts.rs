//! Local-first, content-addressed repository facts and bounded impact queries.

use crate::blueprint::Blueprint;
use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum FactObject {
    Entity(String),
    Text(String),
    Integer(i64),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceRef {
    pub adapter: String,
    pub locator: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Fact {
    pub subject: String,
    pub predicate: String,
    pub object: FactObject,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    pub source: SourceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub parent_id: Option<String>,
    pub fact_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Traversal {
    Dependencies,
    Impact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BqlQuery {
    pub traversal: Traversal,
    pub entity: String,
    pub depth: usize,
}

impl BqlQuery {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        anyhow::ensure!(
            parts.len() == 4 && parts[2].eq_ignore_ascii_case("DEPTH"),
            "BQL syntax: (DEPENDENCIES|IMPACT) <entity> DEPTH <1..64>"
        );
        let traversal = if parts[0].eq_ignore_ascii_case("DEPENDENCIES") {
            Traversal::Dependencies
        } else if parts[0].eq_ignore_ascii_case("IMPACT") {
            Traversal::Impact
        } else {
            anyhow::bail!(
                "unknown BQL traversal '{}': expected DEPENDENCIES or IMPACT",
                parts[0]
            );
        };
        let depth: usize = parts[3].parse().context("BQL depth must be an integer")?;
        anyhow::ensure!(
            (1..=64).contains(&depth),
            "BQL depth must be between 1 and 64"
        );
        anyhow::ensure!(!parts[1].is_empty(), "BQL entity must not be empty");
        Ok(Self {
            traversal,
            entity: parts[1].into(),
            depth,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResult {
    pub entities: Vec<String>,
    pub supporting_facts: Vec<Fact>,
}

pub trait FactStore {
    fn create_snapshot(&self, parent_id: Option<&str>, facts: &[Fact]) -> anyhow::Result<Snapshot>;
    fn facts(&self, snapshot_id: &str) -> anyhow::Result<Vec<Fact>>;
    fn query(&self, snapshot_id: &str, query: &BqlQuery) -> anyhow::Result<QueryResult>;
    fn set_ref(&self, name: &str, snapshot_id: &str) -> anyhow::Result<()>;
    fn resolve_snapshot(&self, id_or_ref: &str) -> anyhow::Result<String>;
}

pub struct SqliteFactStore {
    connection: Mutex<Connection>,
}

impl SqliteFactStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS snapshots (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 fact_count INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS facts (
                 snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
                 ordinal INTEGER NOT NULL,
                 payload TEXT NOT NULL,
                 PRIMARY KEY (snapshot_id, ordinal)
             );
             CREATE INDEX IF NOT EXISTS facts_snapshot ON facts(snapshot_id);
             CREATE TABLE IF NOT EXISTS snapshot_refs (
                 name TEXT PRIMARY KEY,
                 snapshot_id TEXT NOT NULL REFERENCES snapshots(id)
             );",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("fact database mutex poisoned"))
    }
}

impl FactStore for SqliteFactStore {
    fn create_snapshot(&self, parent_id: Option<&str>, facts: &[Fact]) -> anyhow::Result<Snapshot> {
        let canonical: BTreeSet<Fact> = facts.iter().cloned().collect();
        let canonical: Vec<Fact> = canonical.into_iter().collect();
        let bytes = serde_json::to_vec(&(parent_id, &canonical))?;
        let id = format!("b3:{}", blake3::hash(&bytes).to_hex());
        let snapshot = Snapshot {
            id: id.clone(),
            parent_id: parent_id.map(str::to_owned),
            fact_count: canonical.len(),
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let exists: bool = transaction
            .query_row("SELECT 1 FROM snapshots WHERE id = ?1", [&id], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !exists {
            transaction.execute(
                "INSERT INTO snapshots(id, parent_id, fact_count) VALUES (?1, ?2, ?3)",
                params![id, parent_id, canonical.len() as i64],
            )?;
            for (ordinal, fact) in canonical.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO facts(snapshot_id, ordinal, payload) VALUES (?1, ?2, ?3)",
                    params![snapshot.id, ordinal as i64, serde_json::to_string(fact)?],
                )?;
            }
        }
        transaction.commit()?;
        Ok(snapshot)
    }

    fn facts(&self, snapshot_id: &str) -> anyhow::Result<Vec<Fact>> {
        let connection = self.connection()?;
        let known: bool = connection
            .query_row(
                "SELECT 1 FROM snapshots WHERE id = ?1",
                [snapshot_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        anyhow::ensure!(known, "unknown fact snapshot '{snapshot_id}'");
        let mut statement = connection
            .prepare("SELECT payload FROM facts WHERE snapshot_id = ?1 ORDER BY ordinal")?;
        let rows = statement.query_map([snapshot_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    fn query(&self, snapshot_id: &str, query: &BqlQuery) -> anyhow::Result<QueryResult> {
        let facts = self.facts(snapshot_id)?;
        let mut adjacency: BTreeMap<&str, Vec<(&str, &Fact)>> = BTreeMap::new();
        for fact in &facts {
            if fact.predicate != "depends-on" {
                continue;
            }
            let FactObject::Entity(object) = &fact.object else {
                continue;
            };
            let (from, to) = match query.traversal {
                Traversal::Dependencies => (fact.subject.as_str(), object.as_str()),
                Traversal::Impact => (object.as_str(), fact.subject.as_str()),
            };
            adjacency.entry(from).or_default().push((to, fact));
        }
        for edges in adjacency.values_mut() {
            edges.sort_by(|a, b| a.0.cmp(b.0));
        }

        let mut queue = VecDeque::from([(query.entity.as_str(), 0usize)]);
        let mut seen = BTreeSet::from([query.entity.as_str()]);
        let mut entities = Vec::new();
        let mut supporting = BTreeSet::new();
        while let Some((entity, depth)) = queue.pop_front() {
            if depth == query.depth {
                continue;
            }
            for &(next, fact) in adjacency.get(entity).into_iter().flatten() {
                supporting.insert(fact.clone());
                if seen.insert(next) {
                    entities.push(next.to_owned());
                    queue.push_back((next, depth + 1));
                }
            }
        }
        Ok(QueryResult {
            entities,
            supporting_facts: supporting.into_iter().collect(),
        })
    }

    fn set_ref(&self, name: &str, snapshot_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')),
            "snapshot ref must use only letters, digits, '.', '-', or '_'"
        );
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO snapshot_refs(name, snapshot_id) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET snapshot_id = excluded.snapshot_id",
            params![name, snapshot_id],
        )?;
        Ok(())
    }

    fn resolve_snapshot(&self, id_or_ref: &str) -> anyhow::Result<String> {
        let connection = self.connection()?;
        if let Some(id) = connection
            .query_row(
                "SELECT snapshot_id FROM snapshot_refs WHERE name = ?1",
                [id_or_ref],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let exists = connection
            .query_row(
                "SELECT id FROM snapshots WHERE id = ?1",
                [id_or_ref],
                |row| row.get(0),
            )
            .optional()?;
        exists.ok_or_else(|| anyhow::anyhow!("unknown snapshot or ref '{id_or_ref}'"))
    }
}

pub fn facts_from_blueprint(blueprint: &Blueprint, locator: &str) -> Vec<Fact> {
    let source_bytes =
        serde_json::to_vec(blueprint).expect("Blueprint contains only serializable values");
    let source = SourceRef {
        adapter: "bonsai-blueprint".into(),
        locator: locator.into(),
        digest: format!("b3:{}", blake3::hash(&source_bytes).to_hex()),
    };
    let entity = |kind: &str, id: &str| format!("{}#{kind}/{id}", blueprint.meta.id);
    let mut facts = Vec::new();
    let mut add_entity = |subject: String, predicate: &str, object: String| {
        facts.push(Fact {
            subject,
            predicate: predicate.into(),
            object: FactObject::Entity(object),
            attributes: BTreeMap::new(),
            source: source.clone(),
        });
    };
    for slot in &blueprint.slots {
        add_entity(
            entity("slot", &slot.id),
            "member-of",
            blueprint.meta.id.clone(),
        );
    }
    for port in &blueprint.ports {
        add_entity(
            entity("port", &port.id),
            "member-of",
            entity("slot", &port.slot),
        );
    }
    let ports: BTreeMap<&str, &str> = blueprint
        .ports
        .iter()
        .map(|port| (port.id.as_str(), port.slot.as_str()))
        .collect();
    for flow in &blueprint.flows {
        if let (Some(from), Some(to)) = (ports.get(flow.from.as_str()), ports.get(flow.to.as_str()))
        {
            add_entity(entity("slot", to), "depends-on", entity("slot", from));
        }
    }
    for implementation in &blueprint.implementations {
        for slot in &implementation.slots {
            add_entity(
                entity("implementation", &implementation.id),
                "realizes",
                entity("slot", slot),
            );
        }
    }
    facts.sort();
    facts.dedup();
    facts
}

pub fn facts_from_scip(graph: &crate::scip::CodeGraph, locator: &str, digest: &str) -> Vec<Fact> {
    let source = SourceRef {
        adapter: "scip".into(),
        locator: locator.into(),
        digest: digest.into(),
    };
    let symbol_id = |id: &str| format!("scip:symbol/{id}");
    let file_id = |id: &str| format!("scip:file/{id}");
    let mut facts = Vec::new();
    for (id, symbol) in &graph.symbols {
        let mut attributes = BTreeMap::new();
        attributes.insert("display".into(), symbol.display.clone());
        attributes.insert("kind".into(), symbol.kind.to_string());
        attributes.insert("line".into(), (symbol.def.sl + 1).to_string());
        attributes.insert("implementation".into(), symbol.is_impl.to_string());
        facts.push(Fact {
            subject: symbol_id(id),
            predicate: "defined-in".into(),
            object: FactObject::Entity(file_id(&symbol.file)),
            attributes,
            source: source.clone(),
        });
    }
    for (from, targets) in &graph.refs {
        for target in targets {
            facts.push(Fact {
                subject: symbol_id(from),
                predicate: "depends-on".into(),
                object: FactObject::Entity(symbol_id(target)),
                attributes: BTreeMap::new(),
                source: source.clone(),
            });
        }
    }
    for (from, targets) in &graph.file_deps {
        for target in targets {
            facts.push(Fact {
                subject: file_id(from),
                predicate: "depends-on".into(),
                object: FactObject::Entity(file_id(target)),
                attributes: BTreeMap::new(),
                source: source.clone(),
            });
        }
    }
    facts.sort();
    facts.dedup();
    facts
}
