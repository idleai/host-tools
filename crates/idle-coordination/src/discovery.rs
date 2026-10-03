//! Public repository discovery. Directory entries never grant membership or access.

use std::{fmt::Write as _, sync::Arc, time::Duration};

use async_trait::async_trait;
use editchain_sync::PublicDevice;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    invitation::{DISCOVERY_PROTOCOL, RelayEndpoint, valid_id, valid_marker, verify_device},
    peer::PeerCoordinator,
    transport::{Credentials, bounded},
};

/// Existing TypeScript advertisement; includes only the public allowlisted fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Advertisement {
    /// Existing advertisement version, currently one.
    pub version: u8,
    /// Outer discovery protocol, separate from engine peer-v5.
    pub protocol: u16,
    /// Existing JSON encoding, currently one.
    pub encoding: u8,
    /// Bound replication space.
    pub space: String,
    /// Public device identity; discovery cannot approve it.
    pub device: PublicDevice,
    /// Hosting resource marker, stable across reconnect/restart.
    pub instance: String,
    /// Public pinned relay endpoint, without a connect token.
    pub endpoint: RelayEndpoint,
    /// Exclusive freshness boundary in Unix milliseconds.
    pub expires_at: u64,
}

impl Advertisement {
    /// Validate compatibility, public identity, route and bounded freshness.
    ///
    /// # Errors
    /// Rejects unsupported, stale or malformed directory metadata.
    pub fn validate(&self, now: u64) -> Result<()> {
        if self.version != 1 || self.protocol != DISCOVERY_PROTOCOL || self.encoding != 1 {
            return Err(Error::Version);
        }
        if !valid_id(&self.space)
            || !valid_marker(&self.instance)
            || self.expires_at <= now
            || self.expires_at > now.saturating_add(900_000)
        {
            return Err(Error::Invalid);
        }
        verify_device(&self.device)?;
        self.endpoint.validate()
    }

    /// Existing GitHub variable key, isolating simultaneous hosting resources.
    #[must_use]
    pub fn name(&self) -> String {
        let digest = Sha256::digest(format!(
            "{}:{}:{}",
            self.space, self.device.fingerprint, self.instance
        ));
        let suffix = digest
            .iter()
            .take(20)
            .fold(String::new(), |mut text, byte| {
                let _written = write!(text, "{byte:02X}");
                text
            });
        format!("EDITCHAIN_PEER_{suffix}")
    }
}

/// Injected public directory, separate from authorization and active peer streams.
#[async_trait]
pub trait Directory: std::fmt::Debug + Send + Sync {
    /// Read bounded candidates for one already bound space.
    ///
    /// # Errors
    /// Reports unavailable/incomplete discovery without interrupting peer workers.
    async fn read(
        &self,
        space: &str,
        now: u64,
        cancel: &CancellationToken,
    ) -> Result<Vec<Advertisement>>;
    /// Publish only a validated public advertisement.
    ///
    /// # Errors
    /// Reports directory authorization/transport failures.
    async fn publish(
        &self,
        advertisement: &Advertisement,
        now: u64,
        cancel: &CancellationToken,
    ) -> Result<()>;
    /// Remove this hosting resource's entry, preserving other instances.
    ///
    /// # Errors
    /// Reports pending removal; freshness still expires independently.
    async fn remove(&self, advertisement: &Advertisement, cancel: &CancellationToken)
    -> Result<()>;
}

/// Existing GitHub Actions variable discovery, now usable by native hosts.
#[derive(Debug)]
pub struct GitHubDirectory {
    repository: String,
    credentials: Arc<dyn Credentials>,
    http: reqwest::Client,
}

impl GitHubDirectory {
    /// Bind one explicit GitHub `owner/repository`, without repository-wide scans.
    ///
    /// # Errors
    /// Rejects malformed names or HTTP initialization failures.
    pub fn new(repository: &str, credentials: Arc<dyn Credentials>) -> Result<Self> {
        let (owner, name) = repository.split_once('/').ok_or(Error::Invalid)?;
        if owner.is_empty()
            || owner.len() > 39
            || !owner
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || !owner
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || name.is_empty()
            || name.len() > 100
            || matches!(name, "." | "..")
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(Error::Invalid);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_error| Error::Transport)?;
        Ok(Self {
            repository: repository.into(),
            credentials,
            http,
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        suffix: &str,
        body: Option<&serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<(u16, Vec<u8>)> {
        bounded(cancel, Duration::from_secs(10), async {
            let token = self.credentials.management(cancel).await?;
            let mut request = self
                .http
                .request(
                    method,
                    format!(
                        "https://api.github.com/repos/{}/actions/variables{suffix}",
                        self.repository
                    ),
                )
                .bearer_auth(&token.0)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2026-03-10")
                .header("User-Agent", "idle-coordination");
            if let Some(body) = body {
                request = request.json(body);
            }
            let mut response = request.send().await.map_err(|_error| Error::Transport)?;
            let status = response.status().as_u16();
            if !response.status().is_success() && status != 404 {
                return Err(Error::Transport);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_error| Error::Transport)? {
                if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                    return Err(Error::Invalid);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((status, bytes))
        })
        .await
    }
}

#[async_trait]
impl Directory for GitHubDirectory {
    async fn read(
        &self,
        space: &str,
        now: u64,
        cancel: &CancellationToken,
    ) -> Result<Vec<Advertisement>> {
        #[derive(Deserialize)]
        struct Variable {
            name: String,
            value: String,
        }
        #[derive(Deserialize)]
        struct Page {
            variables: Vec<Variable>,
            total_count: u64,
        }
        let mut found = Vec::new();
        for page in 1_u64..=20 {
            let (status, bytes) = self
                .request(
                    reqwest::Method::GET,
                    &format!("?per_page=30&page={page}"),
                    None,
                    cancel,
                )
                .await?;
            if status == 404 {
                return Err(Error::Forbidden);
            }
            let response: Page = serde_json::from_slice(&bytes)?;
            if response.variables.len() > 30 {
                return Err(Error::Invalid);
            }
            for variable in &response.variables {
                if !variable.name.starts_with("EDITCHAIN_PEER_") || variable.value.len() > 32 * 1024
                {
                    continue;
                }
                if let Ok(candidate) = serde_json::from_str::<Advertisement>(&variable.value)
                    && candidate.space == space
                    && candidate.validate(now).is_ok()
                    && variable.name == candidate.name()
                    && found.len() < 32
                {
                    found.push(candidate);
                }
            }
            if response.variables.len() < 30 || page.saturating_mul(30) >= response.total_count {
                return Ok(found);
            }
        }
        Err(Error::Busy)
    }

    async fn publish(
        &self,
        advertisement: &Advertisement,
        now: u64,
        cancel: &CancellationToken,
    ) -> Result<()> {
        advertisement.validate(now)?;
        let value = serde_json::to_string(advertisement)?;
        if value.len() > 32 * 1024 {
            return Err(Error::Invalid);
        }
        let name = advertisement.name();
        let body = serde_json::json!({ "name": name, "value": value });
        let (status, _bytes) = self
            .request(
                reqwest::Method::PATCH,
                &format!("/{name}"),
                Some(&body),
                cancel,
            )
            .await?;
        if status == 404 {
            let (status, _bytes) = self
                .request(reqwest::Method::POST, "", Some(&body), cancel)
                .await?;
            if status == 404 {
                return Err(Error::Forbidden);
            }
        }
        Ok(())
    }

    async fn remove(
        &self,
        advertisement: &Advertisement,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _response = self
            .request(
                reqwest::Method::DELETE,
                &format!("/{}", advertisement.name()),
                None,
                cancel,
            )
            .await?;
        Ok(())
    }
}

/// Serialized directory upkeep. The host schedules refreshes independently from peers.
#[derive(Debug)]
pub struct DirectorySync {
    directory: Arc<dyn Directory>,
    published: Option<Advertisement>,
}

impl DirectorySync {
    /// Construct upkeep for an explicitly configured public repository directory.
    #[must_use]
    pub fn new(directory: Arc<dyn Directory>) -> Self {
        Self {
            directory,
            published: None,
        }
    }

    /// Publish and read once. Failures do not stop the coordinator's active streams.
    ///
    /// # Errors
    /// Returns unavailable discovery or rejected local persistence.
    pub async fn refresh(
        &mut self,
        coordinator: &mut PeerCoordinator,
        now: u64,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let Some(space) = coordinator.status().space else {
            return Ok(());
        };
        if let Some(advertisement) = coordinator.describe(cancel).await? {
            self.published = Some(advertisement.clone());
            self.directory.publish(&advertisement, now, cancel).await?;
        }
        let candidates = self.directory.read(&space, now, cancel).await?;
        coordinator.discover(&candidates).await
    }

    /// Await removal of the last published resource; retain its key on failure.
    ///
    /// # Errors
    /// Returns pending directory cleanup, whose record still has a short expiry.
    pub async fn stop(&mut self, cancel: &CancellationToken) -> Result<()> {
        if let Some(advertisement) = &self.published {
            self.directory.remove(advertisement, cancel).await?;
            self.published = None;
        }
        Ok(())
    }
}
