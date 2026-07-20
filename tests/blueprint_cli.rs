use std::fs;
use std::process::Command;

fn blueprint(lifecycle: &str) -> String {
    format!(
        r#"[blueprint]
id = "example.pipeline.v1"
lifecycle = "{lifecycle}"

[[slot]]
id = "ingest"
invariants = ["accepts bytes"]

[[slot]]
id = "output"
invariants = ["emits records"]

[[port]]
id = "bytes"
slot = "ingest"
direction = "input"
schema = "example.Bytes/v1"
compatibility = "permanent"

[[port]]
id = "records"
slot = "ingest"
direction = "output"
schema = "example.Records/v1"

[[port]]
id = "output-records"
slot = "output"
direction = "input"
schema = "example.Records/v1"

[[flow]]
from = "records"
to = "output-records"

[[witness]]
id = "contract"
kind = "command"
command = "true"
covers = ["ingest", "output"]
"#
    )
}

fn bonsai() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bonsai"))
}

#[test]
fn validate_verify_lock_and_check_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipeline.toml");
    let results = dir.path().join("results.json");
    let lock = dir.path().join("pipeline.lock.json");
    fs::write(&path, blueprint("locked")).unwrap();

    assert!(bonsai()
        .args(["blueprint", "validate"])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["blueprint", "verify"])
        .arg(&path)
        .args(["--root"])
        .arg(dir.path())
        .args(["--out"])
        .arg(&results)
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["blueprint", "lock"])
        .arg(&path)
        .args(["--results"])
        .arg(&results)
        .args(["--out"])
        .arg(&lock)
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["blueprint", "check"])
        .arg(&path)
        .args(["--lock"])
        .arg(&lock)
        .status()
        .unwrap()
        .success());

    let conventional = dir.path().join(".bonsai/blueprints");
    fs::create_dir_all(&conventional).unwrap();
    fs::copy(&path, conventional.join("pipeline.toml")).unwrap();
    fs::copy(&lock, conventional.join("pipeline.lock.json")).unwrap();
    assert!(bonsai()
        .args(["init", "--root"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["check", "--root"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
}

#[test]
fn repository_check_reruns_the_locked_witness_contract() {
    let dir = tempfile::tempdir().unwrap();
    let conventional = dir.path().join(".bonsai/blueprints");
    fs::create_dir_all(&conventional).unwrap();
    let path = conventional.join("pipeline.toml");
    let results = dir.path().join("results.json");
    let lock = conventional.join("pipeline.lock.json");
    fs::write(
        &path,
        blueprint("locked").replace("command = \"true\"", "command = \"touch witness-ran\""),
    )
    .unwrap();

    assert!(bonsai()
        .args(["blueprint", "verify"])
        .arg(&path)
        .args(["--root"])
        .arg(dir.path())
        .args(["--out"])
        .arg(&results)
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["blueprint", "lock"])
        .arg(&path)
        .args(["--results"])
        .arg(&results)
        .args(["--out"])
        .arg(&lock)
        .status()
        .unwrap()
        .success());
    fs::remove_file(dir.path().join("witness-ran")).unwrap();
    assert!(bonsai()
        .args(["init", "--root"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(bonsai()
        .args(["check", "--root"])
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(dir.path().join("witness-ran").exists());
}

#[test]
fn scaffold_emits_language_specific_contract_stubs_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipeline.toml");
    let out = dir.path().join("generated");
    fs::write(&path, blueprint("draft")).unwrap();

    let mut command = bonsai();
    command
        .args(["blueprint", "scaffold"])
        .arg(&path)
        .args(["--language", "rust", "--out"])
        .arg(&out);
    assert!(command.status().unwrap().success());
    let ingest = fs::read_to_string(out.join("ingest.rs")).unwrap();
    assert!(ingest.contains("pub trait Ingest"));
    assert!(ingest.contains("pub struct IngestInput"));
    assert!(ingest.contains("pub bytes: Vec<u8>"));
    assert!(ingest.contains("pub struct IngestOutput"));
    assert!(ingest.contains("pub records: Vec<u8>"));
    assert!(ingest.contains("fn execute(&mut self, input: IngestInput)"));
    assert!(ingest.contains("fn schema_contract_is_well_formed()"));
    assert!(ingest.contains("example.Bytes/v1"));

    assert!(
        !command.status().unwrap().success(),
        "must not overwrite authored code"
    );

    for (language, file, expected) in [
        (
            "python",
            "ingest.py",
            &[
                "class IngestInput",
                "bytes: bytes",
                "def execute(self, input:",
            ][..],
        ),
        (
            "c",
            "ingest.h",
            &[
                "typedef struct",
                "bytes_data",
                "ingest_execute(void *context, const ingest_input *input",
            ][..],
        ),
        (
            "type-script",
            "ingest.ts",
            &[
                "interface IngestInput",
                "bytes: Uint8Array",
                "execute(input: IngestInput)",
            ][..],
        ),
    ] {
        let language_out = out.join(language);
        assert!(bonsai()
            .args(["blueprint", "scaffold"])
            .arg(&path)
            .args(["--language", language, "--out"])
            .arg(&language_out)
            .status()
            .unwrap()
            .success());
        let generated = fs::read_to_string(language_out.join(file)).unwrap();
        for needle in expected {
            assert!(generated.contains(needle), "{language} omitted {needle}");
        }
    }
}
