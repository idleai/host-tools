use super::{
    Arc, Authority, Change, Error, Memory, Mutation, MutationValue, TestClock, WriteCondition,
    bootstrap, fs, principal, saved, settings, support,
};
use idle_protocol::v1::{
    grants::{ComputePermission, GrantCommand, GrantScope, ProviderPermission},
    identity::{Revision, Timestamp},
    membership::{Membership, MembershipStatus, Role},
    resources::{Availability, Health, ProviderKind},
    standalone::RepositorySnapshot,
};
use std::sync::atomic::Ordering;

fn revisions(snapshot: &RepositorySnapshot) -> idle_coordination::Result<(Revision, Revision)> {
    Ok((
        snapshot.hosts.first().ok_or(Error::Invalid)?.revision,
        snapshot.providers.first().ok_or(Error::Invalid)?.revision,
    ))
}

fn advanced(next: &RepositorySnapshot, previous: &RepositorySnapshot) -> support::TestResult {
    let (host, provider) = revisions(next)?;
    let (old_host, old_provider) = revisions(previous)?;
    ensure!(
        host > old_host && provider > old_provider,
        "both resource representations advance across source and access changes"
    )?;
    Ok(())
}

fn grant(
    authority: &mut Authority,
    id: &str,
    scope: GrantScope,
    expires_at: Option<Timestamp>,
) -> support::TestResult {
    let _saved = saved(
        authority,
        id,
        Mutation::Grant(GrantCommand::Issue {
            grant_id: id.into(),
            grantee: "viewer".into(),
            scope,
            expires_at,
        }),
    )?;
    Ok(())
}

fn compute() -> GrantScope {
    GrantScope::Compute {
        host_id: "workstation".into(),
        permissions: vec![ComputePermission::Connect],
    }
}

fn provider() -> GrantScope {
    GrantScope::Provider {
        provider_id: "local".into(),
        permissions: vec![ProviderPermission::UseModels],
    }
}

#[test]
fn declarations_publications_and_expiring_access_share_monotonic_revisions() -> support::TestResult
{
    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let clock = Arc::new(TestClock::default());
    let mut authority =
        Authority::open_repository(memory.clone(), clock.clone(), bootstrap(), root.path())?;
    let directory = root.path().join(".idle/workspace");
    fs::write(
        directory.join("hosts.json"),
        r#"{"hosts":[{"id":"workstation","name":"Host"}]}"#,
    )?;
    fs::write(
        directory.join("providers.json"),
        r#"{"providers":[{"id":"local","name":"Models","host_id":"workstation"}]}"#,
    )?;
    authority.refresh_repository()?;
    let declared = authority.snapshot(&principal("owner"))?;
    for health in declared
        .hosts
        .iter()
        .map(|record| &record.value.health)
        .chain(declared.providers.iter().map(|record| &record.value.health))
    {
        ensure!(
            health.observed_at < health.valid_until,
            "declared health satisfies client validation"
        )?;
        equal!(
            health.availability,
            Availability::Unknown,
            "declarations never imply connectivity"
        )?;
    }
    let _saved = saved(
        &mut authority,
        "member",
        Mutation::Membership(Change {
            expected: WriteCondition::Absent,
            value: Membership {
                contributor_id: "viewer".into(),
                role: Role::Member,
                status: MembershipStatus::Active,
            },
        }),
    )?;
    let mut host = declared.hosts.first().ok_or(Error::Invalid)?.value.clone();
    host.owner = "owner".into();
    host.health = Health {
        availability: Availability::Available,
        observed_at: Timestamp(support::NOW),
        valid_until: Timestamp(support::NOW + 30_000),
    };
    let mut models = declared
        .providers
        .first()
        .ok_or(Error::Invalid)?
        .value
        .clone();
    models.owner = "owner".into();
    models.health = host.health.clone();
    models.kind = ProviderKind::Local {
        host_id: "workstation".into(),
        runtime_id: "runtime".into(),
    };
    let _saved = saved(
        &mut authority,
        "publish-host",
        Mutation::Host(Change {
            expected: WriteCondition::Absent,
            value: host,
        }),
    )?;
    let _saved = saved(
        &mut authority,
        "publish-models",
        Mutation::Provider(Change {
            expected: WriteCondition::Absent,
            value: models,
        }),
    )?;
    let published = authority.snapshot(&principal("owner"))?;
    advanced(&published, &declared)?;
    let _saved = saved(
        &mut authority,
        "unrelated-settings",
        settings("{}", WriteCondition::Absent),
    )?;
    equal!(
        revisions(&authority.snapshot(&principal("owner"))?)?,
        revisions(&published)?,
        "unrelated configuration changes do not change resource write revisions"
    )?;
    let hidden = authority.snapshot(&principal("viewer"))?;
    ensure!(
        hidden
            .hosts
            .first()
            .ok_or(Error::Invalid)?
            .value
            .owner
            .0
            .starts_with("configured:"),
        "no grant exposes only the declaration"
    )?;
    grant(
        &mut authority,
        "compute",
        compute(),
        Some(Timestamp(support::NOW + 1000)),
    )?;
    grant(
        &mut authority,
        "models",
        provider(),
        Some(Timestamp(support::NOW + 1000)),
    )?;
    let visible = authority.snapshot(&principal("viewer"))?;
    advanced(&visible, &hidden)?;
    equal!(
        &visible.hosts.first().ok_or(Error::Invalid)?.value.owner.0,
        "owner",
        "authorized publication replaces the declaration"
    )?;
    let _previous = clock.0.fetch_add(2000, Ordering::SeqCst);
    let expired = authority.snapshot(&principal("viewer"))?;
    advanced(&expired, &visible)?;
    ensure!(
        expired
            .hosts
            .first()
            .ok_or(Error::Invalid)?
            .value
            .owner
            .0
            .starts_with("configured:"),
        "expiry restores the declared row"
    )?;
    grant(&mut authority, "compute-restored", compute(), None)?;
    grant(&mut authority, "models-restored", provider(), None)?;
    let restored = authority.snapshot(&principal("viewer"))?;
    advanced(&restored, &expired)?;
    let current = authority.snapshot(&principal("owner"))?;
    let host = current.hosts.first().ok_or(Error::Invalid)?;
    let receipt = saved(
        &mut authority,
        "update-host",
        Mutation::Host(Change {
            expected: WriteCondition::Revision(host.revision),
            value: host.value.clone(),
        }),
    )?;
    let MutationValue::Host(updated) = receipt.value else {
        return Err(Error::Invalid.into());
    };
    ensure!(
        updated.revision > host.revision,
        "snapshot revisions remain valid conditional-write tokens"
    )?;
    let before_restart = authority.snapshot(&principal("viewer"))?;
    drop(authority);
    let reopened = Authority::open_repository(memory, clock, bootstrap(), root.path())?;
    equal!(
        revisions(&reopened.snapshot(&principal("viewer"))?)?,
        revisions(&before_restart)?,
        "restart retains resource revision history"
    )?;
    Ok(())
}

#[test]
fn resource_definition_edits_and_recreation_never_reuse_revisions() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let mut authority = super::open(root.path(), Arc::new(Memory::default()))?;
    let directory = root.path().join(".idle/workspace");
    let mut previous = None;
    for name in ["Initial", "Updated", "Recreated"] {
        fs::write(
            directory.join("hosts.json"),
            serde_json::to_vec(&serde_json::json!({
                "hosts":[{"id":"workstation","name":name}]
            }))?,
        )?;
        fs::write(
            directory.join("providers.json"),
            serde_json::to_vec(&serde_json::json!({
                "providers":[{"id":"local","name":name}]
            }))?,
        )?;
        authority.refresh_repository()?;
        let current = authority.snapshot(&principal("owner"))?;
        if let Some(previous) = &previous {
            advanced(&current, previous)?;
        }
        authority.refresh_repository()?;
        equal!(
            revisions(&authority.snapshot(&principal("owner"))?)?,
            revisions(&current)?,
            "repeated reads retain resource revisions"
        )?;
        previous = Some(current);
        if name == "Updated" {
            fs::remove_file(directory.join("hosts.json"))?;
            fs::remove_file(directory.join("providers.json"))?;
            authority.refresh_repository()?;
            let absent = authority.snapshot(&principal("owner"))?;
            ensure!(
                absent.hosts.is_empty() && absent.providers.is_empty(),
                "removed declarations disappear"
            )?;
        }
    }
    Ok(())
}

#[test]
fn existing_repository_state_initializes_resource_revision_history_once() -> support::TestResult {
    use idle_coordination::persistence::Persistence as _;

    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let mut authority = super::open(root.path(), memory.clone())?;
    let directory = root.path().join(".idle/workspace");
    fs::write(
        directory.join("hosts.json"),
        r#"{"hosts":[{"id":"workstation","name":"Host"}]}"#,
    )?;
    fs::write(
        directory.join("providers.json"),
        r#"{"providers":[{"id":"local","name":"Models"}]}"#,
    )?;
    authority.refresh_repository()?;
    let _saved = saved(
        &mut authority,
        "settings",
        settings("{}", WriteCondition::Absent),
    )?;
    let before = authority.snapshot(&principal("owner"))?;
    drop(authority);
    let bytes = memory.load("authority")?.ok_or(Error::Invalid)?;
    let mut state: serde_json::Value = serde_json::from_slice(&bytes)?;
    let _removed = state
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .remove("resource_revisions");
    memory.compare_exchange(
        "authority",
        Some(&bytes),
        Some(&serde_json::to_vec(&state)?),
    )?;
    let upgraded = super::open(root.path(), memory.clone())?;
    let after = upgraded.snapshot(&principal("owner"))?;
    let (host, provider) = revisions(&after)?;
    ensure!(
        host.0 > before.as_of.position.0 && provider.0 > before.as_of.position.0,
        "upgrade advances past the earlier snapshot-position declaration revisions"
    )?;
    drop(upgraded);
    let restarted = super::open(root.path(), memory)?;
    equal!(
        revisions(&restarted.snapshot(&principal("owner"))?)?,
        revisions(&after)?,
        "upgrade runs once"
    )?;
    Ok(())
}
