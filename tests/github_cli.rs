use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn bonsai() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bonsai"))
}

#[test]
fn github_init_scaffolds_standard_policy_and_trusted_workflow() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("bonsai.toml"),
        "[bonsai]\nroot = \".\"\nversion = 1\n",
    )
    .unwrap();

    let status = bonsai()
        .args([
            "github",
            "init",
            "--root",
            dir.path().to_str().unwrap(),
            "--repo",
            "example/widget",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let policy = fs::read_to_string(dir.path().join("bonsai.toml")).unwrap();
    assert!(policy.contains("[github]"));
    assert!(policy.contains("profile = \"standard-v1\""));
    assert!(policy.contains("repository = \"example/widget\""));

    let workflow =
        fs::read_to_string(dir.path().join(".github/workflows/bonsai-compliance.yml")).unwrap();
    assert!(workflow.contains("pull_request_target:"));
    assert!(!workflow.contains("branches:"));
    assert!(workflow.contains("bonsai compliance"));
    assert!(workflow.contains("persist-credentials: false"));
    assert!(workflow
        .contains("actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1"));

    assert!(dir.path().join("SECURITY.md").is_file());
    assert!(dir.path().join(".github/dependabot.yml").is_file());
    assert!(dir
        .path()
        .join(".github/pull_request_template.md")
        .is_file());
}

#[test]
fn github_apply_refuses_missing_managed_workflow_before_network() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("bonsai.toml"),
        "[bonsai]\nroot = \".\"\nversion = 1\n",
    )
    .unwrap();
    assert!(bonsai()
        .args([
            "github",
            "init",
            "--root",
            dir.path().to_str().unwrap(),
            "--repo",
            "example/widget",
        ])
        .status()
        .unwrap()
        .success());
    fs::remove_file(dir.path().join(".github/workflows/bonsai-compliance.yml")).unwrap();
    let output = bonsai()
        .args([
            "github",
            "apply",
            "--root",
            dir.path().to_str().unwrap(),
            "--confirm",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("managed files drift"));
}

#[test]
fn github_init_refuses_to_overwrite_managed_files() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("bonsai.toml"),
        "[bonsai]\nroot = \".\"\nversion = 1\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
    fs::write(
        dir.path().join(".github/workflows/bonsai-compliance.yml"),
        "human-owned\n",
    )
    .unwrap();

    let output = bonsai()
        .args([
            "github",
            "init",
            "--root",
            dir.path().to_str().unwrap(),
            "--repo",
            "example/widget",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to overwrite"));
}

#[test]
fn github_init_records_explicit_codeowners_without_guessing_identity() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("bonsai.toml"),
        "[bonsai]\nroot = \".\"\nversion = 1\n",
    )
    .unwrap();
    assert!(bonsai()
        .args([
            "github",
            "init",
            "--root",
            dir.path().to_str().unwrap(),
            "--repo",
            "example/widget",
            "--codeowner",
            "@example/maintainers"
        ])
        .status()
        .unwrap()
        .success());
    assert_eq!(
        fs::read_to_string(dir.path().join(".github/CODEOWNERS")).unwrap(),
        "* @example/maintainers\n"
    );
    assert!(fs::read_to_string(dir.path().join("bonsai.toml"))
        .unwrap()
        .contains("codeowners = [\"@example/maintainers\"]"));
}

#[test]
fn github_doctor_reads_initialized_policy_without_network() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("bonsai.toml"),
        "[bonsai]\nroot = \".\"\nversion = 1\n",
    )
    .unwrap();
    assert!(bonsai()
        .args([
            "github",
            "init",
            "--root",
            dir.path().to_str().unwrap(),
            "--repo",
            "example/widget",
        ])
        .status()
        .unwrap()
        .success());
    let output = bonsai()
        .args(["github", "doctor", "--root", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("standard-v1 for example/widget"));
}
