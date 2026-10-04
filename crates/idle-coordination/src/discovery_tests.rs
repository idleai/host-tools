use std::sync::atomic::{AtomicU64, Ordering};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::mpsc,
};

use super::*;
use crate::invitation::{Invitation, Secret};

#[derive(Debug, Default)]
struct Credential(AtomicU64);

#[async_trait]
impl Credentials for Credential {
    async fn management(&self, _cancel: &CancellationToken) -> Result<Secret> {
        Err(Error::Forbidden)
    }
    async fn discovery(&self, _cancel: &CancellationToken) -> Result<Secret> {
        Ok(Secret(format!(
            "private-{}",
            self.0.fetch_add(1, Ordering::SeqCst)
        )))
    }
    async fn renew(
        &self,
        _invitation: &Invitation,
        _cancel: &CancellationToken,
    ) -> Result<Option<Invitation>> {
        Ok(None)
    }
}

struct Server {
    origin: String,
    requests: mpsc::Receiver<String>,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn new(responses: Vec<(u16, serde_json::Value)>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let (sender, requests) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _address) = listener.accept().await?;
                let mut bytes = Vec::new();
                let header_end = loop {
                    let byte = stream.read_u8().await?;
                    bytes.push(byte);
                    if bytes.len() > 65_536 {
                        return Err(Error::Invalid);
                    }
                    if bytes.ends_with(b"\r\n\r\n") {
                        break bytes.len();
                    }
                };
                let headers = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map(str::parse::<usize>)
                    .transpose()
                    .map_err(|_error| Error::Invalid)?
                    .unwrap_or(0);
                if length > 65_536 {
                    return Err(Error::Invalid);
                }
                bytes.resize(header_end.checked_add(length).ok_or(Error::Invalid)?, 0);
                let _read = stream
                    .read_exact(bytes.get_mut(header_end..).ok_or(Error::Invalid)?)
                    .await?;
                sender
                    .send(String::from_utf8(bytes).map_err(|_error| Error::Invalid)?)
                    .await
                    .map_err(|_error| Error::Transport)?;
                let body = serde_json::to_vec(&body)?;
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
                stream.write_all(&body).await?;
            }
            Ok(())
        });
        Ok(Self {
            origin,
            requests,
            task,
        })
    }

    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .expect("HTTP fixture responds")
            .expect("HTTP fixture request")
    }

    fn directory(&self, credential: Arc<Credential>) -> GitHubDirectory {
        let mut directory =
            GitHubDirectory::new("owner/repository", credential).expect("explicit directory");
        directory.origin.clone_from(&self.origin);
        directory
    }
}

fn advertisement() -> Advertisement {
    let root = tempfile::tempdir().expect("device directory");
    let device = editchain_sync::DeviceIdentity::load_or_create(root.path())
        .expect("fixture device")
        .public();
    Advertisement {
        version: 1,
        protocol: DISCOVERY_PROTOCOL,
        encoding: 1,
        space: "space".into(),
        device,
        instance: "idle-relay-0123456789abcdef01234567".into(),
        expires_at: 600_000,
        endpoint: RelayEndpoint {
            tunnel_id: "tunnel".into(),
            cluster_id: "test".into(),
            host_id: "host".into(),
            client_relay_uri: "wss://test.rel.tunnels.api.visualstudio.com/test".into(),
            host_public_keys: vec!["YWJj".into()],
        },
    }
}

#[tokio::test]
async fn publication_contains_only_public_fields_and_creates_only_after_not_found() {
    let mut server = Server::new(vec![
        (404, serde_json::json!({})),
        (201, serde_json::json!({})),
        (
            403,
            serde_json::json!({"message":"private provider detail"}),
        ),
    ])
    .await
    .expect("HTTP fixture");
    let credential = Arc::new(Credential::default());
    let directory = server.directory(credential.clone());
    let advertisement = advertisement();
    directory
        .publish(&advertisement, 1, &CancellationToken::new())
        .await
        .expect("create missing variable");
    let patch = server.next().await;
    let post = server.next().await;
    assert!(
        patch.starts_with("PATCH ") && post.starts_with("POST "),
        "only a missing variable enables creation"
    );
    assert!(
        patch.contains("Bearer private-0") && post.contains("Bearer private-1"),
        "each HTTP call obtains current discovery credentials"
    );
    let body: serde_json::Value =
        serde_json::from_str(post.split_once("\r\n\r\n").expect("HTTP body").1)
            .expect("public payload");
    let public: serde_json::Value = serde_json::from_str(
        body.get("value")
            .and_then(serde_json::Value::as_str)
            .expect("variable value"),
    )
    .expect("advertisement");
    assert_eq!(
        public.as_object().expect("public object").len(),
        8,
        "publication uses the public allowlist"
    );
    assert!(
        public.get("connectToken").is_none(),
        "connect grants never enter discovery"
    );
    assert_eq!(
        directory
            .publish(&advertisement, 1, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "permission failures do not authorize fallback creation"
    );
    assert!(
        server.next().await.starts_with("PATCH "),
        "denied update makes exactly one request"
    );
    assert_eq!(
        credential.0.load(Ordering::SeqCst),
        3,
        "discovery never uses management authorization"
    );
}

#[tokio::test]
async fn directory_filters_stale_incompatible_misnamed_and_foreign_space_entries_across_pages() {
    let valid = advertisement();
    let variable = |ad: &Advertisement| serde_json::json!({ "name": ad.name(), "value": serde_json::to_string(ad).expect("advertisement JSON") });
    let mut expired = valid.clone();
    expired.expires_at = 1;
    let mut foreign = valid.clone();
    foreign.space = "other-space".into();
    let mut old = valid.clone();
    old.protocol = 2;
    let mut first = vec![
        variable(&expired),
        variable(&foreign),
        variable(&old),
        serde_json::json!({ "name": "EDITCHAIN_PEER_WRONG", "value": serde_json::to_string(&valid).expect("valid JSON") }),
    ];
    first.resize(
        30,
        serde_json::json!({ "name": "UNRELATED", "value": "private unrelated value" }),
    );
    let mut server = Server::new(vec![
        (200, serde_json::json!({"total_count":31,"variables":first})),
        (
            200,
            serde_json::json!({"total_count":31,"variables":[variable(&valid)]}),
        ),
    ])
    .await
    .expect("HTTP fixture");
    let directory = server.directory(Arc::new(Credential::default()));
    assert_eq!(
        directory
            .read("space", 2, &CancellationToken::new())
            .await
            .expect("directory pages"),
        vec![valid],
        "only correctly named compatible fresh entries qualify"
    );
    assert!(
        server.next().await.contains("page=1") && server.next().await.contains("page=2"),
        "all bounded pages are checked"
    );
}

#[tokio::test]
async fn cancelled_discovery_makes_no_http_request_and_removal_targets_one_instance() {
    let mut server = Server::new(vec![(404, serde_json::json!({}))])
        .await
        .expect("HTTP fixture");
    let credential = Arc::new(Credential::default());
    let directory = server.directory(credential.clone());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        directory.read("space", 1, &cancelled).await,
        Err(Error::Cancelled),
        "cancelled directory reads cannot request access"
    );
    assert_eq!(
        credential.0.load(Ordering::SeqCst),
        0,
        "cancelled discovery never queries credentials"
    );
    let advertisement = advertisement();
    directory
        .remove(&advertisement, &CancellationToken::new())
        .await
        .expect("already absent entry");
    assert!(
        server.next().await.starts_with(&format!(
            "DELETE /repos/owner/repository/actions/variables/{} ",
            advertisement.name()
        )),
        "teardown affects only its own public instance"
    );
}
