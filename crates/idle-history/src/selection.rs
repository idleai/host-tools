//! Stable semantic selection for shared history views.

/// Stable semantic selection extracted from the old history renderer.
/// Roving focus, row locations and scrolling remain with the renderer.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    key: Option<String>,
    continuity: Option<String>,
}

impl Selection {
    /// Current stable item key.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// Select an item with the same continuity identity.
    pub fn select(&mut self, key: &str) {
        self.select_with_continuity(key, key);
    }

    /// Retain logical continuity when a legacy projection changes its row key.
    pub fn select_with_continuity(&mut self, key: &str, continuity: &str) {
        self.key = Some(key.to_owned());
        self.continuity = Some(continuity.to_owned());
    }

    /// Identity used to restore selection after a projection changes.
    #[must_use]
    pub fn continuity_key(&self) -> Option<&str> {
        self.continuity.as_deref()
    }

    /// Clear semantic selection.
    pub fn clear(&mut self) {
        self.key = None;
        self.continuity = None;
    }
}
