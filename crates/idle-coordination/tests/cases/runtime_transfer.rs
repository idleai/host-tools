use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::Ordering},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use idle_coordination::{
    Error, Result,
    authority::{
        Authority,
        runtime_transfer::{RuntimeDestination, RuntimeTarget},
    },
    persistence::Persistence,
};
use idle_protocol::v1::{
    Change, WriteCondition,
    configuration::{ConfigurationDocument, ConfigurationValue, ConfigurationWrite},
    grants::{ComputePermission, GrantCommand, GrantScope},
    identity::Timestamp,
    membership::{Membership, MembershipStatus, Role},
    resources::{Availability, ComputeHost, Health},
    standalone::{AccessCheck, Mutation},
};

use super::support::{self, Memory, NOW, TestClock, bootstrap, principal, request};

fn settings(value: &str, expected: WriteCondition) -> Mutation {
    Mutation::Configuration(ConfigurationWrite {
        document: ConfigurationDocument::Settings,
        change: Change {
            expected,
            value: ConfigurationValue {
                schema_version: 1,
                json: value.into(),
            },
        },
    })
}

fn destination(root: &Path) -> RuntimeDestination {
    RuntimeDestination {
        target: RuntimeTarget {
            host_id: "daemon-one".into(),
            checkout_id: "checkout-one".into(),
        },
        workspace_id: "workspace".into(),
        repository_id: "repository".into(),
        chain_id: "logical-chain".into(),
        checkout_root: root.into(),
    }
}

fn copy_definitions(source: &Path, target: &Path) -> support::TestResult {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_definitions(&entry.path(), &target.join(entry.file_name()))?;
        } else {
            let _copied = fs::copy(entry.path(), target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn package(authority: &Authority) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = authority.runtime_transfer_chunk(&principal("owner"), bytes.len())?;
        bytes.extend(
            STANDARD
                .decode(chunk.content)
                .map_err(|_error| Error::Invalid)?,
        );
        if bytes.len() == chunk.total {
            return Ok(bytes);
        }
    }
}

#[test]
fn transfer_preserves_grants_revisions_and_original_request_results() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir(&source)?;
    let clock = Arc::new(TestClock::default());
    let storage = Arc::new(Memory::default());
    let mut local =
        Authority::open_repository(storage.clone(), clock.clone(), bootstrap(), &source)?;
    let owner = principal("owner");
    let original = request(
        &owner,
        "lost-settings-response",
        settings(
            &format!("{{\"value\":\"{}\"}}", "x".repeat(90_000)),
            WriteCondition::Absent,
        ),
    );
    let result = local.execute(&owner, original.clone())?;
    for (id, mutation) in [
        (
            "member",
            Mutation::Membership(Change {
                expected: WriteCondition::Absent,
                value: Membership {
                    contributor_id: "guest".into(),
                    role: Role::Member,
                    status: MembershipStatus::Active,
                },
            }),
        ),
        (
            "host",
            Mutation::Host(Change {
                expected: WriteCondition::Absent,
                value: ComputeHost {
                    id: "compute".into(),
                    owner: "owner".into(),
                    name: "Compute".into(),
                    capabilities: vec![],
                    routes: vec![],
                    health: Health {
                        availability: Availability::Available,
                        observed_at: Timestamp(NOW),
                        valid_until: Timestamp(NOW + 60_000),
                    },
                },
            }),
        ),
        (
            "grant",
            Mutation::Grant(GrantCommand::Issue {
                grant_id: "retained-grant".into(),
                grantee: "guest".into(),
                scope: GrantScope::Compute {
                    host_id: "compute".into(),
                    permissions: vec![ComputePermission::Connect],
                },
                expires_at: Some(Timestamp(NOW + 60_000)),
            }),
        ),
    ] {
        let _result = local.execute(&owner, request(&owner, id, mutation))?;
    }
    let before = local.snapshot(&owner)?;
    let destination = destination(&target);
    let revision = local.workspace_configuration()?.revision.clone();
    copy_definitions(&source.join(".idle"), &target.join(".idle"))?;
    let receipt = local.prepare_runtime_transfer(&owner, destination.target.clone(), &revision)?;
    let bytes = package(&local)?;
    ensure!(
        bytes.len() > 64 * 1024,
        "transfer must span multiple chunks"
    )?;
    ensure!(
        local
            .execute(
                &owner,
                request(&owner, "new-write", settings("{}", WriteCondition::Absent))
            )
            .is_err(),
        "the source must stay frozen"
    )?;
    equal!(
        local.execute(&owner, original.clone())?,
        result,
        "existing results stay recoverable at the source"
    )?;
    drop(local);
    clock.0.store(NOW + 1000, Ordering::SeqCst);
    let mut local =
        Authority::open_repository(storage.clone(), clock.clone(), bootstrap(), &source)?;
    equal!(
        package(&local)?,
        bytes,
        "restart cannot change the frozen bytes or transfer identity"
    )?;
    let remote_storage = Arc::new(Memory::default());
    let mut remote = Authority::import_runtime(
        remote_storage.clone(),
        clock.clone(),
        &destination,
        "owner",
        &bytes,
    )?;
    equal!(
        remote.snapshot(&owner)?,
        before,
        "all identities, revisions and grants survive transfer"
    )?;
    equal!(
        remote.runtime_owner()?,
        (receipt.clone(), owner.contributor.clone()),
        "daemon retains the same authenticated contributor"
    )?;
    ensure!(
        remote.check_access(
            &principal("guest"),
            &AccessCheck {
                contributor_id: "guest".into(),
                scope: GrantScope::Compute {
                    host_id: "compute".into(),
                    permissions: vec![ComputePermission::Connect]
                }
            }
        )?,
        "transferred grant remains active"
    )?;
    let changed = request(
        &owner,
        "daemon-write",
        settings(
            "{\"later\":true}",
            WriteCondition::Revision(before.settings.as_ref().ok_or(Error::Invalid)?.revision),
        ),
    );
    let changed_result = remote.execute(&owner, changed)?;
    equal!(
        remote.execute(&owner, original.clone())?,
        result,
        "lost-response retry returns the original result"
    )?;
    drop(remote);
    let remote = Authority::import_runtime(remote_storage, clock, &destination, "owner", &bytes)?;
    equal!(
        remote.request_status(&owner, &original.context.key())?,
        Some(result.result),
        "request status survives restart"
    )?;
    ensure!(
        remote.snapshot(&owner)?.settings != before.settings,
        "re-import cannot overwrite later daemon writes: {changed_result:?}"
    )?;
    local.complete_runtime_transfer(&owner, receipt)?;
    equal!(
        package(&local)?,
        bytes,
        "acknowledgement cannot change the frozen digest"
    )?;
    let saved: serde_json::Value =
        serde_json::from_slice(&storage.load("authority")?.ok_or(Error::Storage)?)?;
    equal!(
        saved.get("version").and_then(serde_json::Value::as_u64),
        Some(2),
        "older coordinators reject transferred private state"
    )?;
    Ok(())
}

#[test]
fn transfer_refuses_foreign_owners_bindings_and_divergent_checkouts() -> support::TestResult {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let mut local = Authority::open_repository(
        Arc::new(Memory::default()),
        clock.clone(),
        bootstrap(),
        source.path(),
    )?;
    let owner = principal("owner");
    let destination = destination(target.path());
    let revision = local.workspace_configuration()?.revision.clone();
    ensure!(
        local
            .prepare_runtime_transfer(&principal("guest"), destination.target.clone(), &revision)
            .is_err(),
        "a member cannot transfer ownership"
    )?;
    ensure!(
        local
            .prepare_runtime_transfer(&owner, destination.target.clone(), "stale")
            .is_err(),
        "changed files must be checked before freezing"
    )?;
    ensure!(
        local.runtime_transfer_status(&owner)?.is_none(),
        "failed preflight cannot freeze the source"
    )?;
    let _receipt = local.prepare_runtime_transfer(&owner, destination.target.clone(), &revision)?;
    let bytes = package(&local)?;
    let storage = Arc::new(Memory::default());
    ensure!(
        Authority::import_runtime(
            storage.clone(),
            clock.clone(),
            &destination,
            "guest",
            &bytes
        )
        .is_err(),
        "authenticated client must match the transferred owner"
    )?;
    ensure!(
        Authority::import_runtime(
            storage.clone(),
            clock.clone(),
            &destination,
            "owner",
            &bytes
        )
        .is_err(),
        "missing destination definitions cannot be overwritten"
    )?;
    copy_definitions(&source.path().join(".idle"), &target.path().join(".idle"))?;
    let mut foreign = destination.clone();
    foreign.chain_id = "another-chain".into();
    ensure!(
        Authority::import_runtime(storage.clone(), clock.clone(), &foreign, "owner", &bytes)
            .is_err(),
        "copied configuration cannot rebind the chain"
    )?;
    ensure!(
        storage.load("authority")?.is_none(),
        "refused imports leave no accepted owner"
    )?;
    let remote = Authority::import_runtime(
        storage.clone(),
        clock.clone(),
        &destination,
        "owner",
        &bytes,
    )?;
    drop(remote);
    let mut changed: serde_json::Value = serde_json::from_slice(&bytes)?;
    *changed.get_mut("transfer_id").ok_or(Error::Invalid)? = serde_json::json!("competing-owner");
    ensure!(
        Authority::import_runtime(
            storage,
            clock,
            &destination,
            "owner",
            &serde_json::to_vec(&changed)?
        )
        .is_err(),
        "a second transfer cannot replace the owner"
    )?;
    Ok(())
}

#[test]
fn uncertain_freeze_and_import_recover_the_same_transfer() -> support::TestResult {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let storage = Arc::new(Memory::default());
    let mut local =
        Authority::open_repository(storage.clone(), clock.clone(), bootstrap(), source.path())?;
    let owner = principal("owner");
    let destination = destination(target.path());
    let revision = local.workspace_configuration()?.revision.clone();
    copy_definitions(&source.path().join(".idle"), &target.path().join(".idle"))?;
    storage.uncertain.store(true, Ordering::SeqCst);
    ensure!(
        matches!(
            local.prepare_runtime_transfer(&owner, destination.target.clone(), &revision),
            Err(Error::Storage)
        ),
        "a lost durable acknowledgement stays uncertain"
    )?;
    drop(local);
    let mut local = Authority::open_repository(storage, clock.clone(), bootstrap(), source.path())?;
    let receipt = local.prepare_runtime_transfer(&owner, destination.target.clone(), &revision)?;
    let bytes = package(&local)?;
    let remote_storage = Arc::new(Memory::default());
    remote_storage.uncertain.store(true, Ordering::SeqCst);
    ensure!(
        matches!(
            Authority::import_runtime(
                remote_storage.clone(),
                clock.clone(),
                &destination,
                "owner",
                &bytes
            ),
            Err(Error::Storage)
        ),
        "uncertain import must not claim success"
    )?;
    let remote = Authority::import_runtime(remote_storage, clock, &destination, "owner", &bytes)?;
    equal!(
        remote.runtime_owner()?.0,
        receipt,
        "retry confirms the same accepted transfer"
    )?;
    Ok(())
}
