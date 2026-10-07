use std::collections::BTreeMap;

use super::{
    Arc, Authority, Change, ConfigurationDocument, ConfigurationValue, ConfigurationWrite, Error,
    Memory, Mutation, MutationValue, Persistence, TestClock, WriteCondition, bootstrap, fs, open,
    principal, request, saved, support, view,
};
use idle_protocol::v1::{
    api::{ApiResult, ErrorCode},
    standalone::RepositorySnapshot,
};

fn record(
    snapshot: RepositorySnapshot,
    document: ConfigurationDocument,
) -> idle_coordination::Result<idle_protocol::v1::Record<ConfigurationValue>> {
    match document {
        ConfigurationDocument::Settings => snapshot.settings,
        ConfigurationDocument::AgentRules => snapshot.agent_rules,
    }
    .ok_or(Error::Invalid)
}

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

#[test]
fn invalid_names_and_ids_never_poison_restart() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let mut authority = open(root.path(), memory.clone())?;
    let original = authority.workspace_configuration()?.clone();
    let persisted = memory.load("authority")?;
    let manifest = root.path().join(".idle/workspace/workspace.json");
    let text = fs::read_to_string(&manifest)?;
    let mut invalid = original.manifest.clone();
    invalid.name = "   ".into();
    fs::write(&manifest, serde_json::to_vec(&invalid)?)?;
    equal!(
        authority.refresh_repository(),
        Err(Error::Invalid),
        "blank names are rejected"
    )?;
    equal!(
        memory.load("authority")?,
        persisted,
        "invalid files never replace private state"
    )?;
    fs::write(&manifest, text)?;
    let directory = root.path().join(".idle/workspace/projections");
    fs::create_dir(&directory)?;
    for id in ["   ".to_owned(), "x".repeat(257)] {
        let path = directory.join(format!("~{}.json", blake3::hash(id.as_bytes())));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "id": id, "title": "View", "kind": "task", "definition": {}
            }))?,
        )?;
        equal!(
            authority.refresh_repository(),
            Err(Error::Invalid),
            "invalid IDs are rejected"
        )?;
        equal!(
            memory.load("authority")?,
            persisted,
            "invalid IDs never reach private state"
        )?;
        fs::remove_file(path)?;
    }
    drop(authority);
    equal!(
        open(root.path(), memory)?.workspace_configuration()?,
        &original,
        "repairing tracked files is sufficient to reopen"
    )?;
    Ok(())
}

#[test]
fn deletion_resets_each_document_without_recreating_its_file_or_losing_receipts()
-> support::TestResult {
    for (document, name) in [
        (ConfigurationDocument::Settings, "settings.json"),
        (ConfigurationDocument::AgentRules, "agent-rules.json"),
    ] {
        let root = tempfile::tempdir()?;
        let memory = Arc::new(Memory::default());
        let mut authority = open(root.path(), memory.clone())?;
        let mutation = configuration(document, WriteCondition::Absent, r#"{"original":true}"#);
        let receipt = saved(&mut authority, "original", mutation.clone())?;
        let MutationValue::Configuration(original) = &receipt.value else {
            return Err(Error::Invalid.into());
        };
        let path = root.path().join(".idle/workspace").join(name);
        fs::remove_file(&path)?;
        authority.refresh_repository()?;
        let reset = record(authority.snapshot(&principal("owner"))?, document)?;
        equal!(
            reset.value.json,
            "{}",
            "deleted documents have empty logical content"
        )?;
        ensure!(
            reset.revision > original.revision,
            "deletion advances the document revision"
        )?;
        let cursor = authority.snapshot(&principal("owner"))?.as_of;
        authority.refresh_repository()?;
        equal!(
            authority.snapshot(&principal("owner"))?.as_of,
            cursor,
            "an absent file is reconciled once"
        )?;
        fs::write(
            root.path().join(".idle/workspace/control.json"),
            r#"{"changed":true}"#,
        )?;
        authority.refresh_repository()?;
        drop(authority);
        let mut reopened = open(root.path(), memory)?;
        equal!(
            record(reopened.snapshot(&principal("owner"))?, document)?,
            reset,
            "unrelated changes and restart retain the deletion revision"
        )?;
        equal!(
            saved(&mut reopened, "original", mutation)?,
            receipt,
            "retries retain the original result"
        )?;
        ensure!(
            !path.exists(),
            "restart and original retries never recreate deleted files"
        )?;
        let owner = principal("owner");
        let stale = reopened.execute(
            &owner,
            request(
                &owner,
                "stale",
                configuration(
                    document,
                    WriteCondition::Revision(original.revision),
                    r#"{"stale":true}"#,
                ),
            ),
        )?;
        ensure!(
            matches!(stale.result, ApiResult::Failure(error) if error.code == ErrorCode::StaleRevision),
            "a pre-deletion draft cannot overwrite the reset"
        )?;
        let _saved = saved(
            &mut reopened,
            "recreate",
            configuration(
                document,
                WriteCondition::Revision(reset.revision),
                r#"{"new":true}"#,
            ),
        )?;
        equal!(
            fs::read_to_string(path)?,
            r#"{"new":true}"#,
            "a reviewed save recreates the file"
        )?;
    }
    Ok(())
}

#[test]
fn oversized_legacy_migrations_stop_before_creating_files() -> support::TestResult {
    for (count, json) in [
        (257, "{}".to_owned()),
        (
            17,
            serde_json::to_string(&serde_json::json!({"large": "x".repeat(250_000)}))?,
        ),
    ] {
        let root = tempfile::tempdir()?;
        let memory = Arc::new(Memory::default());
        let mut authority = Authority::open(
            memory.clone(),
            Arc::new(TestClock::default()),
            Some(bootstrap()),
        )?;
        for index in 0..count {
            let mut item = view();
            item.id = format!("view-{index}");
            item.json.clone_from(&json);
            let _saved = saved(
                &mut authority,
                &format!("view-{index}"),
                Mutation::View(Change {
                    expected: WriteCondition::Absent,
                    value: item,
                }),
            )?;
        }
        drop(authority);
        ensure!(
            matches!(open(root.path(), memory.clone()), Err(Error::Invalid)),
            "migration checks count and total size limits"
        )?;
        ensure!(
            !root.path().join(".idle").exists(),
            "rejected migration writes no authored files"
        )?;
        ensure!(
            memory.load("workspace-seed")?.is_none(),
            "rejected migration retains no invalid seed"
        )?;
        let original = Authority::open(memory, Arc::new(TestClock::default()), Some(bootstrap()))?;
        equal!(
            original.snapshot(&principal("owner"))?.views.len(),
            count,
            "legacy definitions remain available"
        )?;
    }
    Ok(())
}

#[test]
fn resumed_migration_validates_seed_limits_before_writing() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let memory = Arc::new(Memory::default());
    let mut documents = BTreeMap::from([(
        "workspace.json".to_owned(),
        r#"{"schema_version":1,"id":"workspace","name":"Workspace"}"#.to_owned(),
    )]);
    for index in 0..257 {
        let id = format!("view-{index}");
        let _old = documents.insert(
            format!("projections/{id}.json"),
            serde_json::to_string(&serde_json::json!({
                "schema_version":1, "id":id, "title":"View", "kind":"task", "definition":{}
            }))?,
        );
    }
    let seed = serde_json::to_vec(&documents)?;
    memory.compare_exchange("workspace-seed", None, Some(&seed))?;
    ensure!(
        matches!(open(root.path(), memory.clone()), Err(Error::Invalid)),
        "resumed seeds use current limits"
    )?;
    ensure!(
        !root.path().join(".idle").exists(),
        "invalid resumed seeds create no files"
    )?;
    equal!(
        memory.load("workspace-seed")?,
        Some(seed),
        "failed validation preserves the original seed"
    )?;
    Ok(())
}
