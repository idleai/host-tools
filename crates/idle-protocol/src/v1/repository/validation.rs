use std::collections::BTreeSet;

use super::{RepositorySnapshot, SourceRecord};

fn identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl SourceRecord {
    /// Validate every full immutable and logical address before history navigation.
    ///
    /// # Errors
    /// Rejects prefixes, uppercase encodings or missing stored hashes.
    pub fn validate(&self) -> Result<(), &'static str> {
        if [&self.observation, &self.item, &self.record_hash]
            .iter()
            .all(|value| identity(value))
        {
            Ok(())
        } else {
            Err("Repository sources require complete lowercase history identities.")
        }
    }
}

impl RepositorySnapshot {
    /// Validate replacement shape, bounded lists and exact session source addresses.
    /// Hosts still authorize the scope and independently resolve all paths.
    ///
    /// # Errors
    /// Rejects incomplete scopes, invalid source addresses, duplicates or oversized lists.
    pub fn validate(&self) -> Result<(), &'static str> {
        if [
            &self.scope.workspace_id,
            &self.scope.repository_id,
            &self.scope.chain,
        ]
        .iter()
        .any(|value| value.is_empty() || value.len() > 1024)
        {
            return Err("Repository input requires one complete workspace/repository/chain scope.");
        }
        if self.sessions.len() > 1000
            || self.git_authors.len() > 500
            || self.contributors.len() > 100
            || self.collaborators.len() > 100
            || self.reports.is_empty()
            || self.reports.len() > 20
        {
            return Err("Repository input exceeds its declared read limits.");
        }
        let mut sessions = BTreeSet::new();
        let mut observations = BTreeSet::new();
        for session in &self.sessions {
            if !identity(&session.id)
                || !sessions.insert(&session.id)
                || session.records.is_empty()
                || session.records.len() > 1000
                || session.labels.len() > 1000
                || session.sources.len() > 1000
                || session.actions.len() > 4
            {
                return Err(
                    "Recorded sessions require distinct full identities and supplied source records.",
                );
            }
            for record in &session.records {
                record.validate()?;
                if record.item != session.id || !observations.insert(&record.observation) {
                    return Err("Session source records must belong to that session exactly once.");
                }
            }
        }
        if observations.len() > 1000 {
            return Err("Recorded sessions exceed the 1,000-observation read limit.");
        }
        for report in &self.reports {
            if report.topic.is_empty()
                || report.topic.len() > 128
                || report.message.is_empty()
                || report.message.len() > 8192
            {
                return Err("Repository read reports require a bounded topic and explanation.");
            }
        }
        for people in [&self.contributors, &self.collaborators] {
            let mut ids = BTreeSet::new();
            for person in people {
                if person.id.is_empty()
                    || person.id.len() > 20
                    || !person.id.bytes().all(|byte| byte.is_ascii_digit())
                    || person.login.is_empty()
                    || !ids.insert(&person.id)
                    || !github_url(&person.url)
                {
                    return Err(
                        "GitHub people require distinct account identities and safe source URLs.",
                    );
                }
            }
        }
        if self.github.as_ref().is_some_and(|repository| {
            repository.full_name.is_empty() || !github_url(&repository.url)
        }) {
            return Err("GitHub repository metadata requires a name and safe source URL.");
        }
        Ok(())
    }
}

pub(super) fn github_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        && url.len() <= 2048
        && !url.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == '\\'
        })
}
