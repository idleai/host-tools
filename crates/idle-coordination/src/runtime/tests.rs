use super::{RuntimeInvitation, framing};
use crate::{
    Error,
    invitation::{RelayEndpoint, Secret},
    transport::RelayDescriptor,
};

fn invitation() -> RuntimeInvitation {
    RuntimeInvitation {
        version: 1,
        host_id: "host-one".into(),
        workspace_id: "workspace-one".into(),
        repository_id: "repository-one".into(),
        checkout_id: "checkout-one".into(),
        chain_id: "chain-one".into(),
        client_id: "client-one".into(),
        grant_id: "grant-one".into(),
        grant_token: Secret("a".repeat(64)),
        expires_at: 10_000,
        coordination_owner: false,
        relay: RelayDescriptor {
            endpoint: RelayEndpoint {
                tunnel_id: "tunnel-one".into(),
                cluster_id: "use2".into(),
                host_id: "relay-one".into(),
                client_relay_uri: "wss://use2.rel.tunnels.api.visualstudio.com/client".into(),
                host_public_keys: vec!["YWJj".into()],
            },
            connect_token: Secret("relay-credential".into()),
            expires_at: 9_000,
        },
    }
}

#[test]
fn runtime_invitation_keeps_history_and_runtime_credentials_separate() {
    let value = invitation();
    let encoded = value.encode(1).expect("valid invitation");
    let decoded = RuntimeInvitation::parse(&encoded.0, 1).expect("valid encoded invitation");
    assert_eq!(
        decoded.grant_id, value.grant_id,
        "grant identity must round trip"
    );
    assert_eq!(
        decoded.grant_token, value.grant_token,
        "the runtime secret must round trip privately"
    );
    assert!(
        !format!("{decoded:?}").contains("relay-credential"),
        "debug output must redact credentials"
    );
    assert!(
        matches!(
            RuntimeInvitation::parse(&encoded.0, 10_000),
            Err(Error::Expired)
        ),
        "expired invitations must be rejected"
    );
    assert!(
        RuntimeInvitation::parse("editchain:abc", 1).is_err(),
        "history invitations cannot grant runtime access"
    );
}

#[test]
fn runtime_routes_reject_untrusted_servers() {
    let mut value = invitation();
    value.relay.endpoint.client_relay_uri = "wss://example.com/steal".into();
    assert!(
        value.encode(1).is_err(),
        "runtime credentials must only go to the validated relay"
    );
}

#[test]
fn coordination_ownership_requires_a_new_invitation_version() {
    let mut value = invitation();
    value.coordination_owner = true;
    assert!(
        value.encode(1).is_err(),
        "an old invitation cannot gain owner permission"
    );
    value.version = 2;
    let encoded = value.encode(1).expect("explicit owner invitation");
    assert!(
        RuntimeInvitation::parse(&encoded.0, 1)
            .expect("new owner invitation")
            .coordination_owner,
        "new clients retain the explicit permission"
    );
}

#[tokio::test]
async fn runtime_frames_reject_oversized_payloads_before_reading_them() {
    use tokio::io::AsyncWriteExt as _;
    let (mut input, mut output) = tokio::io::duplex(16);
    output
        .write_all(&1_048_577_u32.to_be_bytes())
        .await
        .expect("write header");
    assert!(
        framing::read::<serde_json::Value>(&mut input)
            .await
            .is_err(),
        "oversized frames must fail before allocation"
    );
}
