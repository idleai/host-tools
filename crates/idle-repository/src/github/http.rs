//! Conditional HTTPS reads. Tokens, error bodies and arbitrary redirects stay out of results.

use std::{collections::BTreeMap, io, time::Duration};

use reqwest::{
    Client, StatusCode,
    header::{HeaderMap, HeaderValue},
};
use url::Url;

use crate::Credentials;

const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Page {
    pub(super) bytes: Vec<u8>,
    pub(super) more: bool,
}

#[derive(Clone, Debug)]
struct Cached {
    etag: Option<HeaderValue>,
    page: Page,
}

#[derive(Clone, Debug)]
pub(super) struct Failure {
    pub(super) message: String,
    pub(super) retry_at_ms: Option<u64>,
    pub(super) unauthorized: bool,
}

impl Failure {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_at_ms: None,
            unauthorized: false,
        }
    }
}

#[derive(Debug)]
pub(super) struct Http {
    client: Client,
    origin: Url,
    audience: Option<[u8; 32]>,
    cache: BTreeMap<String, Cached>,
    blocked: Option<Failure>,
}

impl Http {
    pub(super) fn new() -> io::Result<Self> {
        let client = Client::builder()
            .user_agent("idle-repository/0.1")
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .https_only(true)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            origin: Url::parse("https://api.github.com").map_err(io::Error::other)?,
            audience: None,
            cache: BTreeMap::new(),
            blocked: None,
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(origin: Url) -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .expect("test client"),
            origin,
            audience: None,
            cache: BTreeMap::new(),
            blocked: None,
        }
    }

    pub(super) fn bind(&mut self, repository: &str, credentials: Option<&Credentials>) {
        let mut hash = blake3::Hasher::new();
        let _hash = hash.update(repository.as_bytes());
        if let Some(credentials) = credentials {
            let _hash = hash
                .update(b"\0")
                .update(credentials.account.as_bytes())
                .update(b"\0")
                .update(credentials.token.as_bytes());
        }
        let audience = *hash.finalize().as_bytes();
        if self.audience != Some(audience) {
            self.cache.clear();
            self.blocked = None;
            self.audience = Some(audience);
        }
    }

    pub(super) fn forget(&mut self) {
        self.cache.clear();
    }

    pub(super) async fn get(
        &mut self,
        path: &str,
        credentials: Option<&Credentials>,
        now: u64,
    ) -> Result<Page, Failure> {
        if let Some(blocked) = &self.blocked {
            if blocked.retry_at_ms.is_some_and(|retry| retry > now) {
                return Err(blocked.clone());
            }
            self.blocked = None;
        }
        if !path.starts_with("/repos/")
            || path.starts_with("//")
            || path.contains(['\\', '\r', '\n', '#'])
        {
            return Err(Failure::new("Invalid GitHub repository endpoint."));
        }
        let url = self
            .origin
            .join(path)
            .map_err(|_error| Failure::new("Invalid GitHub repository endpoint."))?;
        if url.origin() != self.origin.origin() {
            return Err(Failure::new("GitHub endpoint changed origin."));
        }
        let mut request = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10");
        if let Some(credentials) = credentials {
            request = request.bearer_auth(&credentials.token);
        }
        if let Some(etag) = self.cache.get(path).and_then(|cached| cached.etag.clone()) {
            request = request.header("If-None-Match", etag);
        }
        let mut response = request.send().await.map_err(|_error| Failure::new("GitHub could not be reached within the read deadline. Local Git and recorded history remain available."))?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return self
                .cache
                .get(path)
                .map(|cached| cached.page.clone())
                .ok_or_else(|| {
                    Failure::new(
                        "GitHub returned a conditional result without a matching cached response.",
                    )
                });
        }
        if response.status() != StatusCode::OK {
            let failure = status_failure(response.status(), response.headers(), now);
            let _old = self.cache.remove(path);
            if failure.unauthorized {
                self.cache.clear();
            }
            if failure.retry_at_ms.is_some() {
                self.blocked = Some(failure.clone());
            }
            return Err(failure);
        }
        if response
            .content_length()
            .is_some_and(|length| length > u64::try_from(MAX_BODY).unwrap_or(u64::MAX))
        {
            let _old = self.cache.remove(path);
            return Err(Failure::new(
                "GitHub response exceeds the two-MiB read limit.",
            ));
        }
        let etag = response.headers().get("etag").cloned();
        // Pagination always constructs the next bounded page on our fixed origin.
        // A Link header can signal more pages, but can never choose a destination.
        let more = response
            .headers()
            .get("link")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(',').any(|link| {
                    link.split(';')
                        .skip(1)
                        .any(|part| part.trim() == "rel=\"next\"")
                })
            });
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_error| Failure::new("GitHub response was interrupted."))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
                let _old = self.cache.remove(path);
                return Err(Failure::new(
                    "GitHub response exceeds the two-MiB read limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let page = Page { bytes, more };
        if self.cache.len() >= 16 && !self.cache.contains_key(path) {
            let _old = self.cache.pop_first();
        }
        let _old = self.cache.insert(
            path.into(),
            Cached {
                etag,
                page: page.clone(),
            },
        );
        Ok(page)
    }
}

fn status_failure(status: StatusCode, headers: &HeaderMap, now: u64) -> Failure {
    let number = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    };
    let retry =
        number("retry-after").map(|seconds| now.saturating_add(seconds.saturating_mul(1000)));
    let limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (number("x-ratelimit-remaining") == Some(0) || retry.is_some()));
    if limited {
        return Failure {
            message: "GitHub rate limit reached. Retry after the reported reset time.".into(),
            retry_at_ms: Some(
                retry
                    .or_else(|| {
                        number("x-ratelimit-reset").map(|seconds| seconds.saturating_mul(1000))
                    })
                    .unwrap_or_else(|| now.saturating_add(60_000))
                    .max(now.saturating_add(1000)),
            ),
            unauthorized: false,
        };
    }
    let message = match status {
        StatusCode::UNAUTHORIZED => "GitHub authentication expired or was rejected. Sign in again.",
        StatusCode::FORBIDDEN => "This GitHub account cannot read this repository resource.",
        StatusCode::NOT_FOUND => {
            "This GitHub resource does not exist or is not visible to this account."
        }
        StatusCode::MOVED_PERMANENTLY
        | StatusCode::FOUND
        | StatusCode::TEMPORARY_REDIRECT
        | StatusCode::PERMANENT_REDIRECT => {
            "GitHub redirected this repository. Update the Git remote before refreshing."
        }
        _ => "GitHub could not complete this repository read. Refresh to retry.",
    };
    Failure {
        message: message.into(),
        retry_at_ms: None,
        unauthorized: status == StatusCode::UNAUTHORIZED,
    }
}
