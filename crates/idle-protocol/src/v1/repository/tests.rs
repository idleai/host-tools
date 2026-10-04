use super::RepositorySnapshot;

#[test]
fn repository_fixture_retains_full_sources_and_validates_selection_boundaries() {
    let raw = include_str!("../../../tests/fixtures/repository_snapshot.json");
    let mut snapshot: RepositorySnapshot = serde_json::from_str(raw).expect("snapshot fixture");
    snapshot.validate().expect("valid complete sources");
    assert_eq!(
        serde_json::to_value(&snapshot).expect("snapshot encoding"),
        serde_json::from_str::<serde_json::Value>(raw).expect("fixture JSON")
    );
    snapshot
        .sessions
        .first_mut()
        .expect("session")
        .records
        .first_mut()
        .expect("record")
        .item = "12".repeat(32);
    assert!(
        snapshot.validate().is_err(),
        "foreign items cannot enter recorded session selection"
    );
}

#[test]
fn github_links_and_duplicate_session_observations_are_rejected() {
    let raw = include_str!("../../../tests/fixtures/repository_snapshot.json");
    let mut snapshot: RepositorySnapshot = serde_json::from_str(raw).expect("fixture");
    let session = snapshot.sessions.first_mut().expect("session");
    session
        .records
        .push(session.records.first().expect("record").clone());
    assert!(
        snapshot.validate().is_err(),
        "observations have one exact session owner"
    );
    assert!(!super::validation::github_url(
        "https://github.com@invalid.example/repo"
    ));
    assert!(!super::validation::github_url(
        "https://github.com/owner/repo\n"
    ));
}

#[test]
#[cfg(feature = "schema")]
fn repository_schema_matches_the_published_contract() {
    let published: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/repository-v1.json"))
            .expect("published schema");
    assert_eq!(
        serde_json::to_value(schemars::schema_for!(RepositorySnapshot)).expect("generated schema"),
        published,
        "repository schema remains current"
    );
}
