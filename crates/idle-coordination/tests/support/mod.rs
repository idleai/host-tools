use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use editchain_core::{
    ActorId, BlobRef, Clock as EngineClock, ContentId, MessageOp, NodeId, Op, OpId, OpKind,
    ParentSet, Payload, ScopeRef, SourceId, Tags,
};
use editchain_store::{
    BlobStore, CanonicalChain, SegmentStore,
    format::{Page, encode_op},
};
use idle_coordination::{
    Error, Result,
    authority::{Authority, Bootstrap, Principal},
    clock::Clock,
    engine::Engine,
    invitation::Secret,
    persistence::{FilePersistence, Persistence},
};
use idle_protocol::v1::{
    ApiVersion,
    api::Request,
    identity::{ContributorIdentity, ExternalIdentity, RequestContext, Timestamp},
    standalone::Mutation,
    workspace::{CoordinationMode, Repository, Workspace},
};

pub(super) mod relay;

pub(super) type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(super) const NOW: u64 = 1_800_000_000_000;

#[derive(Debug)]
pub(super) struct TestClock(pub AtomicU64);

impl Default for TestClock {
    fn default() -> Self {
        Self(AtomicU64::new(NOW))
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> Result<u64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Default)]
pub(super) struct Memory {
    pub values: Mutex<BTreeMap<String, Vec<u8>>>,
    pub uncertain: AtomicBool,
    pub fail_stop_read: AtomicBool,
}

impl Persistence for Memory {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if key == "sharing-stop" && self.fail_stop_read.load(Ordering::SeqCst) {
            return Err(Error::Storage);
        }
        Ok(self
            .values
            .lock()
            .map_err(|_error| Error::Storage)?
            .get(key)
            .cloned())
    }
    fn compare_exchange(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        replacement: Option<&[u8]>,
    ) -> Result<()> {
        let mut values = self.values.lock().map_err(|_error| Error::Storage)?;
        if values.get(key).map(Vec::as_slice) != expected {
            return Err(Error::Conflict);
        }
        if let Some(replacement) = replacement {
            let _old = values.insert(key.into(), replacement.to_vec());
        } else {
            let _old = values.remove(key);
        }
        if self.uncertain.swap(false, Ordering::SeqCst) {
            return Err(Error::Storage);
        }
        Ok(())
    }
}

pub(super) fn principal(id: &str) -> Principal {
    Principal {
        contributor: ContributorIdentity {
            contributor_id: id.into(),
            authenticated_as: ExternalIdentity {
                issuer: "test-device".into(),
                subject: format!("device-{id}"),
            },
        },
        runtime: None,
    }
}

pub(super) fn workspace() -> Workspace {
    Workspace {
        id: "workspace".into(),
        chain: "logical-chain".into(),
        name: "Repository".into(),
        mode: CoordinationMode::Standalone {
            repository: Repository {
                id: "repository".into(),
                name: "Repository".into(),
                remote: Some("https://github.com/idleai/host-tools".into()),
            },
        },
    }
}

pub(super) fn bootstrap() -> Bootstrap {
    Bootstrap {
        workspace: workspace(),
        owner: "owner".into(),
    }
}

pub(super) fn request(actor: &Principal, id: &str, body: Mutation) -> Request<Mutation> {
    Request {
        api_version: ApiVersion::V1,
        context: RequestContext {
            workspace_id: "workspace".into(),
            request_id: id.into(),
            contributor: actor.contributor.clone(),
            expires_at: Timestamp(NOW.saturating_add(3_600_000)),
        },
        control_fence: None,
        body,
    }
}

pub(super) fn authority(storage: Arc<dyn Persistence>, clock: Arc<TestClock>) -> Result<Authority> {
    Authority::open(storage, clock, Some(bootstrap()))
}

pub(super) fn engine(root: &Path) -> Engine {
    Engine {
        chain: root.join("chain"),
        device_directory: root.join("device"),
    }
}

pub(super) fn storage(root: &Path) -> Result<Arc<FilePersistence>> {
    Ok(Arc::new(FilePersistence::open(root.join("private"))?))
}

pub(super) fn token(expiration: u64) -> Secret {
    Secret(format!(
        "e30.{}.test",
        URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{}}}", expiration / 1000))
    ))
}

pub(super) fn seed(root: &Path, node: u64, sequence: u64, content: &[u8]) -> Result<()> {
    let mut store = SegmentStore::open_wait(root, std::time::Duration::from_secs(5))?;
    BlobStore::new(root.join("blobs"))?.write(content)?;
    let op = Op {
        source: Some(SourceId::new(NodeId(node), 1, sequence)),
        id: OpId::new(NodeId(node), 1, sequence),
        parents: ParentSet::None,
        actor: ActorId(node),
        clock: EngineClock::UnixMs(NOW),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Blob(BlobRef {
                id: ContentId::Hash256(*blake3::hash(content).as_bytes()),
                len: u32::try_from(content.len()).map_err(|_error| Error::Invalid)?,
            }),
            content_type: Payload::Empty,
        }),
    };
    let mut page = Page::new(0);
    page.add_record(0, encode_op(&op).map_err(|_error| Error::Invalid)?);
    Ok(store.append_page(&page)?)
}

pub(super) fn record_count(engine: &Engine) -> Result<usize> {
    Ok(CanonicalChain::read(&engine.chain)?.stats().accepted)
}

pub(super) async fn until(mut condition: impl FnMut() -> bool) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while !condition() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "condition must converge within the lifecycle deadline"
    );
}
