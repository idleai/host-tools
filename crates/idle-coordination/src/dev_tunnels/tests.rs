use std::sync::Arc;

use tunnels::contracts::Tunnel;

use crate::{Error, invitation::HostLease, persistence::FilePersistence};

use super::{
    close_result,
    journal::{Entry, Journal},
    owned,
};

fn lease() -> HostLease {
    HostLease {
        marker: "idle-relay-0123456789abcdef01234567".into(),
        tunnel_id: "test-tunnel".into(),
        cluster_id: "test".into(),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("SDK session closed: {0}")]
struct SdkFailure(#[from] russh::Error);

#[test]
fn closing_an_ended_sdk_session_is_idempotent_without_hiding_other_failures() {
    assert_eq!(
        close_result(Ok::<(), SdkFailure>(())),
        Ok(()),
        "confirmed disconnect must succeed"
    );
    assert_eq!(
        close_result(Err(SdkFailure(russh::Error::SendError))),
        Ok(()),
        "a closed command receiver already ended the session"
    );
    assert_eq!(
        close_result(Err(SdkFailure(russh::Error::NotAuthenticated))),
        Err(Error::Transport),
        "other SDK failures must remain visible"
    );
}

#[test]
fn uncertain_cloud_ownership_survives_reopen_and_cannot_change_resource() {
    let root = tempfile::tempdir().expect("private test directory");
    let storage =
        Arc::new(FilePersistence::open(root.path().join("state")).expect("private storage"));
    let journal = Journal::new(storage.clone());
    let owned_lease = lease();
    let mut entry = Entry {
        marker: owned_lease.marker.clone(),
        lease: None,
        created_at: 1,
    };
    journal
        .remember(&entry)
        .expect("record marker before cloud creation");
    drop(journal);
    let reopened = Journal::new(storage);
    assert!(
        reopened
            .entries()
            .expect("recover journal")
            .first()
            .is_some_and(|entry| entry.lease.is_none()),
        "an uncertain create must retain its exact owner marker"
    );
    entry.lease = Some(owned_lease.clone());
    reopened
        .remember(&entry)
        .expect("reconcile the allocated resource");
    entry.lease.as_mut().expect("known resource").tunnel_id = "different-resource".into();
    assert_eq!(
        reopened.remember(&entry),
        Err(Error::Conflict),
        "one marker cannot silently change cloud resources"
    );
    reopened
        .forget(&owned_lease.marker)
        .expect("confirmed cleanup retires its marker");
    assert!(
        reopened.entries().expect("read retired journal").is_empty(),
        "completed cleanup must not be resurrected"
    );
}

#[test]
fn cleanup_requires_both_exact_resource_identity_and_owner_marker() {
    let lease = lease();
    let mut tunnel = Tunnel {
        tunnel_id: Some(lease.tunnel_id.clone()),
        cluster_id: Some(lease.cluster_id.clone()),
        labels: vec![lease.marker.clone()],
        ..Tunnel::default()
    };
    assert_eq!(
        owned(&tunnel, &lease),
        Ok(()),
        "saved owned resources may resume"
    );
    tunnel.labels.clear();
    assert_eq!(
        owned(&tunnel, &lease),
        Err(Error::Forbidden),
        "matching IDs without the owner marker cannot authorize deletion"
    );
    tunnel.labels.push(lease.marker.clone());
    tunnel.tunnel_id = Some("unrelated".into());
    assert_eq!(
        owned(&tunnel, &lease),
        Err(Error::Forbidden),
        "a matching label cannot substitute for saved resource identity"
    );
}

#[tokio::test]
async fn imported_marker_keeps_its_retained_host_without_requesting_credentials() {
    use super::{DevTunnels, EnvironmentCredentials};
    use crate::{clock::SystemClock, transport::RelayProvider as _};
    let root = tempfile::tempdir().expect("private test directory");
    let storage =
        Arc::new(FilePersistence::open(root.path().join("state")).expect("private storage"));
    let relay = DevTunnels::new(
        Arc::new(EnvironmentCredentials {
            variable: "UNUSED_IMPORTED_MARKER_TOKEN".into(),
        }),
        storage.clone(),
        Arc::new(SystemClock),
    )
    .expect("SDK adapter");
    let retained = lease();
    relay
        .import_cleanup(std::slice::from_ref(&retained.marker))
        .expect("durable import");
    relay
        .cleanup(Some(&retained), &tokio_util::sync::CancellationToken::new())
        .await
        .expect("retained marker must be excluded before management access");
    let entries = Journal::new(storage)
        .entries()
        .expect("journal remains durable");
    assert_eq!(
        entries.len(),
        1,
        "retained ownership must survive migration"
    );
    assert_eq!(
        relay.import_cleanup(&["unrelated-marker".into()]),
        Err(Error::Invalid),
        "import cannot invent an owner label"
    );
}

#[test]
fn marker_only_cleanup_refuses_ambiguous_or_unrelated_resources() {
    let lease = lease();
    let tunnel = Tunnel {
        tunnel_id: Some(lease.tunnel_id.clone()),
        cluster_id: Some(lease.cluster_id.clone()),
        labels: vec![lease.marker.clone()],
        ..Tunnel::default()
    };
    assert_eq!(
        super::cleanup_target(std::slice::from_ref(&tunnel), &lease.marker),
        Ok(Some(lease.clone())),
        "a single labeled resource can be reconciled"
    );
    assert_eq!(
        super::cleanup_target(&[tunnel.clone(), tunnel], &lease.marker),
        Err(Error::Conflict),
        "ambiguous discovery must not delete any resource"
    );
    assert_eq!(
        super::cleanup_target(&[Tunnel::default()], &lease.marker),
        Err(Error::Forbidden),
        "unrelated cloud results never authorize deletion"
    );
}
