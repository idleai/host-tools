use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::timeout;

use super::{RemoteStream, framing};
use crate::{Error, Result};

#[tokio::test]
async fn idle_reader_drains_notifications_and_retains_approval_until_user_response() {
    let (local, mut remote) = tokio::io::duplex(1024);
    let mut stream = RemoteStream::new(Box::new(local));
    let approval = json!({"id":"approval-one","method":"item/commandExecution/requestApproval","params":{"threadId":"runner","command":"echo test"}});
    framing::write(&mut remote, &approval)
        .await
        .expect("write approval");
    // Sending more than a pipe can hold while no request is active must finish.
    timeout(Duration::from_secs(5), async {
        for index in 0..200 {
            framing::write(
                &mut remote,
                &json!({"method":"item/outputDelta","params":{"threadId":"runner","delta":index}}),
            )
            .await?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .expect("idle reader drains the pipe")
    .expect("write notifications");
    let server = tokio::spawn(async move {
        let call = framing::read::<Value>(&mut remote)
            .await?
            .ok_or_else(framing::invalid)?;
        framing::write(
            &mut remote,
            &json!({"id":call.get("id"),"result":{"running":true}}),
        )
        .await?;
        let response = framing::read::<Value>(&mut remote)
            .await?
            .ok_or_else(framing::invalid)?;
        Ok::<_, std::io::Error>((response, remote))
    });
    assert_eq!(
        stream
            .exchange(&json!({"id":1,"method":"thread/read","params":{"threadId":"runner"}}))
            .await
            .expect("read RPC response"),
        json!({"id":1,"result":{"running":true}}),
        "notifications cannot replace an RPC response"
    );
    let events = stream.events().expect("read buffered events");
    assert_eq!(
        events.get("droppedNotifications"),
        Some(&json!(136)),
        "the bounded queue reports discarded notifications"
    );
    assert_eq!(
        events.get("approvals"),
        Some(&json!([approval.clone()])),
        "approval stays available while output is drained"
    );
    assert_eq!(
        stream
            .events()
            .expect("poll buffered events")
            .get("approvals"),
        Some(&json!([approval])),
        "polling never approves a command"
    );
    assert_eq!(
        stream.approve(json!("unknown"), "accept").await,
        Err(Error::Invalid),
        "only a recorded request can be answered"
    );
    let submitted = stream
        .approve(json!("approval-one"), "decline")
        .await
        .expect("submit decline");
    assert_eq!(
        submitted,
        json!({"submitted":true}),
        "submission has no automatic approval result"
    );
    let (response, _remote) = server
        .await
        .expect("server task")
        .expect("server exchanges");
    assert_eq!(
        response,
        json!({"id":"approval-one","result":{"decision":"decline"}}),
        "only the user's decision is forwarded"
    );
    stream.close().await;
}

#[tokio::test]
async fn lost_transport_closes_reader_without_accepting_pending_approval() -> Result<()> {
    let (local, mut remote) = tokio::io::duplex(1024);
    let stream = RemoteStream::new(Box::new(local));
    framing::write(&mut remote, &json!({"id":"pending","method":"item/commandExecution/requestApproval","params":{"threadId":"runner"}})).await?;
    drop(remote);
    timeout(Duration::from_secs(5), async {
        loop {
            if stream.events() == Err(Error::Transport) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_error| Error::Timeout)?;
    stream.close().await;
    Ok(())
}
