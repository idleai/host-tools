//! The existing `editchain:` invitation and saved-sharing JSON formats.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use editchain_sync::PublicDevice;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{Error, Result};

/// Relay port shared with the TypeScript coordinator.
pub const MULTIPLAYER_PORT: u16 = 43188;
/// Outer discovery protocol. The encrypted engine protocol remains peer-v5.
pub const DISCOVERY_PROTOCOL: u16 = 3;
/// Maximum encoded invitation size.
pub const INVITATION_LIMIT: usize = 32 * 1024;

/// A bearer credential whose debug representation never contains its value.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

/// Fixed join-request discriminator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RequestKind {
    /// A request to approve this exact device.
    #[serde(rename = "request")]
    Request,
}

/// Fixed invitation discriminator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InviteKind {
    /// A grant for an already approved guest.
    #[serde(rename = "invite")]
    Invite,
}

/// Public identity submitted through a trusted approval channel.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoinRequest {
    /// Existing format version, currently one.
    pub version: u8,
    /// Request discriminator.
    pub kind: RequestKind,
    /// Exact guest certificate and fingerprint.
    pub device: PublicDevice,
}

/// Public, pinned relay route. It carries no connect credential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpoint {
    /// Allocated tunnel identity.
    pub tunnel_id: String,
    /// Allocated Microsoft relay cluster.
    pub cluster_id: String,
    /// Host instance registered on this tunnel.
    pub host_id: String,
    /// Microsoft WSS relay address.
    pub client_relay_uri: String,
    /// Approved SSH host keys from the descriptor.
    pub host_public_keys: Vec<String>,
}

/// Existing invitation, containing a bearer grant; persist only in private storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    /// Existing format version, currently one.
    pub version: u8,
    /// Invitation discriminator.
    pub kind: InviteKind,
    /// Bound replication space.
    pub space: String,
    /// Approved host certificate, separate from the relay's SSH key.
    pub host: PublicDevice,
    /// Exact invited guest fingerprint.
    pub guest: String,
    /// Route within the approved tunnel.
    pub endpoint: RelayEndpoint,
    /// Grant accepted and verified by the Microsoft relay.
    pub connect_token: Secret,
    /// Invitation acceptance deadline in Unix milliseconds.
    pub expires_at: u64,
}

/// Owner marker and exact cloud resource, preserving the TypeScript lease shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLease {
    /// Random resource label used to resolve uncertain creation and deletion.
    pub marker: String,
    /// Exact resource to resume or remove.
    pub tunnel_id: String,
    /// Exact cluster containing the resource.
    pub cluster_id: String,
}

/// Version-one saved sharing. Expiry does not erase consent or change its boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SavedSharing {
    /// Existing format version, currently one.
    pub version: u8,
    /// Existing engine binding.
    pub space: String,
    /// Resource retained across service suspension/restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostLease>,
    /// Invitations for previously approved devices.
    pub peers: Vec<Invitation>,
}

impl JoinRequest {
    /// Decode and recompute the guest certificate fingerprint.
    ///
    /// # Errors
    /// Rejects malformed, oversized, unsupported or mismatched identities.
    pub fn parse(text: &str) -> Result<Self> {
        let value: Self = decode(text)?;
        if value.version != 1 {
            return Err(Error::Version);
        }
        verify_device(&value.device)?;
        Ok(value)
    }
}

impl RelayEndpoint {
    /// Validate a Microsoft-only relay address and bounded host keys.
    ///
    /// # Errors
    /// Rejects userinfo, fragments, non-TLS ports, foreign domains or invalid IDs.
    pub fn validate(&self) -> Result<()> {
        if ![&self.tunnel_id, &self.cluster_id, &self.host_id]
            .into_iter()
            .all(|id| valid_id(id))
            || self.client_relay_uri.len() > 4096
            || self.host_public_keys.is_empty()
            || self.host_public_keys.len() > 4
            || self.host_public_keys.iter().any(|key| {
                key.is_empty()
                    || key.len() > 4096
                    || !key
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
            })
        {
            return Err(Error::Invalid);
        }
        let url = url::Url::parse(&self.client_relay_uri).map_err(|_error| Error::Invalid)?;
        let label = url
            .host_str()
            .and_then(|host| host.strip_suffix(".rel.tunnels.api.visualstudio.com"));
        if url.scheme() != "wss"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port().is_some_and(|port| port != 443)
            || !label.is_some_and(|label| {
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            })
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

impl Invitation {
    /// Accept a fresh invitation addressed to this device.
    ///
    /// # Errors
    /// Rejects expired invitations, invalid identities and unapproved relay routes.
    pub fn parse(text: &str, guest: &PublicDevice, now: u64) -> Result<Self> {
        let invitation: Self = decode(text)?;
        invitation.validate(guest, now, false)?;
        Ok(invitation)
    }

    /// Validate a saved invitation without extending its underlying service grant.
    /// `enrolled` permits an elapsed invitation acceptance deadline only.
    ///
    /// # Errors
    /// Returns invalid, expired, incompatible or wrong-device failures.
    pub fn validate(&self, guest: &PublicDevice, now: u64, enrolled: bool) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Version);
        }
        verify_device(&self.host)?;
        verify_device(guest)?;
        if !valid_id(&self.space) || self.guest != guest.fingerprint || self.host == *guest {
            return Err(Error::Forbidden);
        }
        self.endpoint.validate()?;
        if self.expires_at > now.saturating_add(86_400_000)
            || self.expires_at > 9_007_199_254_740_991
        {
            return Err(Error::Invalid);
        }
        let expiration = token_expiration(&self.connect_token)?;
        if (!enrolled && self.expires_at <= now)
            || expiration <= now
            || expiration < self.expires_at
        {
            return Err(Error::Expired);
        }
        Ok(())
    }

    /// Require renewal to stay inside the same device, space and tunnel approval.
    /// Relay host instances and SSH keys may rotate within that authenticated route.
    ///
    /// # Errors
    /// Rejects changes to the approved scope or expired replacements.
    pub fn validate_renewal(
        &self,
        replacement: &Self,
        guest: &PublicDevice,
        now: u64,
    ) -> Result<()> {
        replacement.validate(guest, now, true)?;
        if self.space != replacement.space
            || self.host != replacement.host
            || self.guest != replacement.guest
            || self.endpoint.tunnel_id != replacement.endpoint.tunnel_id
            || self.endpoint.cluster_id != replacement.endpoint.cluster_id
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}

impl HostLease {
    /// Validate owner marker and resource identity before management operations.
    ///
    /// # Errors
    /// Rejects invalid markers or IDs.
    pub fn validate(&self) -> Result<()> {
        if !valid_marker(&self.marker) || !valid_id(&self.tunnel_id) || !valid_id(&self.cluster_id)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

impl SavedSharing {
    /// Validate the saved format and limits without granting access to any peer.
    ///
    /// # Errors
    /// Rejects unknown versions, invalid bindings or excessive peers.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Version);
        }
        if !valid_id(&self.space) || self.peers.len() > 32 {
            return Err(Error::Invalid);
        }
        if let Some(host) = &self.host {
            host.validate()?;
        }
        Ok(())
    }
}

/// Encode the existing URL-safe invitation format.
///
/// # Errors
/// Rejects serialization failures or invitations above the wire size bound.
pub fn encode(value: &impl Serialize) -> Result<String> {
    let result = format!(
        "editchain:{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?)
    );
    if result.len() > INVITATION_LIMIT {
        return Err(Error::Invalid);
    }
    Ok(result)
}

fn decode<T: DeserializeOwned>(text: &str) -> Result<T> {
    let source = text.trim();
    if source.len() > INVITATION_LIMIT {
        return Err(Error::Invalid);
    }
    let payload = source.strip_prefix("editchain:").ok_or(Error::Invalid)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_error| Error::Invalid)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Parse only the public expiry claim. The relay verifies the signed credential.
///
/// # Errors
/// Rejects oversized, malformed tokens or missing/overflowing expiry claims.
pub fn token_expiration(token: &Secret) -> Result<u64> {
    if token.0.len() > 8192 {
        return Err(Error::Invalid);
    }
    let mut parts = token.0.split('.');
    let _header = parts.next().ok_or(Error::Invalid)?;
    let payload = parts.next().ok_or(Error::Invalid)?;
    let _signature = parts.next().ok_or(Error::Invalid)?;
    if parts.next().is_some() {
        return Err(Error::Invalid);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_error| Error::Invalid)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes)?;
    claims
        .get("exp")
        .and_then(serde_json::Value::as_u64)
        .and_then(|seconds| seconds.checked_mul(1000))
        .ok_or(Error::Invalid)
}

pub(crate) fn verify_device(device: &PublicDevice) -> Result<()> {
    if PublicDevice::parse(&device.certificate)? != *device {
        return Err(Error::Forbidden);
    }
    Ok(())
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

pub(crate) fn valid_marker(value: &str) -> bool {
    value
        .strip_prefix("idle-relay-")
        .or_else(|| value.strip_prefix("editchain-multiplayer-"))
        .is_some_and(|suffix| {
            suffix.len() == 24
                && suffix
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
}
