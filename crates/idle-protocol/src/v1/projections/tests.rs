use serde_json::{Value, json};

use super::{ProjectionAvailability, ProjectionReference, ProjectionSnapshot};
use crate::v1::identity::ProjectionCount;

const FIXTURE: &str = include_str!("../../../tests/fixtures/projection_snapshot.json");

fn snapshot() -> ProjectionSnapshot {
    serde_json::from_str(FIXTURE).expect("projection fixture")
}

#[test]
fn published_projection_fixture_round_trips_all_destinations_and_full_references() {
    let input = snapshot();
    input.validate().expect("valid fixture");
    assert_eq!(
        serde_json::to_value(input).expect("serialize"),
        serde_json::from_str::<Value>(FIXTURE).expect("fixture JSON"),
        "shared projection inputs round trip exactly"
    );
    let mut json = serde_json::from_str::<Value>(FIXTURE).expect("fixture JSON");
    *json.get_mut("version").expect("version field") = json!("2");
    assert!(
        serde_json::from_value::<ProjectionSnapshot>(json).is_err(),
        "unknown input versions cannot be interpreted"
    );
}

#[test]
fn references_reject_prefixes_uppercase_empty_and_unbound_digests() {
    for reference in [
        ProjectionReference {
            observation: Some("ab12".into()),
            item: None,
            record_hash: None,
        },
        ProjectionReference {
            observation: Some("AB".repeat(32)),
            item: None,
            record_hash: None,
        },
        ProjectionReference {
            observation: None,
            item: None,
            record_hash: None,
        },
        ProjectionReference {
            observation: None,
            item: Some("ab".repeat(32)),
            record_hash: Some("cd".repeat(32)),
        },
    ] {
        assert!(
            reference.validate().is_err(),
            "invalid full history reference must fail"
        );
    }
}

#[test]
fn atomic_input_rejects_duplicate_destinations_keys_and_inconsistent_counts() {
    let mut missing = snapshot();
    drop(missing.inputs.pop());
    assert!(missing.validate().is_err(), "all destinations are required");
    let mut duplicate = snapshot();
    duplicate.inputs.last_mut().expect("destination").kind = super::ProjectionKind::Activity;
    assert!(
        duplicate.validate().is_err(),
        "duplicate destination cannot masquerade as full snapshot"
    );
    let mut count = snapshot();
    count.inputs.first_mut().expect("input").total = Some(ProjectionCount(2));
    assert!(
        count.validate().is_err(),
        "complete result must have exact total"
    );
    let mut keys = snapshot();
    let list = keys.inputs.first_mut().expect("input");
    list.rows.push(list.rows.first().expect("row").clone());
    list.total = Some(ProjectionCount(2));
    assert!(keys.validate().is_err(), "duplicate row keys are invalid");
    let mut unavailable = snapshot();
    unavailable.inputs.first_mut().expect("input").availability =
        ProjectionAvailability::Unavailable;
    assert!(
        unavailable.validate().is_err(),
        "unavailable inputs cannot claim data"
    );
    let mut item_only = snapshot();
    let source = item_only
        .inputs
        .first_mut()
        .expect("input")
        .rows
        .first_mut()
        .expect("row")
        .sources
        .first_mut()
        .expect("source");
    source.observation = None;
    source.record_hash = None;
    assert!(
        item_only.validate().is_err(),
        "a logical item alone cannot identify the record behind a row"
    );
}

#[test]
fn partial_counts_preserve_the_full_u64_range_without_json_rounding() {
    let mut input = snapshot();
    let list = input.inputs.first_mut().expect("input");
    list.availability = ProjectionAvailability::Partial;
    list.total = Some(ProjectionCount(u64::MAX));
    list.gaps.push(super::ProjectionGap {
        reference: None,
        message: "Bounded result".into(),
    });
    input.validate().expect("partial count");
    let json = serde_json::to_value(&input).expect("JSON");
    assert_eq!(
        json.pointer("/inputs/0/total").and_then(Value::as_str),
        Some(u64::MAX.to_string().as_str()),
        "full count is a decimal string"
    );
    assert_eq!(
        serde_json::from_value::<ProjectionSnapshot>(json).expect("decode"),
        input,
        "no rounded counters"
    );
}

#[cfg(feature = "schema")]
#[test]
fn checked_in_projection_schema_matches_shared_contract() {
    let published: Value =
        serde_json::from_str(include_str!("../../../schemas/projections-v1.json"))
            .expect("published schema");
    assert_eq!(
        serde_json::to_value(schemars::schema_for!(ProjectionSnapshot)).expect("generated schema"),
        published,
        "regenerate the projection schema after reviewing compatibility"
    );
}
