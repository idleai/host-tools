use idle_coordination::service::Command;
use idle_protocol::v1::{
    Change, WriteCondition,
    grants::{ComputePermission, GrantCommand, GrantScope, SessionPermission},
    identity::Revision,
    membership::{Membership, MembershipStatus, Role},
    standalone::Mutation,
};

use super::*;

fn configuration(fixture: &Fixture, runtime: &str) -> Result<Value, Error> {
    let mut configuration = fixture.configuration.clone();
    let fields = configuration.as_object_mut().ok_or(Error::Invalid)?;
    let _previous = fields.insert("version".into(), json!(2));
    let _previous = fields.insert("runtime_id".into(), json!(runtime));
    Ok(configuration)
}

async fn install(client: &mut DuplexStream, fixture: &Fixture) -> support::TestResult {
    let uploaded = call(
        client,
        "owner",
        json!({"kind":"upload", "receipt":fixture.receipt,
        "offset":0,"total":fixture.chunk.total,"content":fixture.chunk.content}),
    )
    .await?;
    ensure!(uploaded.get("Ok").is_some(), "fixture upload succeeds")?;
    equal!(
        call(
            client,
            "owner",
            json!({"kind":"commit","receipt":fixture.receipt})
        )
        .await?,
        json!({"Ok":fixture.receipt}),
        "fixture transfer commits"
    )?;
    Ok(())
}

async fn daemon_call(
    client: &mut DuplexStream,
    contributor: &str,
    request: Value,
) -> Result<Value, Error> {
    write_frame(
        client,
        &json!({"client_id":contributor,"source":"daemon","request":request}),
    )
    .await?;
    read_frame(client).await?.ok_or(Error::Transport)
}

async fn mutate(client: &mut DuplexStream, id: &str, mutation: Mutation) -> support::TestResult {
    let command = Command::Mutate(Box::new(support::request(
        &principal("owner"),
        id,
        mutation,
    )));
    let result = call(client, "owner", json!({"kind":"call","command":command})).await?;
    equal!(
        result.pointer("/Ok/result/status"),
        Some(&json!("success")),
        "owner mutation commits"
    )?;
    Ok(())
}

async fn publish_runner(
    client: &mut DuplexStream,
    session: &str,
    parent: Option<&str>,
) -> support::TestResult {
    equal!(
        daemon_call(
            client,
            "owner",
            json!({"kind":"register_runner","session_id":session,"parent":parent})
        )
        .await?,
        json!({"Ok":null}),
        "the daemon publishes its reserved runner"
    )?;
    Ok(())
}

async fn grant_guest(
    client: &mut DuplexStream,
    id: &str,
    scope: GrantScope,
) -> support::TestResult {
    mutate(
        client,
        id,
        Mutation::Grant(GrantCommand::Issue {
            grant_id: id.into(),
            grantee: "guest".into(),
            expires_at: None,
            scope,
        }),
    )
    .await
}

#[tokio::test]
async fn runner_calls_require_negotiation_and_trusted_daemon_dispatch() -> support::TestResult {
    let fixture = fixture()?;
    let register = json!({"kind":"register_runner","session_id":"runner","parent":null});
    let authorize = json!({"kind":"authorize_runner","session_id":"runner","write":true});
    let (mut client, task) = start(&fixture.configuration).await?;
    install(&mut client, &fixture).await?;
    equal!(
        daemon_call(&mut client, "owner", register.clone()).await?,
        json!({"Err":"version"}),
        "legacy helpers never admit runner work"
    )?;
    drop(client);
    task.await??;

    let (mut client, task) = start(&configuration(&fixture, "runtime-one")?).await?;
    for request in [register.clone(), authorize.clone()] {
        equal!(
            call(&mut client, "owner", request).await?,
            json!({"Err":"forbidden"}),
            "a coordination call cannot claim a runtime operation"
        )?;
    }
    publish_runner(&mut client, "runner", None).await?;
    let admitted = daemon_call(&mut client, "owner", authorize.clone()).await?;
    equal!(
        admitted.pointer("/Ok/session/runtime"),
        Some(&json!({"host_id":"daemon","runtime_id":"runtime-one"})),
        "authorization retains the actual host and runtime"
    )?;
    let snapshot_request = json!({"kind":"call","command":{"kind":"snapshot"}});
    let before = call(&mut client, "owner", snapshot_request.clone()).await?;
    equal!(
        before.pointer("/Ok/hosts/0/value/capabilities"),
        Some(&json!(["sessions"])),
        "only supported session capability is published"
    )?;
    equal!(
        daemon_call(
            &mut client,
            "owner",
            json!({"kind":"register_runner","session_id":"bad-child","parent":"missing"})
        )
        .await?,
        json!({"Err":"forbidden"}),
        "invalid parent prevents registration"
    )?;
    equal!(
        call(&mut client, "owner", snapshot_request).await?,
        before,
        "failed registration cannot partially update host metadata"
    )?;

    let rules = "{\"schema_version\":1,\"runners\":{\"enabled\":false}}";
    fs::write(
        fixture
            .root
            .path()
            .join("target/.idle/workspace/agent-rules.json"),
        rules,
    )?;
    let changed = daemon_call(&mut client, "owner", authorize.clone()).await?;
    equal!(
        changed.pointer("/Ok/agent_rules"),
        Some(&json!(rules)),
        "admission reads current authored rules"
    )?;
    ensure!(
        changed.pointer("/Ok/configuration_revision")
            != admitted.pointer("/Ok/configuration_revision"),
        "changed rules carry a new configuration revision"
    )?;
    drop(client);
    task.await??;

    let (mut client, task) = start(&configuration(&fixture, "runtime-two")?).await?;
    equal!(
        daemon_call(&mut client, "owner", authorize.clone()).await?,
        json!({"Err":"forbidden"}),
        "a restarted daemon must explicitly restore the session binding"
    )?;
    publish_runner(&mut client, "runner", None).await?;
    equal!(
        daemon_call(&mut client, "owner", authorize)
            .await?
            .pointer("/Ok/session/runtime/runtime_id"),
        Some(&json!("runtime-two")),
        "resume keeps the session under the new runtime generation"
    )?;
    publish_runner(&mut client, "child", Some("runner")).await?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn runner_admission_rechecks_session_and_compute_grants_on_open_pipe() -> support::TestResult
{
    let fixture = fixture()?;
    let (mut client, task) = start(&configuration(&fixture, "runtime")?).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    let authorize = json!({"kind":"authorize_runner","session_id":"runner","write":false});
    let denied = json!({"Err":"forbidden"});
    equal!(
        daemon_call(&mut client, "guest", authorize.clone()).await?,
        denied,
        "unknown contributors are rejected"
    )?;
    mutate(
        &mut client,
        "member",
        Mutation::Membership(Change {
            expected: WriteCondition::Absent,
            value: Membership {
                contributor_id: "guest".into(),
                role: Role::Member,
                status: MembershipStatus::Active,
            },
        }),
    )
    .await?;
    grant_guest(
        &mut client,
        "session-grant",
        GrantScope::Session {
            session_id: "runner".into(),
            permissions: vec![SessionPermission::Observe, SessionPermission::SubmitInput],
        },
    )
    .await?;
    equal!(
        daemon_call(&mut client, "guest", authorize.clone()).await?,
        denied,
        "session grants do not authorize compute"
    )?;
    grant_guest(
        &mut client,
        "compute-grant",
        GrantScope::Compute {
            host_id: "daemon".into(),
            permissions: vec![
                ComputePermission::Connect,
                ComputePermission::ReadFiles,
                ComputePermission::Execute,
            ],
        },
    )
    .await?;
    equal!(
        daemon_call(&mut client, "guest", authorize.clone())
            .await?
            .pointer("/Ok/contributor_id"),
        Some(&json!("guest")),
        "authorized contributor identity is preserved"
    )?;
    equal!(
        daemon_call(
            &mut client,
            "guest",
            json!({"kind":"authorize_runner","session_id":"runner","write":true})
        )
        .await?,
        denied,
        "read and execute grants cannot authorize writes"
    )?;
    mutate(
        &mut client,
        "revoke",
        Mutation::Grant(GrantCommand::Revoke {
            grant_id: "session-grant".into(),
            expected_revision: Revision(1),
        }),
    )
    .await?;
    equal!(
        daemon_call(&mut client, "guest", authorize).await?,
        denied,
        "revocation is checked on the existing connection"
    )?;
    drop(client);
    task.await??;
    Ok(())
}
