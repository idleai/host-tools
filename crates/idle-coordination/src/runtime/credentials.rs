use std::{path::PathBuf, process::Stdio, time::Duration};

use async_trait::async_trait;
use tokio::{io::AsyncReadExt as _, process::Command};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    invitation::{Invitation, Secret},
    persistence::Persistence,
    transport::{Credentials, bounded},
};

#[derive(Debug)]
pub(super) struct GitHubCli(pub(super) PathBuf);

#[async_trait]
impl Credentials for GitHubCli {
    async fn management(&self, cancel: &CancellationToken) -> Result<Secret> {
        if !self.0.is_absolute() {
            return Err(Error::Invalid);
        }
        bounded(cancel, Duration::from_secs(20), async {
            let mut child = Command::new(&self.0)
                .args(["auth", "token", "--hostname", "github.com"])
                .env("GH_PROMPT_DISABLED", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_error| Error::Forbidden)?;
            let mut bytes = Vec::new();
            let _read = child
                .stdout
                .take()
                .ok_or(Error::Forbidden)?
                .take(16_385)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_error| Error::Forbidden)?;
            if bytes.len() > 16_384 || !child.wait().await?.success() {
                return Err(Error::Forbidden);
            }
            let token = String::from_utf8(bytes).map_err(|_error| Error::Forbidden)?;
            let token = token.trim();
            if token.is_empty() {
                return Err(Error::Forbidden);
            }
            Ok(Secret(token.into()))
        })
        .await
    }

    async fn renew(
        &self,
        _invitation: &Invitation,
        _cancel: &CancellationToken,
    ) -> Result<Option<Invitation>> {
        Ok(None)
    }
}

#[derive(Debug)]
pub(super) struct NoManagement;

#[async_trait]
impl Credentials for NoManagement {
    async fn management(&self, _cancel: &CancellationToken) -> Result<Secret> {
        Err(Error::Forbidden)
    }

    async fn renew(
        &self,
        _invitation: &Invitation,
        _cancel: &CancellationToken,
    ) -> Result<Option<Invitation>> {
        Ok(None)
    }
}

impl Persistence for NoManagement {
    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn compare_exchange(
        &self,
        _key: &str,
        _previous: Option<&[u8]>,
        _replacement: Option<&[u8]>,
    ) -> Result<()> {
        Err(Error::Forbidden)
    }
}
