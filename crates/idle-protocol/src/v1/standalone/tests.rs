use serde_json::{Value, json};

use super::{Mutation, RepositoryMessage};
use crate::v1::{
    api::Request,
    configuration::{ConfigurationDocument, ConfigurationWrite},
};

const SETTINGS: &str = include_str!("../../../tests/fixtures/configuration_write.json");

#[test]
fn configuration_preserves_f27_names_and_unknown_document_fields() {
    let expected: Value = serde_json::from_str(SETTINGS).expect("configuration fixture is JSON");
    let request: Request<Mutation> =
        serde_json::from_str(SETTINGS).expect("configuration mutation decodes");
    assert_eq!(
        serde_json::to_value(&request).expect("mutation serializes"),
        expected,
        "the existing envelope and configuration text must round trip exactly"
    );
    assert!(
        matches!(request.body, Mutation::Configuration(_)),
        "fixture must be a configuration mutation"
    );
    let write: ConfigurationWrite = serde_json::from_value(
        expected
            .pointer("/body/data")
            .cloned()
            .expect("configuration payload"),
    )
    .expect("f27 configuration body decodes independently");
    assert_eq!(
        write.document,
        ConfigurationDocument::Settings,
        "f27 uses the original PascalCase document names"
    );
    assert_eq!(
        write.change.value.json, "{\"future\":{\"enabled\":true},\"theme\":\"dark\"}",
        "unknown settings fields must survive unchanged"
    );
    let rules = ConfigurationWrite {
        document: ConfigurationDocument::AgentRules,
        ..write
    };
    assert_eq!(
        serde_json::to_value(rules)
            .expect("rules serialize")
            .get("document"),
        Some(&json!("AgentRules")),
        "rules have their own unchanged revision scope"
    );
    let union: RepositoryMessage =
        serde_json::from_str(SETTINGS).expect("schema union includes mutations");
    assert_eq!(
        serde_json::to_value(union).expect("union serializes"),
        expected,
        "schema union must not add a wrapper"
    );
}

#[test]
fn repository_mutations_require_known_versions_and_operations() {
    let source: Value = serde_json::from_str(SETTINGS).expect("configuration fixture is JSON");
    let mut unknown = source.clone();
    *unknown.pointer_mut("/body/kind").expect("mutation tag") = json!("future_mutation");
    assert!(
        serde_json::from_value::<Request<Mutation>>(unknown).is_err(),
        "unknown operations must fail closed"
    );
    let mut incompatible = source;
    *incompatible.get_mut("api_version").expect("version") = json!("2");
    assert!(
        serde_json::from_value::<Request<Mutation>>(incompatible).is_err(),
        "new major versions cannot enter v1 authority"
    );
}

#[cfg(feature = "schema")]
#[test]
fn standalone_schema_matches_the_public_repository_contracts() {
    let published: Value =
        serde_json::from_str(include_str!("../../../schemas/standalone-v1.json"))
            .expect("published schema is JSON");
    let generated =
        serde_json::to_value(schemars::schema_for!(RepositoryMessage)).expect("schema serializes");
    assert_eq!(
        generated, published,
        "review compatibility and run export_standalone_schema after contract changes"
    );
}
