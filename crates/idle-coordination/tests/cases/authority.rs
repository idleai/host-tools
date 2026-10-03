use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex, atomic::Ordering},
};

use async_trait::async_trait;
use idle_coordination::{
    Error, Result,
    authority::{
        Authority, Principal,
        adoption::{AdoptionPackage, AdoptionReceipt, HistoryConsent, ManagedAdoption},
    },
    engine::ScopeChoice,
    persistence::{FilePersistence, Persistence},
};
use idle_protocol::v1::{
    Change, WriteCondition,
    api::{ApiResult, ErrorCode, Response},
    configuration::{ConfigurationDocument, ConfigurationValue, ConfigurationWrite},
    control::{ControlCommand, ControlHolder, ControlValidation},
    grants::{ComputePermission, GrantCommand, GrantScope, SessionPermission},
    identity::{ControlEpoch, Revision, Timestamp},
    membership::{Membership, MembershipStatus, Role},
    projections::ProjectionKind,
    resources::{Availability, ComputeHost, Health, HostCapability},
    sessions::{RuntimeBinding, Session, SessionKind},
    standalone::{
        AccessCheck, Mutation, MutationResult, Presence, RepositoryRecovery, ViewDefinition,
    },
    workspace::CoordinationMode,
};
use tokio_util::sync::CancellationToken;

use super::support::{self, Memory, NOW, TestClock, authority, principal, request};

fn configuration(
    document: ConfigurationDocument,
    expected: WriteCondition,
    json: &str,
) -> Mutation {
    Mutation::Configuration(ConfigurationWrite {
        document,
        change: Change {
            expected,
            value: ConfigurationValue {
                schema_version: 1,
                json: json.into(),
            },
        },
    })
}

fn success(response: Response<MutationResult>) -> MutationResult {
    match response.result {
        ApiResult::Success(value) => Some(value),
        ApiResult::Failure(_) => None,
    }
    .expect("expected a successful committed mutation")
}

fn refused(response: Response<MutationResult>, expected: ErrorCode) {
    assert!(
        matches!(response.result, ApiResult::Failure(error) if error.code == expected),
        "must return the specific domain refusal"
    );
}

fn member(authority: &mut Authority, owner: &Principal, name: &str, role: Role) -> Result<()> {
    let _result = success(authority.execute(
        owner,
        request(
            owner,
            &format!("member-{name}"),
            Mutation::Membership(Change {
                expected: WriteCondition::Absent,
                value: Membership {
                    contributor_id: name.into(),
                    role,
                    status: MembershipStatus::Active,
                },
            }),
        ),
    )?);
    Ok(())
}

fn register_host(authority: &mut Authority, owner: &Principal) -> Result<()> {
    let host = ComputeHost {
        id: "host".into(),
        owner: owner.contributor.contributor_id.clone(),
        name: "Host".into(),
        capabilities: vec![HostCapability::Sessions],
        health: Health {
            availability: Availability::Available,
            observed_at: Timestamp(NOW),
            valid_until: Timestamp(NOW.saturating_add(120_000)),
        },
        routes: Vec::new(),
    };
    let _result = success(authority.execute(
        owner,
        request(
            owner,
            "host",
            Mutation::Host(Change {
                expected: WriteCondition::Absent,
                value: host,
            }),
        ),
    )?);
    Ok(())
}

fn register_session(authority: &mut Authority, owner: &Principal) -> Result<ControlHolder> {
    let holder = ControlHolder {
        host_id: "host".into(),
        runtime_id: "runtime".into(),
        session_id: "session".into(),
    };
    let session = Session {
        id: holder.session_id.clone(),
        owner: owner.contributor.contributor_id.clone(),
        title: "Control".into(),
        kind: SessionKind::Control,
        runtime: RuntimeBinding {
            host_id: holder.host_id.clone(),
            runtime_id: holder.runtime_id.clone(),
        },
        parent: None,
    };
    let _result = success(authority.execute(
        owner,
        request(
            owner,
            "session",
            Mutation::Session(Change {
                expected: WriteCondition::Absent,
                value: session,
            }),
        ),
    )?);
    Ok(holder)
}

#[test]
fn conditional_documents_retry_identity_and_recovery_are_independent() -> support::TestResult {
    let memory = Arc::new(Memory::default());
    let clock = Arc::new(TestClock::default());
    let owner = principal("owner");
    let mut service = authority(memory, clock)?;
    let before = service.snapshot(&owner)?;
    let settings = request(
        &owner,
        "settings",
        configuration(
            ConfigurationDocument::Settings,
            WriteCondition::Absent,
            r#"{"unknown":{"keep":true}}"#,
        ),
    );
    let committed = service.execute(&owner, settings.clone())?;
    let _result = success(committed.clone());
    equal!(
        service.execute(&owner, settings.clone())?,
        committed,
        "unchanged retries must return the original complete response"
    )?;
    let mut changed = settings.clone();
    changed.body = configuration(
        ConfigurationDocument::AgentRules,
        WriteCondition::Absent,
        "{}",
    );
    refused(
        service.execute(&owner, changed)?,
        ErrorCode::IdempotencyConflict,
    );
    let stale = request(
        &owner,
        "stale",
        configuration(
            ConfigurationDocument::Settings,
            WriteCondition::Absent,
            "{}",
        ),
    );
    refused(
        service.execute(&owner, stale.clone())?,
        ErrorCode::StaleRevision,
    );
    let _rules = success(service.execute(
        &owner,
        request(
            &owner,
            "rules",
            configuration(
                ConfigurationDocument::AgentRules,
                WriteCondition::Absent,
                "{}",
            ),
        ),
    )?);
    let _view = success(service.execute(
        &owner,
        request(
            &owner,
            "view",
            Mutation::View(Change {
                expected: WriteCondition::Absent,
                value: ViewDefinition {
                    id: "board".into(),
                    title: "Board".into(),
                    kind: ProjectionKind::Task,
                    schema_version: 1,
                    json: "{}".into(),
                },
            }),
        ),
    )?);
    let after = service.snapshot(&owner)?;
    equal!(
        after.settings.as_ref().map(|record| record.revision),
        Some(Revision(1)),
        "rules must not change settings revisions"
    )?;
    equal!(
        after
            .settings
            .as_ref()
            .map(|record| record.value.json.as_str()),
        Some(r#"{"unknown":{"keep":true}}"#),
        "unknown configuration fields must survive"
    )?;
    equal!(
        after.agent_rules.as_ref().map(|record| record.revision),
        Some(Revision(1)),
        "documents must have independent revision scopes"
    )?;
    equal!(
        after.views.len(),
        1,
        "view definitions must be in the replacement snapshot"
    )?;
    ensure!(
        matches!(service.catch_up(&owner, &before.as_of, 2)?, RepositoryRecovery::Events { events, has_more: true, .. } if events.len() == 2),
        "recovery must page ordered commits, excluding refused writes"
    )?;
    refused(service.execute(&owner, stale)?, ErrorCode::StaleRevision);
    let mut forged = settings;
    forged.context.contributor.authenticated_as.subject = "another-device".into();
    refused(service.execute(&owner, forged)?, ErrorCode::Unauthenticated);
    Ok(())
}

#[test]
fn grants_membership_visibility_and_presence_remain_separate() -> support::TestResult {
    let clock = Arc::new(TestClock::default());
    let mut service = authority(Arc::new(Memory::default()), clock.clone())?;
    let owner = principal("owner");
    let guest = principal("guest");
    member(&mut service, &owner, "guest", Role::Admin)?;
    register_host(&mut service, &owner)?;
    let _holder = register_session(&mut service, &owner)?;
    let before = service.snapshot(&guest)?;
    ensure!(
        before.hosts.is_empty() && before.sessions.is_empty(),
        "metadata admins must not inherit resource access"
    )?;
    let session_scope = GrantScope::Session {
        session_id: "session".into(),
        permissions: vec![SessionPermission::Observe],
    };
    let _grant = success(service.execute(
        &owner,
        request(
            &owner,
            "grant",
            Mutation::Grant(GrantCommand::Issue {
                grant_id: "grant".into(),
                grantee: "guest".into(),
                scope: session_scope.clone(),
                expires_at: Some(Timestamp(NOW.saturating_add(60_000))),
            }),
        ),
    )?);
    ensure!(
        service.check_access(
            &guest,
            &AccessCheck {
                contributor_id: "guest".into(),
                scope: session_scope.clone()
            }
        )?,
        "an explicit session grant must allow observation"
    )?;
    ensure!(
        !service.check_access(
            &guest,
            &AccessCheck {
                contributor_id: "guest".into(),
                scope: GrantScope::Compute {
                    host_id: "host".into(),
                    permissions: vec![ComputePermission::Execute]
                }
            }
        )?,
        "session sharing must not authorize compute"
    )?;
    equal!(
        service.snapshot(&guest)?.sessions.len(),
        1,
        "explicitly visible sessions must be discoverable"
    )?;
    equal!(
        service.catch_up(&guest, &before.as_of, 20)?,
        RepositoryRecovery::SnapshotRequired,
        "grant changes must retire previous visibility cursors"
    )?;
    let presence = Presence {
        connection_id: "connection".into(),
        contributor_id: "owner".into(),
        repository_id: "repository".into(),
        branch: Some("main".into()),
        file: Some("src/lib.rs".into()),
        host_id: Some("host".into()),
        summary: None,
        observed_at: Timestamp(NOW),
        valid_until: Timestamp(NOW.saturating_add(30_000)),
    };
    service.publish_presence(&owner, presence.clone())?;
    equal!(
        service
            .presence(&guest)?
            .first()
            .and_then(|value| value.host_id.as_ref()),
        None,
        "presence must redact hosts without compute discovery access"
    )?;
    let mut traversal = presence;
    traversal.file = Some("../secret".into());
    equal!(
        service.publish_presence(&owner, traversal),
        Err(Error::Invalid),
        "paths must be repository-relative"
    )?;
    equal!(
        service.remove_presence(&guest, "connection"),
        Err(Error::Forbidden),
        "one contributor cannot remove another connection"
    )?;
    clock.0.store(NOW.saturating_add(60_000), Ordering::SeqCst);
    ensure!(
        service.presence(&owner)?.is_empty(),
        "presence must expire without a disconnect callback"
    )?;
    ensure!(
        !service.check_access(
            &guest,
            &AccessCheck {
                contributor_id: "guest".into(),
                scope: session_scope
            }
        )?,
        "grant expiry must be checked on active connections"
    )?;
    let _revoked = success(service.execute(
        &owner,
        request(
            &owner,
            "revoke-member",
            Mutation::Membership(Change {
                expected: WriteCondition::Revision(Revision(1)),
                value: Membership {
                    contributor_id: "guest".into(),
                    role: Role::Admin,
                    status: MembershipStatus::Revoked,
                },
            }),
        ),
    )?);
    equal!(
        service.snapshot(&guest),
        Err(Error::Forbidden),
        "revocation must invalidate reads on the same connection"
    )?;
    Ok(())
}

#[test]
fn restart_retires_leases_and_retains_increasing_epochs_and_retries() -> support::TestResult {
    let directory = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let mut owner = principal("owner");
    owner.runtime = Some(RuntimeBinding {
        host_id: "host".into(),
        runtime_id: "runtime".into(),
    });
    let storage = support::storage(directory.path())?;
    let mut service = authority(storage.clone(), clock.clone())?;
    register_host(&mut service, &owner)?;
    let holder = register_session(&mut service, &owner)?;
    let acquire = |epoch| {
        Mutation::Control(ControlCommand::Acquire {
            holder: holder.clone(),
            expected_epoch: ControlEpoch(epoch),
            lease_duration_ms: NonZeroU32::new(10_000).expect("positive duration"),
        })
    };
    let first = request(&owner, "acquire", acquire(0));
    let committed = service.execute(&owner, first.clone())?;
    let _assigned = success(committed.clone());
    let old_fence = service
        .snapshot(&owner)?
        .control
        .lease
        .expect("successful acquisition has a lease")
        .fence;
    ensure!(
        matches!(
            service.validate_control(&owner, &old_fence)?,
            ControlValidation::Current { .. }
        ),
        "the authenticated current holder must validate"
    )?;
    refused(
        service.execute(&owner, request(&owner, "competing", acquire(1)))?,
        ErrorCode::StaleControl,
    );
    drop(service);
    drop(storage);
    let storage = support::storage(directory.path())?;
    let mut restarted = authority(storage, clock.clone())?;
    equal!(
        restarted.snapshot(&owner)?.control.last_epoch,
        ControlEpoch(1),
        "restart must retain the watermark"
    )?;
    ensure!(
        restarted.snapshot(&owner)?.control.lease.is_none(),
        "restart must retire previously active leases"
    )?;
    equal!(
        restarted.execute(&owner, first)?,
        committed,
        "retained success is historical and must not reacquire ownership"
    )?;
    let _second = success(restarted.execute(&owner, request(&owner, "new-acquire", acquire(1)))?);
    equal!(
        restarted.snapshot(&owner)?.control.last_epoch,
        ControlEpoch(2),
        "reacquisition must increase epoch"
    )?;
    ensure!(
        matches!(
            restarted.validate_control(&owner, &old_fence)?,
            ControlValidation::Stale { .. }
        ),
        "an old fence cannot regain ownership"
    )?;
    let current = restarted
        .snapshot(&owner)?
        .control
        .lease
        .expect("second acquisition has a lease")
        .fence;
    clock.0.store(NOW.saturating_add(10_001), Ordering::SeqCst);
    refused(
        restarted.execute(
            &owner,
            request(
                &owner,
                "late-renew",
                Mutation::Control(ControlCommand::Renew {
                    fence: current,
                    lease_duration_ms: NonZeroU32::new(10_000).expect("positive duration"),
                }),
            ),
        )?,
        ErrorCode::StaleControl,
    );
    Ok(())
}

#[test]
fn uncertain_commit_faults_authority_until_reopened_and_reconciled() -> support::TestResult {
    let storage = Arc::new(Memory::default());
    let clock = Arc::new(TestClock::default());
    let owner = principal("owner");
    let mut service = authority(storage.clone(), clock.clone())?;
    let command = request(
        &owner,
        "uncertain",
        configuration(
            ConfigurationDocument::Settings,
            WriteCondition::Absent,
            "{}",
        ),
    );
    storage.uncertain.store(true, Ordering::SeqCst);
    equal!(
        service.execute(&owner, command.clone()),
        Err(Error::Storage),
        "a lost storage acknowledgement must not claim failure or success"
    )?;
    equal!(
        service.snapshot(&owner),
        Err(Error::Storage),
        "a faulted authority cannot continue from stale memory"
    )?;
    drop(service);
    let mut recovered = authority(storage, clock)?;
    let resolved = recovered
        .request_status(&owner, &command.context.key())?
        .expect("committed receipt must survive the lost acknowledgement");
    equal!(
        recovered.execute(&owner, command)?.result,
        resolved,
        "retry must recover, never execute twice"
    )?;
    Ok(())
}

#[derive(Debug, Default)]
struct Managed {
    accepted: Mutex<Option<(String, AdoptionReceipt)>>,
}

#[async_trait]
impl ManagedAdoption for Managed {
    async fn import(
        &self,
        package: &AdoptionPackage,
        _cancel: &CancellationToken,
    ) -> Result<AdoptionReceipt> {
        let mut accepted = self.accepted.lock().map_err(|_error| Error::Storage)?;
        let hash = package.hash()?;
        if let Some((previous, receipt)) = accepted.as_ref() {
            if previous != &hash {
                return Err(Error::Conflict);
            }
            return Ok(receipt.clone());
        }
        let receipt = AdoptionReceipt {
            transfer_id: package.transfer_id.clone(),
            target: package.target.clone(),
            package_hash: hash.clone(),
            workspace_id: package.snapshot.workspace.value.id.clone(),
            chain: package.snapshot.workspace.value.chain.clone(),
            last_epoch: package.snapshot.control.last_epoch,
        };
        *accepted = Some((hash, receipt));
        // Simulate destination commit followed by a dropped response.
        Err(Error::Transport)
    }
}

#[tokio::test]
async fn managed_adoption_reconciles_lost_ack_without_changing_chain_or_scope()
-> support::TestResult {
    let directory = tempfile::tempdir()?;
    let engine = support::engine(directory.path());
    support::seed(&engine.chain, 1, 1, b"retained local history")?;
    let _scope = engine.configure("space", ScopeChoice::FromNow).await?;
    let history = HistoryConsent::from_engine(&engine).await?;
    let ledger = std::fs::read(engine.chain.join("multiplayer/scope.json"))?;
    let clock = Arc::new(TestClock::default());
    let storage = Arc::new(Memory::default());
    let owner = principal("owner");
    let mut service = authority(storage.clone(), clock.clone())?;
    register_host(&mut service, &owner)?;
    let _holder = register_session(&mut service, &owner)?;
    let original = service.snapshot(&owner)?;
    let package = service.prepare_adoption(&owner, "managed-account", history)?;
    equal!(
        package.snapshot.sessions,
        original.sessions,
        "adoption must retain session/runtime/parent identity"
    )?;
    equal!(
        package.snapshot.workspace.value.chain,
        original.workspace.value.chain,
        "logical chain identity must not be regenerated"
    )?;
    equal!(
        service.execute(
            &owner,
            request(
                &owner,
                "frozen",
                configuration(
                    ConfigurationDocument::Settings,
                    WriteCondition::Absent,
                    "{}"
                )
            )
        ),
        Err(Error::Conflict),
        "frozen requests must not change the exported receipt set"
    )?;
    let managed = Managed::default();
    equal!(
        service
            .finish_adoption(&owner, &managed, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "lost acknowledgement must leave the durable handoff pending"
    )?;
    drop(service);
    let mut recovered = authority(storage.clone(), clock.clone())?;
    equal!(
        recovered
            .pending_adoption(&owner)?
            .expect("frozen package must survive restart")
            .hash()?,
        package.hash()?,
        "retries must submit exactly the same package"
    )?;
    let accepted = managed
        .accepted
        .lock()
        .map_err(|_error| Error::Storage)?
        .clone();
    if let Some((_hash, receipt)) = managed
        .accepted
        .lock()
        .map_err(|_error| Error::Storage)?
        .as_mut()
    {
        receipt.chain = "different-chain".into();
    }
    equal!(
        recovered
            .finish_adoption(&owner, &managed, &CancellationToken::new())
            .await,
        Err(Error::Invalid),
        "a changed chain acknowledgement must not finish adoption"
    )?;
    equal!(
        recovered
            .pending_adoption(&owner)?
            .ok_or(Error::Invalid)?
            .hash()?,
        package.hash()?,
        "invalid acknowledgements must retain the exact frozen package"
    )?;
    *managed.accepted.lock().map_err(|_error| Error::Storage)? = accepted;
    let receipt = recovered
        .finish_adoption(&owner, &managed, &CancellationToken::new())
        .await?;
    equal!(
        receipt.workspace_id,
        original.workspace.value.id,
        "managed receipt must preserve workspace identity"
    )?;
    drop(recovered);
    let restarted = authority(storage, clock)?;
    ensure!(
        matches!(
            restarted.snapshot(&owner)?.workspace.value.mode,
            CoordinationMode::Managed { .. }
        ),
        "completed adoption must reopen as managed"
    )?;
    equal!(
        std::fs::read(engine.chain.join("multiplayer/scope.json"))?,
        ledger,
        "metadata adoption must not rewrite the engine's enforcement ledger"
    )?;
    Ok(())
}

#[test]
fn native_private_storage_excludes_other_owners_and_checks_compare_exchange() -> support::TestResult
{
    let directory = tempfile::tempdir()?;
    let storage = support::storage(directory.path())?;
    ensure!(
        matches!(
            FilePersistence::open(directory.path().join("private")),
            Err(Error::Busy)
        ),
        "one directory must have exactly one live authority"
    )?;
    storage.compare_exchange("test", None, Some(b"one"))?;
    equal!(
        storage.compare_exchange("test", None, Some(b"two")),
        Err(Error::Conflict),
        "stale writes must leave durable state intact"
    )?;
    equal!(
        storage.load("test")?.as_deref(),
        Some(b"one".as_slice()),
        "failed CAS cannot mutate data"
    )?;
    equal!(
        storage.load("../test"),
        Err(Error::Invalid),
        "keys must not escape private storage"
    )?;
    Ok(())
}

#[tokio::test]
async fn metadata_only_adoption_does_not_create_history_consent() -> support::TestResult {
    let directory = tempfile::tempdir()?;
    let engine = support::engine(directory.path());
    let history = HistoryConsent::from_engine(&engine).await?;
    ensure!(
        history.scope.is_none() && history.devices.is_empty(),
        "unconfigured sharing must remain absent"
    )?;
    let mut service = authority(Arc::new(Memory::default()), Arc::new(TestClock::default()))?;
    let owner = principal("owner");
    let package = service.prepare_adoption(&owner, "managed-account", history)?;
    ensure!(
        package.history.scope.is_none(),
        "managed handoff must not manufacture a sharing grant"
    )?;
    let managed = Managed::default();
    equal!(
        service
            .finish_adoption(&owner, &managed, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "first response simulates a lost acknowledgement"
    )?;
    let _receipt = service
        .finish_adoption(&owner, &managed, &CancellationToken::new())
        .await?;
    ensure!(
        engine.scope().await?.is_none(),
        "adoption must leave native sharing unconfigured"
    )?;
    Ok(())
}
