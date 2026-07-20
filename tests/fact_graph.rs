use bonsai::blueprint::Blueprint;
use bonsai::facts::{
    facts_from_blueprint, BqlQuery, Fact, FactObject, FactStore, SourceRef, SqliteFactStore,
};

fn edge(subject: &str, predicate: &str, object: &str) -> Fact {
    Fact {
        subject: subject.into(),
        predicate: predicate.into(),
        object: FactObject::Entity(object.into()),
        attributes: Default::default(),
        source: SourceRef {
            adapter: "test".into(),
            locator: "fixture".into(),
            digest: "b3:test".into(),
        },
    }
}

#[test]
fn snapshots_are_immutable_deduplicated_and_queryable() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteFactStore::open(dir.path().join("facts.db")).unwrap();
    let facts = vec![
        edge("api", "depends-on", "service"),
        edge("service", "depends-on", "database"),
        edge("api", "depends-on", "service"),
    ];
    let first = store.create_snapshot(None, &facts).unwrap();
    let same = store.create_snapshot(None, &facts).unwrap();
    assert_eq!(first.id, same.id, "content-addressed snapshots deduplicate");
    assert_eq!(store.facts(&first.id).unwrap().len(), 2);

    let dependencies = store
        .query(
            &first.id,
            &BqlQuery::parse("DEPENDENCIES api DEPTH 2").unwrap(),
        )
        .unwrap();
    assert_eq!(dependencies.entities, vec!["service", "database"]);

    let impact = store
        .query(
            &first.id,
            &BqlQuery::parse("IMPACT database DEPTH 2").unwrap(),
        )
        .unwrap();
    assert_eq!(impact.entities, vec!["service", "api"]);
}

#[test]
fn snapshot_parent_changes_identity_and_history_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteFactStore::open(dir.path().join("facts.db")).unwrap();
    let old = store
        .create_snapshot(None, &[edge("api", "depends-on", "v1")])
        .unwrap();
    let new = store
        .create_snapshot(Some(&old.id), &[edge("api", "depends-on", "v2")])
        .unwrap();
    assert_ne!(old.id, new.id);
    assert_eq!(
        store.facts(&old.id).unwrap()[0].object,
        FactObject::Entity("v1".into())
    );
    assert_eq!(
        store.facts(&new.id).unwrap()[0].object,
        FactObject::Entity("v2".into())
    );
}

#[test]
fn bql_rejects_ambiguous_or_unbounded_queries() {
    assert!(BqlQuery::parse("IMPACT database").is_err());
    assert!(BqlQuery::parse("IMPACT database DEPTH 0").is_err());
    assert!(BqlQuery::parse("DELETE EVERYTHING DEPTH 2").is_err());
}

#[test]
fn blueprint_adapter_exposes_logical_dependencies_with_provenance() {
    let blueprint = Blueprint::from_toml(
        r#"[blueprint]
id = "pipeline.v1"
[[slot]]
id = "ingest"
[[slot]]
id = "process"
[[port]]
id = "records"
slot = "ingest"
direction = "output"
schema = "Records/v1"
[[port]]
id = "input"
slot = "process"
direction = "input"
schema = "Records/v1"
[[flow]]
from = "records"
to = "input"
"#,
    )
    .unwrap();
    let facts = facts_from_blueprint(&blueprint, "architecture/pipeline.toml");
    assert!(facts.iter().any(|fact| {
        fact.subject == "pipeline.v1#slot/process"
            && fact.predicate == "depends-on"
            && fact.object == FactObject::Entity("pipeline.v1#slot/ingest".into())
            && fact.source.locator == "architecture/pipeline.toml"
    }));
}

#[test]
fn mutable_refs_point_to_immutable_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteFactStore::open(dir.path().join("facts.db")).unwrap();
    let snapshot = store
        .create_snapshot(None, &[edge("a", "depends-on", "b")])
        .unwrap();
    store.set_ref("main", &snapshot.id).unwrap();
    assert_eq!(store.resolve_snapshot("main").unwrap(), snapshot.id);
    assert_eq!(store.resolve_snapshot(&snapshot.id).unwrap(), snapshot.id);
}
