use std::{
    fs,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use idle_coordination::{
    Error, Result, authority::Authority, persistence::Persistence,
    workspace_config::RepositoryFiles,
};
use idle_protocol::v1::{
    Change, WriteCondition,
    api::{ApiResult, ErrorCode},
    configuration::{ConfigurationDocument, ConfigurationValue, ConfigurationWrite},
    grants::{ComputePermission, GrantScope},
    projections::ProjectionKind,
    resources::Availability,
    standalone::{AccessCheck, Mutation, MutationResult, MutationValue, ViewDefinition},
};

use super::support::{self, Memory, TestClock, bootstrap, principal, request};

#[path = "workspace_config_regressions.rs"]
mod regressions;

#[path = "workspace_config_resources.rs"]
mod resources;

fn open(root: &Path, memory: Arc<dyn Persistence>) -> Result<Authority> {
    Authority::open_repository(memory, Arc::new(TestClock::default()), bootstrap(), root)
}

fn settings(json: &str, expected: WriteCondition) -> Mutation {
    Mutation::Configuration(ConfigurationWrite {
        document: ConfigurationDocument::Settings,
        change: Change {
            expected,
            value: ConfigurationValue {
                schema_version: 1,
                json: json.into(),
            },
        },
    })
}

fn saved(authority: &mut Authority, id: &str, mutation: Mutation) -> Result<MutationResult> {
    let owner = principal("owner");
    match authority
        .execute(&owner, request(&owner, id, mutation))?
        .result
    {
        ApiResult::Success(result) => Ok(result),
        ApiResult::Failure(_) => Err(Error::Invalid),
    }
}

fn view() -> ViewDefinition {
    ViewDefinition { id: "review-readiness".into(), title: "Review readiness".into(), kind: ProjectionKind::Task, schema_version: 1,
        json: r#"{"filters":{"labels":["review"],"nested":{"future":true}},"layout":{"group_by":"author"}}"#.into() }
}

fn git(root: &Path, args: &[&str]) -> support::TestResult {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    ensure!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )?;
    Ok(())
}

#[test]
fn workspace_config_fresh_clone_restores_authored_definitions_without_private_state()
-> support::TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    fs::create_dir(&source)?;
    git(&source, &["init", "-q"])?;
    let mut authority = open(&source, Arc::new(Memory::default()))?;
    let _saved = saved(
        &mut authority,
        "settings",
        settings(
            "{\n  \"future\": {\"nested\": [1, 2]}\n}\n",
            WriteCondition::Absent,
        ),
    )?;
    let _view = saved(
        &mut authority,
        "view",
        Mutation::View(Change {
            expected: WriteCondition::Absent,
            value: view(),
        }),
    )?;
    let config = source.join(".idle/workspace");
    fs::write(
        config.join("control.json"),
        r#"{"model":{"provider_id":"local","id":"small"},"budget":100}"#,
    )?;
    fs::write(
        config.join("hosts.json"),
        r#"{"hosts":[{"id":"workstation","name":"Development host"}]}"#,
    )?;
    fs::write(
        config.join("providers.json"),
        r#"{"providers":[{"id":"local","name":"Local models","host_id":"workstation","credential_ref":"local-serving"}]}"#,
    )?;
    authority.refresh_repository()?;
    let original = authority.workspace_configuration()?.clone();
    git(&source, &["add", ".idle/workspace"])?;
    git(
        &source,
        &[
            "-c",
            "user.name=Idle test",
            "-c",
            "user.email=idle@example.invalid",
            "commit",
            "-qm",
            "workspace",
        ],
    )?;
    git(root.path(), &["clone", "-q", "source", "clone"])?;
    let clone = open(&root.path().join("clone"), Arc::new(Memory::default()))?;
    equal!(
        clone.workspace_configuration()?,
        &original,
        "a clone restores the stable identity and all authored definitions"
    )?;
    let snapshot = clone.snapshot(&principal("owner"))?;
    equal!(snapshot.views.len(), 1, "complex projection is restored")?;
    equal!(snapshot.hosts.len(), 1, "declared host appears")?;
    equal!(snapshot.providers.len(), 1, "declared provider appears")?;
    equal!(
        snapshot
            .hosts
            .first()
            .ok_or(Error::Invalid)?
            .value
            .health
            .availability,
        Availability::Unknown,
        "declarations are not live health"
    )?;
    ensure!(
        snapshot.sessions.is_empty()
            && snapshot.grants.is_empty()
            && snapshot.control.lease.is_none(),
        "private runtime state never travels with the definitions"
    )?;
    ensure!(
        !clone.check_access(
            &principal("owner"),
            &AccessCheck {
                contributor_id: "owner".into(),
                scope: GrantScope::Compute {
                    host_id: "workstation".into(),
                    permissions: vec![ComputePermission::Execute]
                }
            }
        )?,
        "a tracked host cannot grant execution"
    )?;
    Ok(())
}

#[test]
fn workspace_config_external_edits_conflict_and_original_retries_do_not_overwrite()
-> support::TestResult {
    let root = tempfile::tempdir()?;
    let mut authority = open(root.path(), Arc::new(Memory::default()))?;
    let mutation = settings(r#"{"theme":"dark"}"#, WriteCondition::Absent);
    let initial = saved(&mut authority, "first", mutation.clone())?;
    let MutationValue::Configuration(record) = &initial.value else {
        return Err(Error::Invalid.into());
    };
    let revision = record.revision;
    let path = root.path().join(".idle/workspace/settings.json");
    fs::write(&path, r#"{"theme":"light","external":true}"#)?;
    let owner = principal("owner");
    let response = authority.execute(
        &owner,
        request(
            &owner,
            "stale",
            settings("{}", WriteCondition::Revision(revision)),
        ),
    )?;
    ensure!(
        matches!(response.result, ApiResult::Failure(error) if error.code == ErrorCode::StaleRevision),
        "stale editor save must conflict"
    )?;
    equal!(
        saved(&mut authority, "first", mutation.clone())?,
        initial,
        "retry returns the original committed result"
    )?;
    equal!(
        fs::read_to_string(&path)?,
        r#"{"theme":"light","external":true}"#,
        "retry preserves the external save"
    )?;
    fs::write(&path, "<<<<<<< conflict")?;
    ensure!(
        authority.refresh_repository().is_err(),
        "invalid files fail explicitly"
    )?;
    equal!(
        saved(&mut authority, "first", mutation)?,
        initial,
        "retained receipts remain readable even with a malformed current file"
    )?;
    Ok(())
}

#[test]
fn workspace_config_migration_is_once_and_existing_repository_files_win() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let mut legacy = support::authority(memory.clone(), Arc::new(TestClock::default()))?;
    let _saved = saved(
        &mut legacy,
        "old-settings",
        settings(r#"{"legacy":true}"#, WriteCondition::Absent),
    )?;
    let _view = saved(
        &mut legacy,
        "old-view",
        Mutation::View(Change {
            expected: WriteCondition::Absent,
            value: view(),
        }),
    )?;
    drop(legacy);
    let migrated = open(root.path(), memory.clone())?;
    ensure!(
        memory.load("workspace-original")?.is_some(),
        "migration retains a private original snapshot"
    )?;
    equal!(
        fs::read_to_string(root.path().join(".idle/workspace/settings.json"))?,
        r#"{"legacy":true}"#,
        "migration retains exact JSON"
    )?;
    let id = migrated.workspace_configuration()?.manifest.id.clone();
    drop(migrated);
    fs::remove_file(root.path().join(".idle/workspace/settings.json"))?;
    let mut reopened = open(root.path(), memory.clone())?;
    ensure!(
        reopened.workspace_configuration()?.settings.is_none(),
        "deletion leaves the authored file absent"
    )?;
    let reset = reopened
        .snapshot(&principal("owner"))?
        .settings
        .ok_or(Error::Invalid)?;
    equal!(
        reset.value.json,
        "{}",
        "deletion resets the logical document"
    )?;
    equal!(
        &reopened.workspace_configuration()?.manifest.id,
        &id,
        "restart retains logical workspace identity"
    )?;
    let _saved = saved(
        &mut reopened,
        "recreate",
        settings(r#"{"new":true}"#, WriteCondition::Revision(reset.revision)),
    )?;
    drop(reopened);
    // An unrelated old installation must load the tracked definition, not export over it.
    let other = Arc::new(Memory::default());
    let mut old = support::authority(other.clone(), Arc::new(TestClock::default()))?;
    let _saved = saved(
        &mut old,
        "other-old",
        settings(r#"{"old":true}"#, WriteCondition::Absent),
    )?;
    drop(old);
    let restored = open(root.path(), other)?;
    equal!(
        restored.workspace_configuration()?.settings.as_deref(),
        Some(r#"{"new":true}"#),
        "repository files take priority"
    )?;
    fs::remove_file(root.path().join(".idle/workspace/workspace.json"))?;
    drop(restored);
    ensure!(
        open(root.path(), memory).is_err(),
        "a missing tracked manifest must not be silently recreated"
    )?;
    Ok(())
}

#[test]
fn workspace_config_branch_changes_and_worktrees_retain_separate_bindings() -> support::TestResult {
    let root = tempfile::tempdir()?;
    git(root.path(), &["init", "-q"])?;
    let mut authority = open(root.path(), Arc::new(Memory::default()))?;
    let _saved = saved(
        &mut authority,
        "initial",
        settings(r#"{"branch":"first"}"#, WriteCondition::Absent),
    )?;
    git(root.path(), &["add", ".idle"])?;
    git(
        root.path(),
        &[
            "-c",
            "user.name=Idle test",
            "-c",
            "user.email=idle@example.invalid",
            "commit",
            "-qm",
            "first",
        ],
    )?;
    git(root.path(), &["branch", "first"])?;
    git(root.path(), &["checkout", "-qb", "second"])?;
    fs::write(
        root.path().join(".idle/workspace/settings.json"),
        r#"{"branch":"second"}"#,
    )?;
    git(root.path(), &["add", ".idle"])?;
    git(
        root.path(),
        &[
            "-c",
            "user.name=Idle test",
            "-c",
            "user.email=idle@example.invalid",
            "commit",
            "-qm",
            "second",
        ],
    )?;
    authority.refresh_repository()?;
    let second = authority
        .snapshot(&principal("owner"))?
        .settings
        .ok_or(Error::Invalid)?;
    git(root.path(), &["checkout", "-q", "first"])?;
    authority.refresh_repository()?;
    let first = authority
        .snapshot(&principal("owner"))?
        .settings
        .ok_or(Error::Invalid)?;
    ensure!(
        first.revision.0 > second.revision.0,
        "checkout rollback increases the local revision"
    )?;
    let linked = tempfile::tempdir()?;
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-q",
            linked.path().to_str().ok_or(Error::Invalid)?,
            "second",
        ],
    )?;
    let other = open(linked.path(), Arc::new(Memory::default()))?;
    equal!(
        &other.workspace_configuration()?.manifest.id,
        &authority.workspace_configuration()?.manifest.id,
        "worktrees share authored identity"
    )?;
    ensure!(
        other.workspace_configuration()?.settings != authority.workspace_configuration()?.settings,
        "worktrees read their own checkout"
    )?;
    Ok(())
}

#[derive(Debug, Default)]
struct InterruptedStorage {
    memory: Memory,
    fail: AtomicU8,
}

impl Persistence for InterruptedStorage {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.memory.load(key)
    }
    fn compare_exchange(
        &self,
        key: &str,
        previous: Option<&[u8]>,
        replacement: Option<&[u8]>,
    ) -> Result<()> {
        let selected = match self.fail.load(Ordering::SeqCst) {
            1 | 2 => key == "authority",
            3 => key == "workspace-write" && replacement.is_some(),
            4 => key == "workspace-write" && replacement.is_none(),
            _ => false,
        };
        let mode = if selected {
            self.fail.swap(0, Ordering::SeqCst)
        } else {
            0
        };
        if mode == 1 {
            return Err(Error::Storage);
        }
        self.memory.compare_exchange(key, previous, replacement)?;
        if matches!(mode, 2..=4) {
            return Err(Error::Storage);
        }
        Ok(())
    }
}

#[test]
fn workspace_config_interrupted_save_recovers_exact_receipt_before_or_after_private_commit()
-> support::TestResult {
    for mode in [1, 2, 3, 4] {
        let root = tempfile::tempdir()?;
        let storage = Arc::new(InterruptedStorage::default());
        let mut authority = open(root.path(), storage.clone())?;
        storage.fail.store(mode, Ordering::SeqCst);
        let mutation = settings(r#"{"saved":true}"#, WriteCondition::Absent);
        equal!(
            saved(&mut authority, "uncertain", mutation.clone()),
            Err(Error::Storage),
            "simulate interrupted journal or private-state persistence"
        )?;
        if mode != 3 {
            equal!(
                fs::read_to_string(root.path().join(".idle/workspace/settings.json"))?,
                r#"{"saved":true}"#,
                "authored file is already durable"
            )?;
        }
        equal!(
            authority.refresh_repository(),
            Err(Error::Storage),
            "uncertain writes freeze the service until recovery"
        )?;
        drop(authority);
        let mut recovered = open(root.path(), storage.clone())?;
        let original = saved(&mut recovered, "uncertain", mutation.clone())?;
        ensure!(
            storage.load("workspace-write")?.is_none(),
            "recovery clears the completed journal"
        )?;
        fs::write(
            root.path().join(".idle/workspace/settings.json"),
            r#"{"later":true}"#,
        )?;
        equal!(
            saved(&mut recovered, "uncertain", mutation)?,
            original,
            "retry is an original result, not another write"
        )?;
    }
    Ok(())
}

#[test]
fn workspace_config_invalid_documents_and_projection_paths_do_not_change_confirmed_state()
-> support::TestResult {
    let root = tempfile::tempdir()?;
    let mut authority = open(root.path(), Arc::new(Memory::default()))?;
    let original = authority.workspace_configuration()?.clone();
    let directory = root.path().join(".idle/workspace");
    for invalid in [
        r#"{"hosts":[{"id":"host","name":"Host","token":"secret"}]}"#,
        r#"{"hosts":[{"id":"host","name":"One"},{"id":"host","name":"Two"}]}"#,
        "[",
    ] {
        fs::write(directory.join("hosts.json"), invalid)?;
        ensure!(
            authority.refresh_repository().is_err(),
            "invalid host definition rejected"
        )?;
        equal!(
            authority.workspace_configuration()?,
            &original,
            "last confirmed state remains unchanged"
        )?;
    }
    fs::write(directory.join("hosts.json"), "{\"hosts\": []}\n")?;
    let mut invalid_view = view();
    invalid_view.id = "../../outside".into();
    ensure!(
        saved(
            &mut authority,
            "invalid-path",
            Mutation::View(Change {
                expected: WriteCondition::Absent,
                value: invalid_view
            })
        )
        .is_ok(),
        "nonportable legacy identities use a bounded encoded filename"
    )?;
    ensure!(
        !root.path().join("outside.json").exists(),
        "no write outside the definition directory"
    )?;
    let mut manifest = original.manifest;
    manifest.schema_version = 2;
    fs::write(
        directory.join("workspace.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    equal!(
        RepositoryFiles::open(root.path())?.read(),
        Err(Error::Version),
        "unknown schema is explicit"
    )?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_config_symlinked_files_and_directories_are_rejected() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let mut authority = open(root.path(), Arc::new(Memory::default()))?;
    let outside = root.path().join("outside.json");
    fs::write(&outside, "{}")?;
    std::os::unix::fs::symlink(&outside, root.path().join(".idle/workspace/settings.json"))?;
    ensure!(
        authority.refresh_repository().is_err(),
        "reject linked settings"
    )?;
    fs::remove_file(root.path().join(".idle/workspace/settings.json"))?;
    std::os::unix::fs::symlink(root.path(), root.path().join(".idle/workspace/projections"))?;
    ensure!(
        authority.refresh_repository().is_err(),
        "reject linked projection directory"
    )?;
    Ok(())
}

#[test]
fn workspace_config_projection_edits_preserve_compatible_document_fields() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let mut authority = open(root.path(), Arc::new(Memory::default()))?;
    let _saved = saved(
        &mut authority,
        "view",
        Mutation::View(Change {
            expected: WriteCondition::Absent,
            value: view(),
        }),
    )?;
    let path = root
        .path()
        .join(".idle/workspace/projections/review-readiness.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let _old = document
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .insert("future_document".into(), serde_json::json!({"keep":true}));
    fs::write(&path, serde_json::to_vec_pretty(&document)?)?;
    authority.refresh_repository()?;
    let record = authority
        .snapshot(&principal("owner"))?
        .views
        .into_iter()
        .next()
        .ok_or(Error::Invalid)?;
    let mut updated = record.value;
    updated.title = "Updated title".into();
    let _saved = saved(
        &mut authority,
        "edit-view",
        Mutation::View(Change {
            expected: WriteCondition::Revision(record.revision),
            value: updated,
        }),
    )?;
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    equal!(
        saved.get("future_document"),
        document.get("future_document"),
        "compatible document fields survive editor saves"
    )?;
    equal!(
        saved.get("definition"),
        document.get("definition"),
        "nested definitions survive editor saves"
    )?;
    Ok(())
}

#[test]
fn workspace_config_recovery_never_replaces_a_divergent_external_save() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let storage = Arc::new(InterruptedStorage::default());
    let mut authority = open(root.path(), storage.clone())?;
    storage.fail.store(1, Ordering::SeqCst);
    equal!(
        saved(
            &mut authority,
            "interrupted",
            settings(r#"{"requested":true}"#, WriteCondition::Absent)
        ),
        Err(Error::Storage),
        "interrupt after the file write"
    )?;
    drop(authority);
    let path = root.path().join(".idle/workspace/settings.json");
    fs::write(&path, r#"{"external":true}"#)?;
    ensure!(
        matches!(open(root.path(), storage), Err(Error::Conflict)),
        "divergent recovery is explicit"
    )?;
    equal!(
        fs::read_to_string(path)?,
        r#"{"external":true}"#,
        "the external edit is preserved"
    )?;
    Ok(())
}

#[test]
fn workspace_config_save_cannot_exceed_the_readers_projection_limit() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let mut authority = open(root.path(), memory.clone())?;
    let directory = root.path().join(".idle/workspace/projections");
    fs::create_dir(&directory)?;
    for index in 0..256 {
        let id = format!("view-{index}");
        fs::write(
            directory.join(format!("{id}.json")),
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1, "id":id, "title":"Saved view", "kind":"task", "definition":{}
            }))?,
        )?;
    }
    authority.refresh_repository()?;
    let before = authority.workspace_configuration()?.clone();
    ensure!(
        saved(
            &mut authority,
            "over-limit",
            Mutation::View(Change {
                expected: WriteCondition::Absent,
                value: view()
            })
        )
        .is_err(),
        "save rejects definitions that could not be reopened"
    )?;
    equal!(
        authority.workspace_configuration()?,
        &before,
        "the confirmed state stays valid"
    )?;
    ensure!(
        !directory.join("review-readiness.json").exists(),
        "invalid writes create no file"
    )?;
    drop(authority);
    equal!(
        open(root.path(), memory)?.workspace_configuration()?,
        &before,
        "reopening restores every accepted definition"
    )?;
    Ok(())
}
