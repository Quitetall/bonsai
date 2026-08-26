//! GitHub compliance: a small policy interface over a deliberately narrow remote adapter.
//!
//! Local Bonsai remains offline. This module is entered only through `bonsai github ...` and
//! treats incomplete remote observations as `unknown`, never as compliance.

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STANDARD_PROFILE: &str = "standard-v1";
pub const GOVERNED_PROFILE: &str = "governed-v1";
pub const RULESET_NAME: &str = "bonsai-standard-v1";
pub const REQUIRED_CHECK: &str = "bonsai compliance";
pub const CANONICAL_SOURCE: &str = "github:Quitetall/bonsai";
pub const CANONICAL_REVISION: &str = "97c29b5a1994a727ed566b556c26165b3f80c11d";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    #[serde(rename = "standard-v1")]
    StandardV1,
    #[serde(rename = "governed-v1")]
    GovernedV1,
}

impl Profile {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            STANDARD_PROFILE => Ok(Self::StandardV1),
            GOVERNED_PROFILE => Ok(Self::GovernedV1),
            _ => bail!("unsupported GitHub compliance profile '{value}'; expected {STANDARD_PROFILE} or {GOVERNED_PROFILE}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::StandardV1 => STANDARD_PROFILE,
            Self::GovernedV1 => GOVERNED_PROFILE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPolicy {
    pub profile: Profile,
    pub repository: String,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default = "canonical_source")]
    pub bonsai_source: String,
    #[serde(default = "canonical_revision")]
    pub bonsai_revision: String,
    /// Explicit owners only. Bonsai never invents a user/team principal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codeowners: Vec<String>,
}

fn canonical_source() -> String {
    CANONICAL_SOURCE.into()
}
fn canonical_revision() -> String {
    CANONICAL_REVISION.into()
}

impl GithubPolicy {
    pub fn standard(repository: String) -> Self {
        Self {
            profile: Profile::StandardV1,
            repository,
            default_branch: None,
            bonsai_source: canonical_source(),
            bonsai_revision: canonical_revision(),
            codeowners: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let (owner, repo) = self
            .repository
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("github.repository must be owner/repo"))?;
        anyhow::ensure!(
            github_name_component(owner) && github_name_component(repo),
            "github.repository must be owner/repo"
        );
        anyhow::ensure!(
            self.codeowners.iter().all(|owner| valid_codeowner(owner)),
            "github.codeowners must contain GitHub @user or @organization/team principals"
        );
        anyhow::ensure!(
            self.bonsai_source == CANONICAL_SOURCE,
            "github.bonsai_source must be canonical public source {CANONICAL_SOURCE}"
        );
        anyhow::ensure!(
            self.bonsai_revision.len() == 40
                && self
                    .bonsai_revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "github.bonsai_revision must be a full 40-character commit SHA"
        );
        Ok(())
    }
}

fn github_name_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_codeowner(value: &str) -> bool {
    let Some(value) = value.strip_prefix('@') else {
        return false;
    };
    let mut segments = value.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    github_name_component(first) && segments.all(github_name_component)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesetObservation {
    pub name: String,
    pub enforcement: String,
    pub targets_default_branch: bool,
    pub bypass_actors: usize,
    pub blocks_deletion: bool,
    pub blocks_non_fast_forward: bool,
    pub pull_request_required: bool,
    pub approvals: u32,
    pub dismisses_stale_reviews: bool,
    pub resolves_threads: bool,
    pub extra_approval_for_unattributed_changes: bool,
    pub strict_required_checks: bool,
    pub required_checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub repository: String,
    pub default_branch: String,
    pub actions_enabled: bool,
    pub sha_pinning_required: bool,
    pub dependabot_security_updates: Option<bool>,
    pub secret_scanning: Option<bool>,
    pub push_protection: Option<bool>,
    /// GitHub omitted one or more ruleset details required to prove no bypass exists.
    pub ruleset_visibility_complete: bool,
    pub rulesets: Vec<RulesetObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceReport {
    pub schema: String,
    pub profile: String,
    pub repository: String,
    pub default_branch: String,
    pub verdict: Verdict,
    pub rules: Vec<String>,
}

impl ComplianceReport {
    pub fn is_pass(&self) -> bool {
        self.verdict == Verdict::Pass
    }
    pub fn is_unknown(&self) -> bool {
        self.verdict == Verdict::Unknown
    }
}

pub fn unknown_report(policy: &GithubPolicy, rule: &str) -> ComplianceReport {
    ComplianceReport {
        schema: "oh.bonsai/github-compliance/v1".into(),
        profile: policy.profile.as_str().into(),
        repository: policy.repository.clone(),
        default_branch: policy
            .default_branch
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        verdict: Verdict::Unknown,
        rules: vec![rule.into()],
    }
}

pub fn finalize_report(report: &mut ComplianceReport) {
    report.rules.sort();
    report.rules.dedup();
    report.verdict = if report.rules.iter().any(|rule| rule.ends_with("-unknown")) {
        Verdict::Unknown
    } else if report.rules.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
}

pub fn evaluate(policy: &GithubPolicy, observation: &Observation) -> ComplianceReport {
    let mut rules = BTreeSet::new();
    if policy.repository != observation.repository {
        rules.insert("github-repository-identity".into());
    }
    if let Some(expected) = &policy.default_branch {
        if expected != &observation.default_branch {
            rules.insert("github-default-branch".into());
        }
    }
    if !observation.actions_enabled {
        rules.insert("github-actions-enabled".into());
    }
    if !observation.sha_pinning_required {
        rules.insert("github-actions-sha-pinning".into());
    }
    flag_security(
        &mut rules,
        "github-dependabot-security-updates",
        observation.dependabot_security_updates,
    );
    flag_security(
        &mut rules,
        "github-secret-scanning",
        observation.secret_scanning,
    );
    flag_security(
        &mut rules,
        "github-push-protection",
        observation.push_protection,
    );
    if !observation.ruleset_visibility_complete {
        rules.insert("github-ruleset-visibility-unknown".into());
    }

    let matching: Vec<_> = observation
        .rulesets
        .iter()
        .filter(|rule| {
            rule.name == RULESET_NAME && rule.enforcement == "active" && rule.targets_default_branch
        })
        .collect();
    if matching.is_empty() {
        rules.insert("github-standard-ruleset".into());
    }
    let protected = matching.iter().any(|rule| {
        rule.bypass_actors == 0
            && rule.blocks_deletion
            && rule.blocks_non_fast_forward
            && rule.pull_request_required
            && rule.approvals >= 1
            && rule.dismisses_stale_reviews
            && rule.resolves_threads
            && rule.extra_approval_for_unattributed_changes
            && rule.strict_required_checks
            && rule
                .required_checks
                .iter()
                .any(|check| check == REQUIRED_CHECK)
    });
    if !protected && !matching.is_empty() {
        let rule = matching[0];
        if rule.bypass_actors != 0 {
            rules.insert("github-no-bypass".into());
        }
        if !rule.blocks_deletion {
            rules.insert("github-deletion-protection".into());
        }
        if !rule.blocks_non_fast_forward {
            rules.insert("github-force-push-protection".into());
        }
        if !rule.pull_request_required || rule.approvals < 1 {
            rules.insert("github-pull-request-review".into());
        }
        if !rule.dismisses_stale_reviews {
            rules.insert("github-stale-review-dismissal".into());
        }
        if !rule.resolves_threads {
            rules.insert("github-resolved-threads".into());
        }
        if !rule.extra_approval_for_unattributed_changes {
            rules.insert("github-unattributed-approval".into());
        }
        if !rule.strict_required_checks {
            rules.insert("github-strict-required-checks".into());
        }
        if !rule
            .required_checks
            .iter()
            .any(|check| check == REQUIRED_CHECK)
        {
            rules.insert("github-required-check".into());
        }
    }
    let mut report = ComplianceReport {
        schema: "oh.bonsai/github-compliance/v1".into(),
        profile: policy.profile.as_str().into(),
        repository: observation.repository.clone(),
        default_branch: observation.default_branch.clone(),
        verdict: Verdict::Fail,
        rules: rules.into_iter().collect(),
    };
    finalize_report(&mut report);
    report
}

fn flag_security(rules: &mut BTreeSet<String>, rule: &str, value: Option<bool>) {
    match value {
        Some(true) => {}
        Some(false) => {
            rules.insert(rule.into());
        }
        None => {
            rules.insert(format!("{rule}-unknown"));
        }
    }
}

pub fn managed_files(policy: &GithubPolicy) -> BTreeMap<PathBuf, String> {
    let mut files = BTreeMap::new();
    files.insert(
        PathBuf::from(".github/workflows/bonsai-compliance.yml"),
        workflow(policy),
    );
    files.insert(
        PathBuf::from(".github/pull_request_template.md"),
        pr_template(policy),
    );
    files.insert(PathBuf::from(".github/dependabot.yml"), dependabot());
    files.insert(PathBuf::from("SECURITY.md"), security());
    if !policy.codeowners.is_empty() {
        files.insert(
            PathBuf::from(".github/CODEOWNERS"),
            format!("* {}\n", policy.codeowners.join(" ")),
        );
    }
    files
}

pub fn policy_block(policy: &GithubPolicy) -> String {
    #[derive(Serialize)]
    struct PolicyToml<'a> {
        github: &'a GithubPolicy,
    }
    format!(
        "\n{}",
        toml::to_string(&PolicyToml { github: policy }).expect("GithubPolicy serializes into TOML")
    )
}

pub fn init(root: &Path, policy: &GithubPolicy) -> Result<Vec<PathBuf>> {
    policy.validate()?;
    let config = root.join("bonsai.toml");
    let existing = std::fs::read_to_string(&config)
        .with_context(|| format!("reading {}", config.display()))?;
    anyhow::ensure!(
        !existing.lines().any(|line| line.trim() == "[github]"),
        "GitHub policy already exists in {}",
        config.display()
    );
    let files = managed_files(policy);
    let occupied: Vec<_> = files
        .keys()
        .map(|relative| root.join(relative))
        .filter(|path| path.exists())
        .collect();
    anyhow::ensure!(
        occupied.is_empty(),
        "refusing to overwrite managed files: {}",
        occupied
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (relative, body) in &files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    }
    std::fs::write(
        &config,
        format!("{}{}", existing.trim_end(), policy_block(policy)),
    )
    .with_context(|| format!("writing {}", config.display()))?;
    Ok(files.keys().cloned().collect())
}

pub fn validate_managed_files(root: &Path, policy: &GithubPolicy) -> Vec<String> {
    let expected = managed_files(policy);
    expected
        .into_iter()
        .filter_map(|(relative, body)| {
            let path = root.join(&relative);
            match std::fs::read_to_string(&path) {
                Ok(actual) if actual == body => None,
                _ => Some(format!("github-managed-file:{}", relative.display())),
            }
        })
        .collect()
}

/// A trusted-base workflow evaluates a PR checkout as data. The candidate may not replace the
/// profile, repository identity, or pinned Bonsai revision that the protected base selected.
pub fn candidate_policy_drift(root: &Path, policy: &GithubPolicy) -> Option<String> {
    #[derive(Deserialize)]
    struct Candidate {
        github: Option<GithubPolicy>,
    }
    let text = std::fs::read_to_string(root.join("bonsai.toml")).ok()?;
    match toml::from_str::<Candidate>(&text)
        .ok()
        .and_then(|candidate| candidate.github)
    {
        Some(candidate) if candidate == *policy => None,
        _ => Some("github-policy-drift".into()),
    }
}

fn workflow(_policy: &GithubPolicy) -> String {
    format!(
        r#"name: Bonsai compliance

on:
  pull_request_target:
  push:

permissions:
  contents: read
  pull-requests: read

jobs:
  compliance:
    name: bonsai compliance
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6
        with:
          ref: ${{{{ github.event.pull_request.base.sha || github.sha }}}}
          fetch-depth: 0
          persist-credentials: false
      - uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1 # v3.2.0
        id: app-token
        with:
          app-id: ${{{{ secrets.BONSAI_GITHUB_APP_ID }}}}
          private-key: ${{{{ secrets.BONSAI_GITHUB_APP_PRIVATE_KEY }}}}
      - name: Materialize candidate as data
        if: github.event_name == 'pull_request_target'
        env:
          PR_NUMBER: ${{{{ github.event.pull_request.number }}}}
          HEAD: ${{{{ github.event.pull_request.head.sha }}}}
        run: |
          git fetch --no-tags origin "refs/pull/$PR_NUMBER/head:refs/remotes/origin/pr-$PR_NUMBER"
          test "$(git rev-parse "origin/pr-$PR_NUMBER")" = "$HEAD"
          git worktree add --detach "$RUNNER_TEMP/candidate" "$HEAD"
      - name: Build trusted Bonsai
        run: |
          source="$(sed -nE 's/^bonsai_source = "([^"]+)"$/\1/p' bonsai.toml)"
          revision="$(sed -nE 's/^bonsai_revision = "([0-9a-f]{{40}})"$/\1/p' bonsai.toml)"
          test "$source" = "{source}" && test -n "$revision"
          git clone --filter=blob:none https://github.com/Quitetall/bonsai.git "$RUNNER_TEMP/bonsai"
          git -C "$RUNNER_TEMP/bonsai" checkout --detach "$revision"
          cd "$RUNNER_TEMP/bonsai"
          cargo build --locked --release
      - name: Check GitHub and candidate policy
        env:
          BONSAI_GITHUB_TOKEN: ${{{{ steps.app-token.outputs.token }}}}
        run: |
          candidate="$RUNNER_TEMP/candidate"
          if [ ! -d "$candidate" ]; then candidate="$GITHUB_WORKSPACE"; fi
          "$RUNNER_TEMP/bonsai/target/release/bonsai" github check --root "$candidate" --policy-root "$GITHUB_WORKSPACE" --format json
"#,
        source = CANONICAL_SOURCE
    )
}

fn pr_template(policy: &GithubPolicy) -> String {
    let warrant = if policy.profile == Profile::GovernedV1 {
        "\nWarrant: OW-WAR-####\n"
    } else {
        ""
    };
    format!("## Bonsai compliance\n\n- [ ] `bonsai check` passes.\n- [ ] GitHub policy files remain managed by `{}`.\n{}", policy.profile.as_str(), warrant)
}
fn dependabot() -> String {
    "version: 2\nupdates:\n  - package-ecosystem: cargo\n    directory: /\n    schedule:\n      interval: weekly\n  - package-ecosystem: github-actions\n    directory: /\n    schedule:\n      interval: weekly\n".into()
}
fn security() -> String {
    "# Security policy\n\nReport vulnerabilities privately to repository maintainers. Do not disclose exploitable details in public issues before remediation.\n".into()
}

pub struct GithubClient {
    client: Client,
    base: String,
    token: Option<String>,
}

impl GithubClient {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("BONSAI_GITHUB_TOKEN")
            .ok()
            .or_else(|| std::env::var("GITHUB_TOKEN").ok());
        Self::new(
            std::env::var("BONSAI_GITHUB_API_URL")
                .unwrap_or_else(|_| "https://api.github.com".into()),
            token,
        )
    }

    pub fn new(base: String, token: Option<String>) -> Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(Duration::from_secs(20)).build()?,
            base: base.trim_end_matches('/').into(),
            token,
        })
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let request = self
            .client
            .request(method, format!("{}{}", self.base, path))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, format!("bonsai/{}", env!("CARGO_PKG_VERSION")));
        match &self.token {
            Some(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        }
    }

    fn json(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let request = self.request(method, path);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        let response = request
            .send()
            .with_context(|| format!("calling GitHub {path}"))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();
        anyhow::ensure!(
            status.is_success(),
            "GitHub {path} returned {status}: {text}"
        );
        serde_json::from_str(&text).with_context(|| format!("parsing GitHub response from {path}"))
    }

    pub fn observe(&self, policy: &GithubPolicy) -> Result<Observation> {
        policy.validate()?;
        let repo = self.json(
            reqwest::Method::GET,
            &format!("/repos/{}", policy.repository),
            None,
        )?;
        let default_branch = repo
            .get("default_branch")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        anyhow::ensure!(
            !default_branch.is_empty(),
            "GitHub did not return a default branch for {}",
            policy.repository
        );
        let actions = self.json(
            reqwest::Method::GET,
            &format!("/repos/{}/actions/permissions", policy.repository),
            None,
        )?;
        let list = self.json(
            reqwest::Method::GET,
            &format!("/repos/{}/rulesets", policy.repository),
            None,
        )?;
        let mut rulesets = Vec::new();
        let mut ruleset_visibility_complete = true;
        for entry in list.as_array().cloned().unwrap_or_default() {
            let Some(id) = entry.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let detail = self.json(
                reqwest::Method::GET,
                &format!("/repos/{}/rulesets/{id}", policy.repository),
                None,
            )?;
            if detail.get("bypass_actors").is_none() {
                ruleset_visibility_complete = false;
            }
            rulesets.push(parse_ruleset(&detail, &default_branch));
        }
        let security = repo.get("security_and_analysis");
        Ok(Observation {
            repository: policy.repository.clone(),
            default_branch,
            actions_enabled: actions
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            sha_pinning_required: actions
                .get("sha_pinning_required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            dependabot_security_updates: security_status(security, "dependabot_security_updates"),
            secret_scanning: security_status(security, "secret_scanning"),
            push_protection: security_status(security, "secret_scanning_push_protection"),
            ruleset_visibility_complete,
            rulesets,
        })
    }

    pub fn apply(&self, policy: &GithubPolicy) -> Result<()> {
        anyhow::ensure!(
            self.has_token(),
            "refusing GitHub mutation without BONSAI_GITHUB_TOKEN or GITHUB_TOKEN"
        );
        let observation = self.observe(policy)?;
        let preflight = evaluate(policy, &observation);
        anyhow::ensure!(
            !preflight.is_unknown(),
            "refusing GitHub mutation while required controls are unknown: {}",
            preflight.rules.join(", ")
        );
        let list = self.json(
            reqwest::Method::GET,
            &format!("/repos/{}/rulesets", policy.repository),
            None,
        )?;
        let existing = list
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("name").and_then(Value::as_str) == Some(RULESET_NAME))
            })
            .and_then(|item| item.get("id").and_then(Value::as_i64));
        let payload = standard_ruleset_payload();
        match existing {
            Some(id) => {
                let current = self.json(
                    reqwest::Method::GET,
                    &format!("/repos/{}/rulesets/{id}", policy.repository),
                    None,
                )?;
                anyhow::ensure!(
                    safe_to_replace_ruleset(&current),
                    "refusing to replace a stronger or extended {RULESET_NAME} ruleset; preserve its controls manually before applying standard-v1"
                );
                self.json(
                    reqwest::Method::PUT,
                    &format!("/repos/{}/rulesets/{id}", policy.repository),
                    Some(payload),
                )?;
            }
            None => {
                self.json(
                    reqwest::Method::POST,
                    &format!("/repos/{}/rulesets", policy.repository),
                    Some(payload),
                )?;
            }
        }
        if !observation.actions_enabled || !observation.sha_pinning_required {
            let allowed = self
                .json(
                    reqwest::Method::GET,
                    &format!("/repos/{}/actions/permissions", policy.repository),
                    None,
                )?
                .get("allowed_actions")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .to_string();
            self.json(reqwest::Method::PUT, &format!("/repos/{}/actions/permissions", policy.repository), Some(json!({"enabled": true, "allowed_actions": allowed, "sha_pinning_required": true})))?;
        }
        self.json(reqwest::Method::PATCH, &format!("/repos/{}", policy.repository), Some(json!({"security_and_analysis": {"dependabot_security_updates": {"status": "enabled"}, "secret_scanning": {"status": "enabled"}, "secret_scanning_push_protection": {"status": "enabled"}}})))?;
        Ok(())
    }
}

/// `apply` may repair a Bonsai-owned ruleset only when replacement cannot erase a stronger
/// requirement. Extra rule types are an administrator decision, not automation input.
fn safe_to_replace_ruleset(value: &Value) -> bool {
    let rules = value
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let known = [
        "deletion",
        "non_fast_forward",
        "pull_request",
        "required_status_checks",
    ];
    if rules.iter().any(|rule| {
        !rule
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| known.contains(&kind))
    }) {
        return false;
    }
    let conditions_safe = value
        .pointer("/conditions/ref_name")
        .is_some_and(|conditions| {
            conditions.get("include").and_then(Value::as_array)
                == Some(&vec![Value::String("~DEFAULT_BRANCH".into())])
                && conditions
                    .get("exclude")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        });
    if !conditions_safe {
        return false;
    }
    let pr = rules
        .iter()
        .find(|rule| rule.get("type").and_then(Value::as_str) == Some("pull_request"))
        .and_then(|rule| rule.get("parameters"));
    if let Some(parameters) = pr {
        let known_parameters = [
            "dismiss_stale_reviews_on_push",
            "require_code_owner_review",
            "require_extra_approval_for_unattributed_changes",
            "require_last_push_approval",
            "required_approving_review_count",
            "required_review_thread_resolution",
            "required_reviewers",
            "allowed_merge_methods",
        ];
        if parameters.as_object().is_none_or(|object| {
            object
                .keys()
                .any(|key| !known_parameters.contains(&key.as_str()))
        }) || parameters
            .get("required_reviewers")
            .and_then(Value::as_array)
            .is_some_and(|reviewers| !reviewers.is_empty())
        {
            return false;
        }
        if parameters
            .get("required_approving_review_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 1
            || parameters
                .get("require_code_owner_review")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || parameters
                .get("require_last_push_approval")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return false;
        }
        let allowed: BTreeSet<_> = parameters
            .get("allowed_merge_methods")
            .and_then(Value::as_array)
            .map(|methods| methods.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !allowed.is_empty() && allowed != BTreeSet::from(["merge", "rebase", "squash"]) {
            return false;
        }
    }
    let checks = rules
        .iter()
        .find(|rule| rule.get("type").and_then(Value::as_str) == Some("required_status_checks"))
        .and_then(|rule| rule.get("parameters"));
    if let Some(parameters) = checks {
        let known_parameters = [
            "do_not_enforce_on_create",
            "strict_required_status_checks_policy",
            "required_status_checks",
        ];
        if parameters.as_object().is_none_or(|object| {
            object
                .keys()
                .any(|key| !known_parameters.contains(&key.as_str()))
        }) {
            return false;
        }
        let contexts: BTreeSet<_> = parameters
            .get("required_status_checks")
            .and_then(Value::as_array)
            .map(|checks| {
                checks
                    .iter()
                    .filter_map(|check| check.get("context").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if !contexts.is_empty() && contexts != BTreeSet::from([REQUIRED_CHECK]) {
            return false;
        }
        let bindings_safe = parameters
            .get("required_status_checks")
            .and_then(Value::as_array)
            .is_none_or(|checks| {
                checks.iter().all(|check| {
                    check.as_object().is_some_and(|object| {
                        object.keys().all(|key| key == "context")
                            && object.get("context").and_then(Value::as_str).is_some()
                    })
                })
            });
        if !bindings_safe {
            return false;
        }
    }
    true
}

fn security_status(security: Option<&Value>, name: &str) -> Option<bool> {
    security
        .and_then(|value| value.get(name))
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .map(|status| status == "enabled")
}

fn parse_ruleset(value: &Value, default_branch: &str) -> RulesetObservation {
    let rules = value
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let find = |kind: &str| {
        rules
            .iter()
            .find(|rule| rule.get("type").and_then(Value::as_str) == Some(kind))
    };
    let pr = find("pull_request").and_then(|rule| rule.get("parameters"));
    let checks = find("required_status_checks").and_then(|rule| rule.get("parameters"));
    let targets = value
        .pointer("/conditions/ref_name/include")
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|item| {
                item.as_str() == Some("~DEFAULT_BRANCH")
                    || item.as_str() == Some(default_branch)
                    || item.as_str() == Some(&format!("refs/heads/{default_branch}"))
            })
        })
        .unwrap_or(false);
    RulesetObservation {
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        enforcement: value
            .get("enforcement")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        targets_default_branch: targets,
        bypass_actors: value
            .get("bypass_actors")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        blocks_deletion: find("deletion").is_some(),
        blocks_non_fast_forward: find("non_fast_forward").is_some(),
        pull_request_required: pr.is_some(),
        approvals: pr
            .and_then(|p| p.get("required_approving_review_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        dismisses_stale_reviews: pr
            .and_then(|p| p.get("dismiss_stale_reviews_on_push"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        resolves_threads: pr
            .and_then(|p| p.get("required_review_thread_resolution"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        extra_approval_for_unattributed_changes: pr
            .and_then(|p| p.get("require_extra_approval_for_unattributed_changes"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        strict_required_checks: checks
            .and_then(|p| p.get("strict_required_status_checks_policy"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required_checks: checks
            .and_then(|p| p.get("required_status_checks"))
            .and_then(Value::as_array)
            .map(|checks| {
                checks
                    .iter()
                    .filter_map(|check| {
                        check
                            .get("context")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn standard_ruleset_payload() -> Value {
    json!({
        "name": RULESET_NAME, "target": "branch", "enforcement": "active", "bypass_actors": [],
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "rules": [
            {"type": "deletion"}, {"type": "non_fast_forward"},
            {"type": "pull_request", "parameters": {"dismiss_stale_reviews_on_push": true, "require_code_owner_review": false, "require_extra_approval_for_unattributed_changes": true, "require_last_push_approval": false, "required_approving_review_count": 1, "required_review_thread_resolution": true, "required_reviewers": [], "allowed_merge_methods": ["squash", "merge", "rebase"]}},
            {"type": "required_status_checks", "parameters": {"do_not_enforce_on_create": false, "strict_required_status_checks_policy": true, "required_status_checks": [{"context": REQUIRED_CHECK}]}}
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn policy_change_in_candidate_is_not_trusted_by_base_workflow() {
        let policy = GithubPolicy::standard("example/widget".into());
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("bonsai.toml"), "[github]\nprofile = \"standard-v1\"\nrepository = \"attacker/repo\"\nbonsai_source = \"github:Quitetall/bonsai\"\nbonsai_revision = \"97c29b5a1994a727ed566b556c26165b3f80c11d\"\n").unwrap();
        assert_eq!(
            candidate_policy_drift(dir.path(), &policy).as_deref(),
            Some("github-policy-drift")
        );
    }

    #[test]
    fn standard_ruleset_payload_has_only_safe_baseline_controls() {
        let payload = standard_ruleset_payload();
        assert_eq!(payload["name"], RULESET_NAME);
        assert_eq!(payload["bypass_actors"], json!([]));
        assert_eq!(
            payload["rules"][2]["parameters"]["required_approving_review_count"],
            1
        );
        assert_eq!(
            payload["rules"][3]["parameters"]["required_status_checks"][0]["context"],
            REQUIRED_CHECK
        );
    }

    #[test]
    fn apply_refuses_to_replace_stronger_or_extended_ruleset() {
        let stronger = json!({"rules": [{"type": "pull_request", "parameters": {"required_approving_review_count": 2}}]});
        let extended = json!({"rules": [{"type": "required_signatures"}]});
        let scoped_reviewers = json!({"rules": [{"type": "pull_request", "parameters": {"required_reviewers": [{"reviewer": "@org/team"}]}}]});
        let check_binding = json!({"rules": [{"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": REQUIRED_CHECK, "integration_id": 42}]}}]});
        let all_branches = json!({"rules": [], "conditions": {}});
        assert!(!safe_to_replace_ruleset(&stronger));
        assert!(!safe_to_replace_ruleset(&extended));
        assert!(!safe_to_replace_ruleset(&scoped_reviewers));
        assert!(!safe_to_replace_ruleset(&check_binding));
        assert!(!safe_to_replace_ruleset(&all_branches));
        assert!(safe_to_replace_ruleset(
            &json!({"conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}}, "rules": [{"type": "pull_request", "parameters": {"required_approving_review_count": 0}}]})
        ));
    }
}
