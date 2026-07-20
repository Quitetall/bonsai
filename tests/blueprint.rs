use bonsai::blueprint::{Blueprint, BlueprintLock, ChangeClass, ShapeDiff, WitnessResult};

const CODEC: &str = r#"
[blueprint]
id = "codec.lossless.v1"
lifecycle = "locked"
description = "Typed lossless codec"

[[slot]]
id = "transform"
responsibility = "Reversible signal transform"
invariants = ["reversible"]

[[slot]]
id = "predict"
responsibility = "Predict and retain reconstruction state"
invariants = ["reversible"]

[[port]]
id = "raw"
slot = "transform"
direction = "input"
schema = "codec.Raw/v1"
compatibility = "permanent"

[[port]]
id = "transformed"
slot = "transform"
direction = "output"
schema = "codec.Transformed/v1"

[[port]]
id = "predict_in"
slot = "predict"
direction = "input"
schema = "codec.Transformed/v1"

[[port]]
id = "residuals"
slot = "predict"
direction = "output"
schema = "codec.Residuals/v1"
compatibility = "permanent"

[[flow]]
from = "transformed"
to = "predict_in"

[[implementation]]
id = "lifting"
slots = ["transform"]
bindings = ["src/stage.rs#LiftPass"]

[[implementation]]
id = "lpc"
slots = ["predict"]
bindings = ["src/stage.rs#PredictPass"]

[[witness]]
id = "byte-equal"
kind = "differential"
command = "cargo test byte_equal"
covers = ["transform", "predict"]
"#;

#[test]
fn locked_codec_blueprint_validates_and_has_stable_shape_digest() {
    let blueprint = Blueprint::from_toml(CODEC).expect("parse blueprint");
    assert_eq!(blueprint.validate(), Vec::<String>::new());
    assert_eq!(blueprint.meta.id, "codec.lossless.v1");
    assert!(blueprint.shape_digest().starts_with("b3:"));

    let mut reordered = blueprint.clone();
    reordered.slots.reverse();
    reordered.ports.reverse();
    reordered.flows.reverse();
    assert_eq!(blueprint.shape_digest(), reordered.shape_digest());

    reordered.meta.description = "Different prose".into();
    reordered.slots[0].responsibility = "Same contract, clearer wording".into();
    assert_eq!(blueprint.shape_digest(), reordered.shape_digest());
}

#[test]
fn implementation_variant_does_not_change_shape_identity() {
    let old = Blueprint::from_toml(CODEC).unwrap();
    let new = Blueprint::from_toml(&CODEC.replace(
        "id = \"lpc\"\nslots = [\"predict\"]\nbindings = [\"src/stage.rs#PredictPass\"]",
        "id = \"mv-rls\"\nslots = [\"predict\"]\nbindings = [\"src/mv_rls.rs#MvRlsPredictor\"]",
    ))
    .unwrap();

    let diff = ShapeDiff::between(&old, &new);
    assert_eq!(diff.class, ChangeClass::ImplementationOnly);
    assert_eq!(old.shape_digest(), new.shape_digest());
}

#[test]
fn flow_reorder_requires_new_shape_identity() {
    let old = Blueprint::from_toml(CODEC).unwrap();
    let changed = CODEC
        .replace(
            "direction = \"output\"\nschema = \"codec.Residuals/v1\"",
            "direction = \"input\"\nschema = \"codec.Residuals/v1\"",
        )
        .replace(
            "from = \"transformed\"\nto = \"predict_in\"",
            "from = \"transformed\"\nto = \"residuals\"",
        );
    let new = Blueprint::from_toml(&changed).unwrap();

    let diff = ShapeDiff::between(&old, &new);
    assert_eq!(diff.class, ChangeClass::NewShapeRequired);
    assert_ne!(old.shape_digest(), new.shape_digest());
}

#[test]
fn lock_records_permanent_ports_and_requires_passing_witnesses() {
    let blueprint = Blueprint::from_toml(CODEC).unwrap();
    let failed = BlueprintLock::create(
        &blueprint,
        &[WitnessResult::failed("byte-equal", "bytes differ")],
    );
    assert!(failed.is_err());

    let lock = BlueprintLock::create(&blueprint, &[WitnessResult::passed("byte-equal")])
        .expect("all declared witnesses passed");
    assert_eq!(lock.permanent_ports, vec!["raw", "residuals"]);
    assert_eq!(lock.check(&blueprint), Vec::<String>::new());
}

#[test]
fn dependency_cycle_needs_explicit_stateful_flow() {
    let cyclic = format!("{CODEC}\n[[flow]]\nfrom = \"residuals\"\nto = \"raw\"\n");
    let blueprint = Blueprint::from_toml(&cyclic).unwrap();
    let errors = blueprint.validate();
    assert!(
        errors.iter().any(|error| error.contains("stateful")),
        "cycle must require declared state: {errors:?}"
    );
}

#[test]
fn fused_implementation_requires_opt_in_and_differential_witness() {
    let mut blueprint = Blueprint::from_toml(CODEC).unwrap();
    blueprint
        .implementations
        .push(bonsai::blueprint::Implementation {
            id: "fused-codec".into(),
            slots: vec!["transform".into(), "predict".into()],
            bindings: vec!["src/fused.rs#FusedCodec".into()],
            variant_of: None,
        });
    assert!(blueprint
        .validate()
        .iter()
        .any(|error| error.contains("does not allow fusion")));

    for slot in &mut blueprint.slots {
        slot.allow_fusion = true;
    }
    blueprint.witnesses[0].covers.clear();
    assert!(blueprint
        .validate()
        .iter()
        .any(|error| error.contains("differential witness")));

    blueprint.witnesses[0].covers = vec!["transform".into(), "predict".into()];
    assert_eq!(blueprint.validate(), Vec::<String>::new());
}

#[test]
fn compatibility_adapters_must_be_direct_and_witnessed() {
    let chained = format!(
        "{CODEC}\n[[adapter]]\nid = \"raw-v0-to-v1\"\nfrom = \"raw-v0\"\nto = \"raw\"\nwitnesses = [\"byte-equal\"]\n\n[[adapter]]\nid = \"raw-v00-to-v0\"\nfrom = \"raw-v00\"\nto = \"raw-v0\"\nwitnesses = [\"byte-equal\"]\n"
    )
    .replace(
        "[[port]]\nid = \"raw\"",
        "[[port]]\nid = \"raw-v00\"\nslot = \"transform\"\ndirection = \"input\"\nschema = \"codec.Raw/v00\"\n\n[[port]]\nid = \"raw-v0\"\nslot = \"transform\"\ndirection = \"input\"\nschema = \"codec.Raw/v0\"\n\n[[port]]\nid = \"raw\"",
    );
    let blueprint = Blueprint::from_toml(&chained).unwrap();
    assert!(blueprint
        .validate()
        .iter()
        .any(|error| error.contains("adapter chain")));
}

#[test]
fn lock_detects_changed_witness_contract() {
    let mut blueprint = Blueprint::from_toml(CODEC).unwrap();
    let lock = BlueprintLock::create(&blueprint, &[WitnessResult::passed("byte-equal")]).unwrap();
    blueprint.witnesses.push(bonsai::blueprint::Witness {
        id: "abi-stable".into(),
        kind: bonsai::blueprint::WitnessKind::Abi,
        command: "cargo test abi".into(),
        covers: vec!["raw".into()],
    });
    assert!(lock
        .check(&blueprint)
        .iter()
        .any(|error| error.contains("witness contract changed")));

    let mut same_ids = Blueprint::from_toml(CODEC).unwrap();
    same_ids.witnesses[0].command = "true".into();
    assert!(lock
        .check(&same_ids)
        .iter()
        .any(|error| error.contains("witness contract changed")));
}

#[test]
fn evolution_preserves_every_historical_permanent_port() {
    let old = Blueprint::from_toml(CODEC).unwrap();
    let mut next = old.clone();
    next.meta.id = "codec.lossless.v2".into();
    next.meta.supersedes = Some(old.meta.id.clone());
    assert_eq!(
        Blueprint::validate_evolution(&old, &next),
        Vec::<String>::new()
    );

    next.ports.retain(|port| port.id != "raw");
    let errors = Blueprint::validate_evolution(&old, &next);
    assert!(errors
        .iter()
        .any(|error| error.contains("permanent port 'raw'")));

    let mut isolated = old.clone();
    isolated.meta.id = "codec.lossless.v2".into();
    isolated.meta.supersedes = Some(old.meta.id.clone());
    isolated.implementations.clear();
    assert!(Blueprint::validate_evolution(&old, &isolated)
        .iter()
        .any(|error| error.contains("no executable native or direct-adapter path")));
}

#[test]
fn evolution_must_name_the_shape_it_supersedes() {
    let old = Blueprint::from_toml(CODEC).unwrap();
    let mut next = old.clone();
    next.meta.id = "codec.lossless.v2".into();
    assert!(Blueprint::validate_evolution(&old, &next)
        .iter()
        .any(|error| error.contains("must supersede")));
}
