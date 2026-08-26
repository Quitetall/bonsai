use bonsai::github::{evaluate, GithubPolicy, Observation, Profile, RulesetObservation};

fn standard_policy() -> GithubPolicy {
    GithubPolicy::standard("example/widget".into())
}

fn compliant() -> Observation {
    Observation {
        repository: "example/widget".into(),
        default_branch: "main".into(),
        actions_enabled: true,
        sha_pinning_required: true,
        dependabot_security_updates: Some(true),
        secret_scanning: Some(true),
        push_protection: Some(true),
        ruleset_visibility_complete: true,
        rulesets: vec![RulesetObservation {
            name: "bonsai-standard-v1".into(),
            enforcement: "active".into(),
            targets_default_branch: true,
            bypass_actors: 0,
            blocks_deletion: true,
            blocks_non_fast_forward: true,
            pull_request_required: true,
            approvals: 1,
            dismisses_stale_reviews: true,
            resolves_threads: true,
            extra_approval_for_unattributed_changes: true,
            strict_required_checks: true,
            required_checks: vec!["bonsai compliance".into()],
        }],
    }
}

#[test]
fn standard_profile_accepts_complete_effective_protection() {
    let report = evaluate(&standard_policy(), &compliant());
    assert!(report.is_pass(), "{report:?}");
}

#[test]
fn standard_profile_rejects_bypass_and_missing_required_check() {
    let mut observation = compliant();
    observation.rulesets[0].bypass_actors = 1;
    observation.rulesets[0].required_checks.clear();
    let report = evaluate(&standard_policy(), &observation);
    assert!(report.rules.iter().any(|rule| rule == "github-no-bypass"));
    assert!(report
        .rules
        .iter()
        .any(|rule| rule == "github-required-check"));
}

#[test]
fn unknown_security_observation_never_claims_compliance() {
    let mut observation = compliant();
    observation.secret_scanning = None;
    let report = evaluate(&standard_policy(), &observation);
    assert!(report.is_unknown());
}

#[test]
fn incomplete_ruleset_visibility_never_claims_compliance() {
    let mut observation = compliant();
    observation.ruleset_visibility_complete = false;
    let report = evaluate(&standard_policy(), &observation);
    assert!(report.is_unknown());
    assert!(report
        .rules
        .iter()
        .any(|rule| rule == "github-ruleset-visibility-unknown"));
}

#[test]
fn only_standard_or_governed_profile_names_are_accepted() {
    assert_eq!(Profile::parse("standard-v1").unwrap(), Profile::StandardV1);
    assert_eq!(Profile::parse("governed-v1").unwrap(), Profile::GovernedV1);
    assert!(Profile::parse("minimal").is_err());
}

#[test]
fn policy_rejects_toml_injection_and_invalid_codeowners() {
    let mut policy = standard_policy();
    policy.repository = "example/widget\n[evil]".into();
    assert!(policy.validate().is_err());

    let mut policy = standard_policy();
    policy.codeowners = vec!["@team\n[evil]".into()];
    assert!(policy.validate().is_err());
}
