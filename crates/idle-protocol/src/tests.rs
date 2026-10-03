use std::cmp::Ordering;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::v1::{
    ApiVersion,
    api::{
        ApiResult, Command, CommandResult, Query, QueryResult, Request, Response, RuntimeReport,
        RuntimeSubmission, WireMessage,
    },
    events::{Event, RecoveryCursor},
    identity::{ControlEpoch, EventPosition, InputOrder, InputRevision, Revision, Timestamp},
    sessions::{RuntimeInputState, RuntimeInputUpdate},
};

const INPUT: &str = include_str!("../tests/fixtures/submit_input.json");
const RECEIPT: &str = include_str!("../tests/fixtures/backend_receipt.json");
const ACCEPTED: &str = include_str!("../tests/fixtures/runtime_accepted.json");
const COMPLETED: &str = include_str!("../tests/fixtures/runtime_completed.json");
const STANDALONE: &str = include_str!("../tests/fixtures/standalone_snapshot.json");
const MANAGED: &str = include_str!("../tests/fixtures/managed_snapshot.json");
const FORWARDED: &str = include_str!("../tests/fixtures/runtime_submission.json");

fn fixture<T: DeserializeOwned + Serialize>(source: &str) -> T {
    let decoded: T =
        serde_json::from_str(source).expect("fixture must decode as its endpoint type");
    let expected: Value = serde_json::from_str(source).expect("fixture must contain valid JSON");
    assert_eq!(
        serde_json::to_value(&decoded).expect("fixture must serialize"),
        expected,
        "wire fields, tags, numeric representations and optional values must remain stable"
    );
    decoded
}

#[test]
fn published_wire_fixtures_round_trip() {
    drop(fixture::<Request<Command>>(INPUT));
    drop(fixture::<Response<CommandResult>>(RECEIPT));
    drop(fixture::<RuntimeSubmission>(FORWARDED));
    drop(fixture::<RuntimeReport>(ACCEPTED));
    drop(fixture::<RuntimeReport>(COMPLETED));
    drop(fixture::<Response<QueryResult>>(STANDALONE));
    drop(fixture::<Response<QueryResult>>(MANAGED));
    drop(fixture::<Event>(include_str!(
        "../tests/fixtures/grant_revoked.json"
    )));
    drop(fixture::<Event>(include_str!(
        "../tests/fixtures/control_released.json"
    )));
    drop(fixture::<Request<Command>>(include_str!(
        "../tests/fixtures/control_acquire.json"
    )));
    drop(fixture::<Request<Query>>(include_str!(
        "../tests/fixtures/catch_up.json"
    )));
    drop(fixture::<Response<QueryResult>>(include_str!(
        "../tests/fixtures/event_page.json"
    )));
    drop(fixture::<Response<QueryResult>>(include_str!(
        "../tests/fixtures/snapshot_required.json"
    )));
    drop(fixture::<Response<CommandResult>>(include_str!(
        "../tests/fixtures/idempotency_conflict.json"
    )));
}

#[test]
fn runtime_forwarding_preserves_the_retry_context_and_original_input() {
    let request = fixture::<Request<Command>>(INPUT);
    let forwarded = fixture::<RuntimeSubmission>(FORWARDED);
    assert_eq!(
        forwarded.context, request.context,
        "forwarding preserves contributor, deadline and key"
    );
    assert_eq!(
        forwarded.control_fence, request.control_fence,
        "forwarding preserves fencing"
    );
    assert_eq!(
        Command::SubmitInput(forwarded.body.clone()),
        request.body,
        "runtime receives the exact original input"
    );
    let receipt = forwarded
        .coordination_receipt
        .as_ref()
        .expect("forwarded fixture has a durable receipt");
    assert_eq!(
        receipt.request,
        forwarded.context.key(),
        "receipt refers to the same logical submission"
    );
    let direct = RuntimeSubmission {
        coordination_receipt: None,
        ..forwarded
    };
    let wire = serde_json::to_string(&direct).expect("direct submission serializes");
    assert_eq!(
        serde_json::from_str::<RuntimeSubmission>(&wire).expect("direct submission decodes"),
        direct,
        "standalone direct submissions require no managed receipt"
    );
}

#[test]
fn retries_preserve_attribution_independently_of_owner_and_operation() {
    let original = fixture::<Request<Command>>(INPUT);
    let retry: Request<Command> = serde_json::from_str(INPUT).expect("retry decodes");
    assert_eq!(
        original, retry,
        "a retry preserves the entire original request"
    );
    let key = original.context.key();
    assert_eq!(
        key.contributor_id.0, "contributor-bob",
        "submitter is Bob, not owner Alice"
    );

    let mut changed_body = retry.clone();
    changed_body.body = Command::SubmitInput(crate::v1::sessions::SubmitInput {
        session_id: "session-shared".into(),
        text: "different work".into(),
    });
    assert_eq!(
        changed_body.context.key(),
        key,
        "payload changes must conflict within the same key"
    );
    assert_ne!(
        changed_body, original,
        "the authority can compare semantic request content"
    );

    let mut other_contributor = retry.clone();
    other_contributor.context.contributor.contributor_id = "contributor-alice".into();
    assert_ne!(
        other_contributor.context.key(),
        key,
        "contributors have distinct retry namespaces"
    );
    let mut other_workspace = retry;
    other_workspace.context.workspace_id = "workspace-other".into();
    assert_ne!(
        other_workspace.context.key(),
        key,
        "workspaces have distinct retry namespaces"
    );
}

#[test]
fn coordination_receipt_cannot_be_mistaken_for_runtime_acceptance() {
    let receipt = fixture::<Response<CommandResult>>(RECEIPT);
    assert!(
        matches!(
            receipt.result,
            ApiResult::Success(CommandResult::Received(_))
        ),
        "receipt has an explicit coordination stage"
    );
    assert!(
        serde_json::from_str::<Response<RuntimeInputUpdate>>(RECEIPT).is_err(),
        "a receipt cannot decode as a runtime status response"
    );
    assert!(
        serde_json::from_str::<RuntimeReport>(RECEIPT).is_err(),
        "a receipt cannot decode as a runtime report"
    );

    let accepted = fixture::<RuntimeReport>(ACCEPTED);
    assert!(
        matches!(accepted.update.state, RuntimeInputState::Accepted { .. }),
        "acceptance carries no delivery order or completion"
    );
    let completed = fixture::<RuntimeReport>(COMPLETED);
    assert!(
        matches!(completed.update.state, RuntimeInputState::Completed { .. }),
        "completion is explicitly runtime-authored"
    );
    assert_eq!(
        accepted.update.input, completed.update.input,
        "reconnect preserves the original input identity"
    );
    assert_eq!(
        accepted.update.contributor, completed.update.contributor,
        "completion preserves the real contributor"
    );
    assert!(
        completed.update.revision > accepted.update.revision,
        "runtime revisions distinguish delayed confirmations"
    );
}

#[test]
fn session_and_compute_permissions_cannot_cross_decode() {
    let session =
        json!({"kind":"session", "session_id":"session-shared", "permissions":["submit_input"]});
    assert!(
        serde_json::from_value::<crate::v1::grants::GrantScope>(session).is_ok(),
        "session input permission is supported"
    );
    let crossed =
        json!({"kind":"compute", "host_id":"host-shared", "permissions":["submit_input"]});
    assert!(
        serde_json::from_value::<crate::v1::grants::GrantScope>(crossed).is_err(),
        "session permission cannot become a compute grant"
    );
    let host_as_session =
        json!({"kind":"session", "session_id":"session-shared", "permissions":["execute"]});
    assert!(
        serde_json::from_value::<crate::v1::grants::GrantScope>(host_as_session).is_err(),
        "session participation cannot carry a host execution permission"
    );
}

#[test]
fn cursors_are_ordered_only_within_workspace_audience_and_generation() {
    let base = RecoveryCursor {
        workspace_id: "workspace-one".into(),
        contributor_id: "contributor-bob".into(),
        stream_id: "stream-standalone-bob-1".into(),
        position: EventPosition(8),
    };
    let later = RecoveryCursor {
        position: EventPosition(u64::MAX),
        ..base.clone()
    };
    assert_eq!(
        base.compare_position(&later),
        Some(Ordering::Less),
        "same-stream positions are ordered losslessly"
    );
    assert_eq!(
        later.compare_position(&base),
        Some(Ordering::Greater),
        "late delivery cannot appear newer"
    );
    assert_eq!(
        base.compare_position(&base),
        Some(Ordering::Equal),
        "replayed positions are equal"
    );
    let changed = [
        RecoveryCursor {
            workspace_id: "workspace-other".into(),
            ..later.clone()
        },
        RecoveryCursor {
            contributor_id: "contributor-alice".into(),
            ..later.clone()
        },
        RecoveryCursor {
            stream_id: "stream-managed-bob-1".into(),
            ..later
        },
    ];
    for cursor in changed {
        assert_eq!(
            base.compare_position(&cursor),
            None,
            "different scopes must not compare by numeric position"
        );
    }
}

#[test]
fn all_unsigned_wire_counters_preserve_the_full_u64_range() {
    let maximum = json!("18446744073709551615");
    assert_eq!(
        serde_json::to_value(ControlEpoch(u64::MAX)).expect("epoch serializes"),
        maximum,
        "epoch is not rounded through a JSON number"
    );
    assert_eq!(
        serde_json::to_value(EventPosition(u64::MAX)).expect("position serializes"),
        maximum,
        "cursor is lossless"
    );
    assert_eq!(
        serde_json::to_value(InputOrder(u64::MAX)).expect("order serializes"),
        maximum,
        "input order is lossless"
    );
    assert_eq!(
        serde_json::to_value(InputRevision(u64::MAX)).expect("input revision serializes"),
        maximum,
        "input revision is lossless"
    );
    assert_eq!(
        serde_json::to_value(Revision(u64::MAX)).expect("revision serializes"),
        maximum,
        "metadata revision is lossless"
    );
    assert_eq!(
        serde_json::to_value(Timestamp(u64::MAX)).expect("timestamp serializes"),
        maximum,
        "timestamp is lossless"
    );
    assert_eq!(
        serde_json::from_value::<ControlEpoch>(maximum).expect("epoch decodes"),
        ControlEpoch(u64::MAX),
        "full range round trips"
    );
}

#[test]
fn counters_reject_noncanonical_or_lossy_wire_values() {
    for invalid in [
        json!(1),
        json!(-1),
        json!(1.5),
        json!(""),
        json!("01"),
        json!("+1"),
        json!("-1"),
        json!("1.0"),
        json!(" 1"),
        json!("18446744073709551616"),
    ] {
        assert!(
            serde_json::from_value::<EventPosition>(invalid.clone()).is_err(),
            "invalid position must fail: {invalid}"
        );
        assert!(
            serde_json::from_value::<ControlEpoch>(invalid.clone()).is_err(),
            "invalid epoch must fail: {invalid}"
        );
    }
    assert_eq!(
        serde_json::from_value::<ControlEpoch>(json!("0")).expect("initial epoch decodes"),
        ControlEpoch(0),
        "zero is the durable unassigned watermark"
    );
}

#[test]
fn unsupported_or_missing_versions_and_unknown_variants_fail_closed() {
    for version in [json!("2"), json!(1), Value::Null] {
        let mut request: Value = serde_json::from_str(INPUT).expect("fixture is JSON");
        *request.get_mut("api_version").expect("fixture has version") = version;
        assert!(
            serde_json::from_value::<Request<Command>>(request).is_err(),
            "a different or missing version must not be interpreted as v1"
        );
    }
    let mut missing: Value = serde_json::from_str(INPUT).expect("fixture is JSON");
    assert_eq!(
        missing
            .as_object_mut()
            .expect("fixture is object")
            .remove("api_version"),
        Some(json!("1")),
        "remove required version"
    );
    assert!(
        serde_json::from_value::<Request<Command>>(missing).is_err(),
        "no implicit default protocol version"
    );

    let mut unknown: Value = serde_json::from_str(INPUT).expect("fixture is JSON");
    *unknown
        .pointer_mut("/body/kind")
        .expect("command tag exists") = json!("future_command");
    assert!(
        serde_json::from_value::<Request<Command>>(unknown).is_err(),
        "unknown command must not become a no-op success"
    );
    let mut event: Value =
        serde_json::from_str(include_str!("../tests/fixtures/grant_revoked.json"))
            .expect("event fixture is JSON");
    *event.pointer_mut("/body/kind").expect("event tag exists") = json!("future_event");
    assert!(
        serde_json::from_value::<Event>(event).is_err(),
        "unknown event cannot silently advance recovery"
    );
}

#[test]
fn optional_extensions_are_compatible_without_changing_the_protocol_version() {
    let mut request: Value = serde_json::from_str(INPUT).expect("fixture is JSON");
    let previous = request.as_object_mut().expect("request is object").insert(
        "description_hint".into(),
        json!("optional presentation metadata"),
    );
    assert!(
        previous.is_none(),
        "fixture did not already define extension"
    );
    let decoded: Request<Command> =
        serde_json::from_value(request).expect("optional unknown field is tolerated");
    assert_eq!(
        decoded.api_version,
        ApiVersion::V1,
        "extension does not change the major version"
    );
}

#[test]
fn schema_union_covers_all_published_top_level_shapes() {
    for source in [
        INPUT,
        RECEIPT,
        ACCEPTED,
        COMPLETED,
        STANDALONE,
        MANAGED,
        include_str!("../tests/fixtures/grant_revoked.json"),
        include_str!("../tests/fixtures/catch_up.json"),
    ] {
        drop(fixture::<WireMessage>(source));
    }
}

#[cfg(feature = "schema")]
#[test]
fn checked_in_schema_matches_the_public_rust_contracts() {
    let published: Value =
        serde_json::from_str(include_str!("../schemas/v1.json")).expect("published schema is JSON");
    let generated =
        serde_json::to_value(schemars::schema_for!(WireMessage)).expect("schema serializes");
    assert_eq!(
        generated, published,
        "wire schema changed: review compatibility, then run the documented schema exporter"
    );
}
