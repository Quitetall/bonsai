use std::fs;
use std::process::Command;

#[test]
fn graph_snapshot_and_bql_query_are_local_and_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    let blueprint = dir.path().join("pipeline.toml");
    let db = dir.path().join(".bonsai/facts.db");
    fs::write(
        &blueprint,
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

    assert!(Command::new(env!("CARGO_BIN_EXE_bonsai"))
        .args(["graph", "snapshot", "--db"])
        .arg(&db)
        .args(["--blueprint"])
        .arg(&blueprint)
        .status()
        .unwrap()
        .success());
    let output = Command::new(env!("CARGO_BIN_EXE_bonsai"))
        .args(["graph", "query", "--db"])
        .arg(&db)
        .args([
            "--snapshot",
            "main",
            "--bql",
            "DEPENDENCIES pipeline.v1#slot/process DEPTH 1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["entities"][0], "pipeline.v1#slot/ingest");

    let output = Command::new(env!("CARGO_BIN_EXE_bonsai"))
        .args(["graph", "graphql", "--db"])
        .arg(&db)
        .args([
            "--query",
            "{ dependencies(entity: \"pipeline.v1#slot/process\", depth: 1) { entities } }",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["data"]["dependencies"]["entities"][0],
        "pipeline.v1#slot/ingest"
    );
}

#[test]
fn graph_snapshot_discovers_repository_authorities_under_root() {
    let dir = tempfile::tempdir().unwrap();
    let blueprints = dir.path().join(".bonsai/blueprints");
    fs::create_dir_all(&blueprints).unwrap();
    fs::write(
        blueprints.join("service.toml"),
        r#"[blueprint]
id = "service.v1"
[[slot]]
id = "ingest"
"#,
    )
    .unwrap();

    assert!(Command::new(env!("CARGO_BIN_EXE_bonsai"))
        .args(["graph", "--root"])
        .arg(dir.path())
        .args(["snapshot", "--discover"])
        .status()
        .unwrap()
        .success());
    assert!(dir.path().join(".bonsai/facts.db").exists());
}
