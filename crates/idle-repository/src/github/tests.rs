use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use idle_protocol::v1::{
    projections::{ProjectionAvailability, ProjectionKind},
    repository::{ReadState, RepositoryScope},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use url::Url;

use super::{Client, Query, http::Http};
use crate::{Binding, Credentials, git::GithubRemote};

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

fn ok(value: &Value) -> Reply {
    Reply {
        status: 200,
        headers: Vec::new(),
        body: serde_json::to_vec(&value).expect("fixture JSON"),
    }
}
fn status(code: u16) -> Reply {
    Reply {
        status: code,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

async fn server(replies: Vec<Reply>) -> (Http, JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local fixture server");
    let origin = Url::parse(&format!(
        "http://{}",
        listener.local_addr().expect("fixture address")
    ))
    .expect("fixture URL");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.expect("fixture connection");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.expect("request header"));
                assert!(request.len() <= 32 * 1024, "bounded fixture request");
            }
            seen.lock()
                .expect("request log")
                .push(String::from_utf8(request).expect("request UTF-8"));
            let extra = reply
                .headers
                .iter()
                .fold(String::new(), |mut result, (key, value)| {
                    result.push_str(key);
                    result.push_str(": ");
                    result.push_str(value);
                    result.push_str("\r\n");
                    result
                });
            let header = format!(
                "HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                reply.status,
                reply.body.len()
            );
            socket
                .write_all(header.as_bytes())
                .await
                .expect("response header");
            let _write = socket.write_all(&reply.body).await;
        }
    });
    (Http::for_test(origin), task, requests)
}

async fn finished(task: JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("all planned fixture requests arrived")
        .expect("fixture server");
}

fn credentials(account: &str) -> Credentials {
    Credentials {
        account: account.into(),
        token: format!("fixture-secret-{account}"),
    }
}

#[tokio::test]
async fn conditional_cache_is_bound_to_account_token_and_repository() {
    let mut original = ok(&json!([{ "number": 1 }]));
    original.headers.push(("ETag", "\"first\"".into()));
    original
        .headers
        .push(("Link", "<https://evil.test/steal>; rel=\"next\"".into()));
    let (mut http, task, requests) = server(vec![
        original,
        status(304),
        status(304),
        status(401),
        ok(&json!([])),
    ])
    .await;
    let first = credentials("one");
    http.bind("owner/repo", Some(&first));
    let page = http
        .get("/repos/owner/repo/issues?page=1", Some(&first), 100)
        .await
        .expect("initial page");
    assert!(
        page.more,
        "next-page existence is retained without following its URL"
    );
    let cached = http
        .get("/repos/owner/repo/issues?page=1", Some(&first), 101)
        .await
        .expect("conditional page");
    assert_eq!(cached.bytes, page.bytes, "304 reuses exact original bytes");
    let second = credentials("two");
    http.bind("owner/repo", Some(&second));
    assert!(
        http.get("/repos/owner/repo/issues?page=1", Some(&second), 102)
            .await
            .is_err(),
        "account change cannot recover another account's cached bytes"
    );
    let denied = http
        .get("/repos/owner/repo/issues?page=1", Some(&second), 103)
        .await
        .expect_err("expired account");
    assert!(denied.unauthorized, "authentication failure is explicit");
    let _fresh = http
        .get("/repos/owner/repo/issues?page=1", Some(&second), 104)
        .await
        .expect("fresh response after rejection");
    finished(task).await;
    let requests = requests.lock().expect("requests");
    assert!(
        requests
            .get(1)
            .expect("conditional request")
            .to_ascii_lowercase()
            .contains("if-none-match: \"first\""),
        "conditional header belongs to the cached page"
    );
    assert!(
        !requests
            .get(2)
            .expect("new account request")
            .to_ascii_lowercase()
            .contains("if-none-match"),
        "account change clears validators"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /repos/owner/repo/issues?page=1 ")),
        "API paths never follow supplied Link destinations"
    );
}

#[tokio::test]
async fn rate_limit_waits_locally_and_redirects_never_receive_credentials() {
    let mut limited = status(429);
    limited.headers.push(("Retry-After", "60".into()));
    let mut moved = status(302);
    moved
        .headers
        .push(("Location", "http://127.0.0.1:1/secret".into()));
    let (mut http, task, requests) = server(vec![limited, moved]).await;
    let account = credentials("one");
    http.bind("owner/repo", Some(&account));
    let first = http
        .get("/repos/owner/repo", Some(&account), 1000)
        .await
        .expect_err("rate limit");
    assert_eq!(
        first.retry_at_ms,
        Some(61_000),
        "server retry delay retained"
    );
    let blocked = http
        .get("/repos/owner/repo", Some(&account), 2000)
        .await
        .expect_err("local backoff");
    assert_eq!(
        blocked.retry_at_ms, first.retry_at_ms,
        "a refresh cannot bypass the retry deadline"
    );
    let redirect = http
        .get("/repos/owner/repo", Some(&account), 61_001)
        .await
        .expect_err("redirect denied");
    assert!(
        redirect.message.contains("redirected"),
        "source changes require a new Git binding"
    );
    finished(task).await;
    assert_eq!(
        requests.lock().expect("requests").len(),
        2,
        "backoff and redirects do not send extra requests"
    );
}

fn repository() -> Value {
    json!({"full_name":"owner/repo", "html_url":"https://github.com/owner/repo", "description":"Fixture repository", "default_branch":"main", "visibility":"public"})
}

fn issue(number: u32, labels: &[&str]) -> Value {
    json!({"number":number, "title":format!("Issue {number}"), "html_url":format!("https://github.com/owner/repo/issues/{number}"), "state":"open", "updated_at":"2026-10-03T12:00:00Z", "labels":labels.iter().map(|name| json!({"name":name})).collect::<Vec<_>>()})
}

fn binding(root: &std::path::Path) -> Binding {
    Binding {
        scope: RepositoryScope {
            workspace_id: "workspace".into(),
            repository_id: "repository".into(),
            chain: "chain".into(),
        },
        root: root.into(),
        chain_directory: root.join("history"),
    }
}

#[tokio::test]
async fn local_startup_reads_git_and_sessions_without_contacting_github() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let installed = binding(directory.path());
    for arguments in [
        vec!["init", "--initial-branch=main"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(arguments)
                .output()
                .expect("Git fixture")
                .status
                .success()
        );
    }
    crate::record_tests::session(&installed.chain_directory, 1, "Local session");
    let (http, task, requests) = server(vec![ok(&repository())]).await;
    let reader = crate::Reader {
        binding: installed,
        github: Client { http, recent: None },
    };
    let result = tokio::time::timeout(Duration::from_secs(5), reader.read_local())
        .await
        .expect("local read does not wait for HTTP")
        .expect("local snapshot");
    assert!(requests.lock().expect("request log").is_empty());
    task.abort();
    assert_eq!(
        result
            .repository
            .checkout
            .expect("local checkout")
            .branch
            .as_deref(),
        Some("main")
    );
    assert_eq!(
        result
            .repository
            .sessions
            .first()
            .expect("local session")
            .labels,
        ["Local session"]
    );
    assert!(result.repository.github.is_none());
    assert!(
        result.projections.iter().all(|input| input.rows.is_empty()),
        "remote rows have not been read"
    );
    assert!(result.projections.iter().all(|input| input.total.is_none()
        && input.availability == ProjectionAvailability::Unavailable
        && input.gaps.iter().all(|gap| gap.message.contains("loading"))));
    assert!(
        result
            .repository
            .reports
            .iter()
            .any(|report| report.topic == "github.repository"
                && report.state == ReadState::Unavailable
                && report.message.contains("loading"))
    );
}

#[tokio::test]
async fn paginated_fields_map_to_distinct_views_with_exact_retained_sources() {
    let directory = tempfile::tempdir().expect("temporary binding");
    let binding = binding(directory.path());
    let mut first_page = ok(&json!([issue(1, &["needs-triage"])]));
    first_page.headers.push((
        "Link",
        "<https://evil.test/wrong?page=2>; rel=\"next\"".into(),
    ));
    let mut second_page = ok(&json!([issue(2, &["needs-input"])]));
    second_page.headers.push((
        "Link",
        "<https://api.github.com/repos/owner/repo/issues?page=3>; rel=\"next\"".into(),
    ));
    let mut pull = issue(3, &[]);
    let object = pull.as_object_mut().expect("pull object");
    let _previous = object.insert(
        "html_url".into(),
        json!("https://github.com/owner/repo/pull/3"),
    );
    let _previous = object.insert("requested_reviewers".into(), json!([{"login":"reviewer"}]));
    let _previous = object.insert("requested_teams".into(), json!([]));
    let (http, task, requests) = server(vec![ok(&repository()), ok(&json!([{"id":7,"login":"author","html_url":"https://github.com/author","contributions":9}])), status(403), first_page, second_page, ok(&json!([pull])), ok(&json!({"check_runs":[{"id":1,"name":"Compile","conclusion":"failure","html_url":"https://github.com/owner/repo/runs/1"}]})), ok(&json!({"workflow_runs":[]}))]).await;
    let mut client = Client { http, recent: None };
    let credentials = credentials("account");
    let remote = GithubRemote {
        owner: "owner".into(),
        name: "repo".into(),
    };
    let read = client
        .read(&Query {
            binding: &binding,
            remote: &remote,
            head: Some("1234567890abcdef"),
            credentials: Some(&credentials),
            now: 1000,
        })
        .await;
    finished(task).await;
    let input = |kind| {
        read.inputs
            .iter()
            .find(|input| input.kind == kind)
            .expect("input")
    };
    assert_eq!(
        input(ProjectionKind::Task).rows.len(),
        3,
        "issues and pull requests supply tasks"
    );
    assert_eq!(
        input(ProjectionKind::Task).availability,
        ProjectionAvailability::Partial,
        "pagination bound remains explicit"
    );
    assert_eq!(
        input(ProjectionKind::Triage).rows.len(),
        1,
        "only explicit triage labels qualify"
    );
    assert_eq!(
        input(ProjectionKind::NeedInput).rows.len(),
        2,
        "explicit labels and requested reviewers qualify"
    );
    assert_eq!(
        input(ProjectionKind::Error).rows.len(),
        1,
        "failed checks supply errors"
    );
    assert_eq!(
        input(ProjectionKind::Error).availability,
        ProjectionAvailability::Complete,
        "errors have independent coverage"
    );
    assert!(
        read.collaborators.is_empty(),
        "a collaborator denial cannot fabricate members"
    );
    assert_eq!(
        read.contributors.len(),
        1,
        "public contributors remain available"
    );
    assert!(
        read.reports
            .iter()
            .any(|report| report.topic == "github.collaborators"
                && report.state == ReadState::Unavailable),
        "partial collaborator access remains visible"
    );
    for input in &read.inputs {
        input.validate().expect("valid shared projection contract");
    }
    let queries = editchain_engine::queries::ChainQueries::open(&binding.chain_directory)
        .expect("stored response history");
    crate::validate_sources(&queries, &read.inputs)
        .expect("all source hashes resolve to accepted stored responses");
    assert!(
        requests
            .lock()
            .expect("requests")
            .iter()
            .all(|request| request.starts_with("GET /repos/owner/repo")),
        "pagination only requests the installed API path"
    );
    let serialized = serde_json::to_string(&read.inputs).expect("projection JSON");
    assert!(
        !serialized.contains(&credentials.token),
        "credentials never enter source rows"
    );
}

#[tokio::test]
async fn authentication_failure_returns_unavailable_and_no_cached_private_rows() {
    let directory = tempfile::tempdir().expect("temporary binding");
    let binding = binding(directory.path());
    let (http, task, _) = server(vec![status(401)]).await;
    let mut client = Client { http, recent: None };
    let remote = GithubRemote {
        owner: "owner".into(),
        name: "repo".into(),
    };
    let credentials = credentials("expired");
    let read = client
        .read(&Query {
            binding: &binding,
            remote: &remote,
            head: None,
            credentials: Some(&credentials),
            now: 100,
        })
        .await;
    finished(task).await;
    assert!(
        read.repository.is_none() && read.contributors.is_empty(),
        "rejected authentication produces no repository metadata"
    );
    assert!(
        read.inputs.iter().all(|input| input.rows.is_empty()
            && input.availability == ProjectionAvailability::Unavailable),
        "unavailable is distinct from an empty repository"
    );
}

#[tokio::test]
async fn automatic_reads_keep_original_check_times_and_explicit_refresh_discards_expired_access() {
    let directory = tempfile::tempdir().expect("temporary binding");
    let binding = binding(directory.path());
    let metadata = json!({"full_name":"owner/repo","html_url":"https://github.com/owner/repo","description":null,"default_branch":"main","visibility":"public"});
    let (http, task, requests) = server(vec![
        ok(&metadata),
        ok(&json!([])),
        ok(&json!([])),
        ok(&json!([])),
        status(401),
    ])
    .await;
    let mut client = Client { http, recent: None };
    let remote = GithubRemote {
        owner: "owner".into(),
        name: "repo".into(),
    };
    let mut query = Query {
        binding: &binding,
        remote: &remote,
        head: None,
        credentials: None,
        now: 100,
    };
    let first = client.replacement(&query, false).await;
    assert!(first.repository.is_some(), "initial public metadata loaded");
    query.now = 200;
    let cached = client.replacement(&query, false).await;
    assert_eq!(
        requests.lock().expect("requests").len(),
        4,
        "automatic updates reuse recent GitHub reads"
    );
    assert!(
        cached
            .reports
            .iter()
            .all(|report| report.checked_at_ms == 100),
        "cached reads retain their original check time"
    );
    assert!(
        cached.inputs.iter().all(|input| input.freshness.status
            == idle_protocol::v1::projections::FreshnessStatus::Unknown),
        "a cache reuse cannot claim a new current result"
    );
    let expired = client.replacement(&query, true).await;
    assert!(
        expired.repository.is_none() && expired.inputs.iter().all(|input| input.rows.is_empty()),
        "explicit refresh replaces the previous access result"
    );
    finished(task).await;
}
