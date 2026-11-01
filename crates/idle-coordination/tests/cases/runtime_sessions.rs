use idle_coordination::clock::{Clock, SystemClock};

use super::*;

fn configuration(fixture: &Fixture, runtime: &str) -> Result<Value, Error> {
    let mut value = super::configuration(fixture, runtime)?;
    *value.get_mut("version").ok_or(Error::Invalid)? = json!(3);
    Ok(value)
}

fn envelope(author: &str, id: &str, body: &Value) -> Result<Value, Error> {
    Ok(json!({"api_version":"1","context":{
        "workspace_id":"workspace","request_id":id,
        "contributor":{"contributor_id":author,"authenticated_as":{"issuer":"idle-runtime-grant","subject":author}},
        "expires_at":SystemClock.now_ms()?.max(support::NOW).saturating_add(60_000).to_string()
    },"control_fence":null,"body":body}))
}

fn submission(author: &str, id: &str, text: &str) -> Result<Value, Error> {
    let mut request = envelope(author, id, &json!({"session_id":"runner","text":text}))?;
    let _previous = request
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .insert("coordination_receipt".into(), Value::Null);
    Ok(json!({"kind":"submit","request":request}))
}

async fn session(
    client: &mut DuplexStream,
    author: &str,
    operation: Value,
) -> Result<Value, Error> {
    daemon_call(
        client,
        author,
        json!({"kind":"session","operation":operation,"participant":{"grant_id":format!("grant-{author}"),"write":false}}),
    )
    .await
}

async fn work(client: &mut DuplexStream, operation: Value) -> Result<Value, Error> {
    daemon_call(
        client,
        "owner",
        json!({"kind":"session_work","work":operation}),
    )
    .await
}

async fn share(client: &mut DuplexStream) -> support::TestResult {
    mutate(
        client,
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
        client,
        "session-grant",
        GrantScope::Session {
            session_id: "runner".into(),
            permissions: vec![SessionPermission::Observe, SessionPermission::SubmitInput],
        },
    )
    .await?;
    grant_guest(
        client,
        "compute-grant",
        GrantScope::Compute {
            host_id: "daemon".into(),
            permissions: vec![
                ComputePermission::Connect,
                ComputePermission::ReadFiles,
                ComputePermission::WriteFiles,
                ComputePermission::Execute,
            ],
        },
    )
    .await
}

#[tokio::test]
async fn shared_inputs_keep_author_order_and_original_reply_across_reconnect() -> support::TestResult
{
    let fixture = fixture()?;
    let configuration = configuration(&fixture, "runtime")?;
    let (mut client, task) = start(&configuration).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    equal!(
        work(
            &mut client,
            json!({"kind":"direct_input","session_id":"runner"})
        )
        .await?,
        json!({"Ok":true}),
        "unshared command runners retain their direct input route"
    )?;
    share(&mut client).await?;
    equal!(
        work(
            &mut client,
            json!({"kind":"direct_input","session_id":"runner"})
        )
        .await?,
        json!({"Ok":false}),
        "shared runners cannot bypass attributed input ordering"
    )?;
    let owner = submission("owner", "owner-task", "owner prompt")?;
    let guest = submission("guest", "guest-task", "guest prompt")?;
    let accepted_owner = session(&mut client, "owner", owner.clone()).await?;
    let accepted_guest = session(&mut client, "guest", guest.clone()).await?;
    equal!(
        accepted_owner.pointer("/Ok/update/state/delivery/order"),
        Some(&json!("1")),
        "first author receives first order"
    )?;
    equal!(
        accepted_guest.pointer("/Ok/update/state/delivery/order"),
        Some(&json!("2")),
        "second author receives second order"
    )?;
    equal!(
        accepted_guest.pointer("/Ok/update/contributor/contributor_id"),
        Some(&json!("guest")),
        "runtime retains actual input author"
    )?;
    let mut changed = guest.clone();
    *changed
        .pointer_mut("/request/body/text")
        .ok_or(Error::Invalid)? = json!("different prompt");
    equal!(
        session(&mut client, "guest", changed).await?,
        json!({"Err":"conflict"}),
        "changing content cannot reuse a request"
    )?;
    equal!(
        session(&mut client, "owner", guest.clone()).await?,
        json!({"Err":"forbidden"}),
        "connection cannot impersonate another author"
    )?;
    drop(client);
    task.await??;

    let (mut client, task) = start(&configuration).await?;
    equal!(
        session(&mut client, "owner", owner).await?,
        accepted_owner,
        "lost owner reply replays its original order"
    )?;
    equal!(
        session(&mut client, "guest", guest).await?,
        accepted_guest,
        "lost guest reply replays its original order"
    )?;
    let snapshot = session(&mut client, "guest", json!({"kind":"snapshot"})).await?;
    let inputs = snapshot
        .pointer("/Ok/inputs")
        .and_then(Value::as_array)
        .ok_or(Error::Invalid)?;
    equal!(
        inputs.len(),
        2,
        "reconnect contains each accepted prompt once"
    )?;
    equal!(
        inputs.first().and_then(|input| input.get("text")),
        Some(&json!("owner prompt")),
        "snapshot retains exact prompt text"
    )?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn claims_and_completion_are_idempotent_and_crashed_work_is_not_replayed()
-> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&configuration(&fixture, "first")?).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    let _accepted = session(&mut client, "owner", submission("owner", "one", "first")?).await?;
    let _accepted = session(&mut client, "owner", submission("owner", "two", "second")?).await?;
    let claim = json!({"kind":"claim","session_id":"runner"});
    let first = work(&mut client, claim.clone()).await?;
    equal!(
        work(&mut client, claim.clone()).await?,
        first,
        "lost claim reply resolves to the same unstarted input"
    )?;
    let reference = first.pointer("/Ok/input").ok_or(Error::Invalid)?.clone();
    let release = json!({"kind":"release","input":reference});
    let released = work(&mut client, release.clone()).await?;
    equal!(
        work(&mut client, release).await?,
        released,
        "lost release reply does not strand the queue"
    )?;
    equal!(
        work(&mut client, claim.clone()).await?,
        first,
        "busy native thread does not consume a task"
    )?;
    let started = json!({"kind":"started","input":reference,"turn_id":"turn-one"});
    let running = work(&mut client, started.clone()).await?;
    equal!(
        work(&mut client, started).await?,
        running,
        "repeated started report retains revision and native turn"
    )?;
    equal!(
        work(&mut client, claim.clone()).await?,
        json!({"Ok":null}),
        "only one input can run at a time"
    )?;
    drop(client);
    task.await??;

    let (mut client, task) = start(&configuration(&fixture, "second")?).await?;
    publish_runner(&mut client, "runner", None).await?;
    let next = work(&mut client, claim).await?;
    equal!(
        next.pointer("/Ok/input/request/request_id"),
        Some(&json!("two")),
        "restart skips the uncertain running task"
    )?;
    let completed = json!({"kind":"completed","input":next.pointer("/Ok/input").ok_or(Error::Invalid)?,"outcome":{"kind":"succeeded"}});
    let saved = work(&mut client, completed.clone()).await?;
    equal!(
        work(&mut client, completed).await?,
        saved,
        "lost completion reply retains the original result"
    )?;
    let snapshot = session(&mut client, "owner", json!({"kind":"snapshot"})).await?;
    equal!(
        snapshot.pointer("/Ok/inputs/0/update/state/outcome/kind"),
        Some(&json!("failed")),
        "uncertain prior work is terminal without replay"
    )?;
    equal!(
        snapshot.pointer("/Ok/inputs/1/update/state/outcome/kind"),
        Some(&json!("succeeded")),
        "later task retains confirmed completion"
    )?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn revocation_blocks_reads_and_queued_work_but_allows_native_terminal_records()
-> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&configuration(&fixture, "runtime")?).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    share(&mut client).await?;
    let _accepted = session(&mut client, "guest", submission("guest", "one", "first")?).await?;
    let _accepted = session(&mut client, "guest", submission("guest", "two", "second")?).await?;
    let claim = json!({"kind":"claim","session_id":"runner"});
    let first = work(&mut client, claim.clone()).await?;
    let reference = first.pointer("/Ok/input").ok_or(Error::Invalid)?;
    let _started = work(
        &mut client,
        json!({"kind":"started","input":reference,"turn_id":"turn"}),
    )
    .await?;
    mutate(
        &mut client,
        "remove-access",
        Mutation::Grant(GrantCommand::Revoke {
            grant_id: "session-grant".into(),
            expected_revision: Revision(1),
        }),
    )
    .await?;
    let snapshot = session(&mut client, "guest", json!({"kind":"snapshot"})).await?;
    equal!(
        snapshot.pointer("/Ok/inputs"),
        Some(&json!([])),
        "removed observer cannot read retained inputs"
    )?;
    equal!(
        session(
            &mut client,
            "guest",
            submission("guest", "three", "blocked")?
        )
        .await?,
        json!({"Err":"forbidden"}),
        "removed participant cannot send another task"
    )?;
    let complete = json!({"kind":"session_work","work":{"kind":"completed","input":reference,"outcome":{"kind":"cancelled"}}});
    equal!(
        call(&mut client, "owner", complete.clone()).await?,
        json!({"Err":"forbidden"}),
        "public client cannot forge a runtime result"
    )?;
    let completed = daemon_call(&mut client, "owner", complete).await?;
    equal!(
        completed.pointer("/Ok/state/outcome/kind"),
        Some(&json!("cancelled")),
        "daemon can record stopped work after revocation"
    )?;
    equal!(
        work(&mut client, claim).await?,
        json!({"Ok":null}),
        "queued input is not executed after its author loses access"
    )?;
    let snapshot = session(&mut client, "owner", json!({"kind":"snapshot"})).await?;
    equal!(
        snapshot.pointer("/Ok/inputs/1/update/state/outcome/data/code"),
        Some(&json!("forbidden")),
        "queued denial becomes a durable result"
    )?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn creation_reserves_one_session_and_separates_compute_from_participation()
-> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&configuration(&fixture, "runtime")?).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    share(&mut client).await?;
    let create = json!({"kind":"create","request":envelope("guest", "new", &json!({"title":"Guest runner","host_id":"daemon"}))?});
    let reserved = session(&mut client, "guest", create.clone()).await?;
    equal!(
        reserved.pointer("/Ok/ready"),
        Some(&json!(false)),
        "reservation is not successful native creation"
    )?;
    equal!(
        session(&mut client, "guest", create.clone()).await?,
        reserved,
        "lost reply retains one reserved native identity"
    )?;
    let id = reserved
        .pointer("/Ok/session_id")
        .ok_or(Error::Invalid)?
        .clone();
    let register = json!({"kind":"register_runner","session_id":id,"parent":null});
    equal!(
        daemon_call(&mut client, "owner", register.clone()).await?,
        json!({"Err":"forbidden"}),
        "another contributor cannot take the reserved runner"
    )?;
    equal!(
        daemon_call(&mut client, "guest", register).await?,
        json!({"Ok":null}),
        "compute invitation permits a guest-owned runner"
    )?;
    let created = work(&mut client, json!({"kind":"created","session_id":id})).await?;
    equal!(
        created.pointer("/Ok/session/value/owner"),
        Some(&json!("guest")),
        "session ownership retains its creator"
    )?;
    equal!(
        created.pointer("/Ok/session/value/title"),
        Some(&json!("Guest runner")),
        "native confirmation publishes the requested title"
    )?;
    equal!(
        session(&mut client, "guest", create).await?,
        created,
        "creation retry returns the native confirmation"
    )?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn full_input_journal_remains_readable_and_allows_completion() -> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&configuration(&fixture, "runtime")?).await?;
    install(&mut client, &fixture).await?;
    publish_runner(&mut client, "runner", None).await?;
    let text = "x".repeat(32 * 1024);
    let mut accepted = 0;
    let mut full = false;
    for index in 0..32 {
        let result = session(
            &mut client,
            "owner",
            submission("owner", &format!("large-{index}"), &text)?,
        )
        .await?;
        if result == json!({"Err":"busy"}) {
            full = true;
            break;
        }
        ensure!(
            result.get("Ok").is_some(),
            "valid input was accepted before the limit"
        )?;
        accepted += 1;
    }
    ensure!(
        full && accepted > 0,
        "input limit reached before frame exhaustion"
    )?;
    let claim = work(&mut client, json!({"kind":"claim","session_id":"runner"})).await?;
    let result = work(&mut client, json!({"kind":"completed","input":claim.pointer("/Ok/input").ok_or(Error::Invalid)?,"outcome":{"kind":"succeeded"}})).await?;
    ensure!(
        result.get("Ok").is_some(),
        "full journal still accepts completion"
    )?;
    let snapshot = session(&mut client, "owner", json!({"kind":"snapshot"})).await?;
    let inputs = snapshot
        .pointer("/Ok/inputs")
        .and_then(Value::as_array)
        .ok_or(Error::Invalid)?;
    equal!(
        inputs.len(),
        accepted,
        "every accepted input remains readable"
    )?;
    ensure!(
        serde_json::to_vec(&snapshot)?.len() < 500 * 1024,
        "snapshot fits the transport"
    )?;
    drop(client);
    task.await??;
    Ok(())
}
