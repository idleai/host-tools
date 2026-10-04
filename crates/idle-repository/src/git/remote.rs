use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GithubRemote {
    pub(crate) owner: String,
    pub(crate) name: String,
}

impl GithubRemote {
    pub(crate) fn parse(remote: &str) -> Option<Self> {
        let url = Url::parse(remote).ok()?;
        if url.scheme() != "https" || url.host_str() != Some("github.com") || url.port().is_some() {
            return None;
        }
        let mut parts = url.path().trim_matches('/').split('/');
        let owner = parts.next()?;
        let name = parts.next()?;
        let name = name.strip_suffix(".git").unwrap_or(name);
        if parts.next().is_some() || !valid_part(owner) || !valid_part(name) {
            return None;
        }
        Some(Self {
            owner: owner.into(),
            name: name.into(),
        })
    }

    pub(crate) fn api_path(&self) -> String {
        format!("/repos/{}/{}", self.owner, self.name)
    }
    pub(crate) fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }
}

fn valid_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

pub(crate) fn sanitize(remote: &str) -> Option<String> {
    let candidate = if remote.contains("://") {
        remote.to_owned()
    } else if let Some((host, path)) = remote.split_once(':') {
        let host = host.rsplit('@').next()?;
        if host.is_empty() || path.is_empty() {
            return None;
        }
        format!("ssh://{host}/{path}")
    } else {
        return Some("Local Git remote".into());
    };
    let mut url = Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "https" | "http" | "ssh" | "git") {
        return Some("Local or unsupported Git remote".into());
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    if url.host_str() == Some("github.com") && url.port().is_none() {
        return Some(format!("https://github.com{}", url.path()));
    }
    Some(url.to_string())
}
