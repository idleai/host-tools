use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, invitation::Secret, transport::RelayDescriptor};

/// Private invitation for one approved Codex workspace connection.
/// It grants no `EditChain` sharing, shell access or model execution.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeInvitation {
    /// Version one permits attachment; version two can grant coordination ownership.
    pub version: u32,
    /// Expected persistent Codex installation identity.
    pub host_id: String,
    /// Workspace approved by the daemon owner.
    pub workspace_id: String,
    /// Repository within that workspace.
    pub repository_id: String,
    /// Exact checkout on the compute host.
    pub checkout_id: String,
    /// Shared logical history identity.
    pub chain_id: String,
    /// Principal recorded by the daemon owner for this grant.
    pub client_id: String,
    /// Persistent grant identity; the secret is checked by Codex.
    pub grant_id: String,
    /// Private runtime credential, separate from the relay connect credential.
    pub grant_token: Secret,
    /// Relay descriptor for the dedicated runtime port.
    pub relay: RelayDescriptor,
    /// Runtime grant expiry in Unix milliseconds.
    pub expires_at: u64,
    /// Version two can explicitly allow standalone coordination ownership.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coordination_owner: bool,
}

impl RuntimeInvitation {
    /// Decode a bounded invitation and validate its route before network access.
    /// # Errors
    /// Rejects malformed, expired or unsupported invitations.
    pub fn parse(text: &str, now: u64) -> Result<Self> {
        if text.len() > 32 * 1024 {
            return Err(Error::Invalid);
        }
        let encoded = text.strip_prefix("idle-runtime:").ok_or(Error::Invalid)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_error| Error::Invalid)?;
        let value: Self = serde_json::from_slice(&bytes).map_err(|_error| Error::Invalid)?;
        value.validate(now)?;
        Ok(value)
    }

    /// Encode a private invitation for approved transfer to the named client.
    /// # Errors
    /// Rejects invalid fields or expired credentials.
    pub fn encode(&self, now: u64) -> Result<Secret> {
        self.validate(now)?;
        let bytes = serde_json::to_vec(self).map_err(|_error| Error::Invalid)?;
        let value = format!("idle-runtime:{}", URL_SAFE_NO_PAD.encode(bytes));
        if value.len() > 32 * 1024 {
            return Err(Error::Invalid);
        }
        Ok(Secret(value))
    }

    fn validate(&self, now: u64) -> Result<()> {
        if !matches!(
            (self.version, self.coordination_owner),
            (1, false) | (2, true)
        ) {
            return Err(Error::Version);
        }
        if self.expires_at <= now || self.relay.expires_at <= now {
            return Err(Error::Expired);
        }
        for id in [
            &self.host_id,
            &self.workspace_id,
            &self.repository_id,
            &self.checkout_id,
            &self.chain_id,
            &self.client_id,
            &self.grant_id,
        ] {
            if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                return Err(Error::Invalid);
            }
        }
        if self.grant_token.0.len() < 32
            || self.grant_token.0.len() > 256
            || self.relay.connect_token.0.is_empty()
            || self.relay.connect_token.0.len() > 16_384
        {
            return Err(Error::Invalid);
        }
        self.relay.endpoint.validate()
    }
}
