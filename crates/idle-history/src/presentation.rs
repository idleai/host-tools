//! Bounded row display text. Full source content is addressed by row identity.

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 bytes in one authored summary or output preview.
pub const MAX_ROW_TEXT_BYTES: usize = 4096;
/// Maximum UTF-8 bytes in a displayed tool label.
pub const MAX_TOOL_LABEL_BYTES: usize = 256;

/// Text read from a recorded field, with explicit completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet generates unsafe reflection helpers; these fields have no safety invariants"
)]
pub struct ContentText {
    /// Plain or authored Markdown text, without a provider JSON envelope.
    pub text: String,
    /// True only when the complete source text survived preparation and this
    /// field's byte limit. Derived summaries and text without a known source
    /// field use false.
    #[serde(default)]
    pub complete: bool,
}

impl ContentText {
    /// Bound an authored summary or output preview without splitting UTF-8.
    #[must_use]
    pub fn new(text: String, complete: bool) -> Self {
        Self::bounded(text, complete, MAX_ROW_TEXT_BYTES)
    }

    /// Bound a tool's display name. The source tool identity is unchanged.
    #[must_use]
    pub fn tool_label(text: String, complete: bool) -> Self {
        Self::bounded(text, complete, MAX_TOOL_LABEL_BYTES)
    }

    fn bounded(mut text: String, complete: bool, limit: usize) -> Self {
        if text.len() <= limit {
            return Self { text, complete };
        }
        let mut end = limit.saturating_sub('…'.len_utf8());
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
        text.push('…');
        text.shrink_to_fit();
        Self {
            text,
            complete: false,
        }
    }
}

/// Source-selected content roles. Absence of this additive DTO identifies a
/// legacy row whose summary needs compatibility recovery at ingestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, facet::Facet)]
#[expect(
    clippy::unsafe_derive_deserialize,
    reason = "Facet generates unsafe reflection helpers; these fields have no safety invariants"
)]
pub struct RowContent {
    /// Tool name read from the recorded tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_label: Option<ContentText>,
    /// Authored prose, invocation, or aggregate activity summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_summary: Option<ContentText>,
    /// Output from a tool or command. Invocation text takes display priority
    /// when both roles are present; output remains separately identifiable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<ContentText>,
}

impl RowContent {
    /// Check untrusted decoded text before a renderer publishes the row.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.tool_label
            .as_ref()
            .is_none_or(|text| text.text.len() <= MAX_TOOL_LABEL_BYTES)
            && self
                .authored_summary
                .as_ref()
                .is_none_or(|text| text.text.len() <= MAX_ROW_TEXT_BYTES)
            && self
                .output_preview
                .as_ref()
                .is_none_or(|text| text.text.len() <= MAX_ROW_TEXT_BYTES)
    }

    /// Selected visible content, preferring nonempty authored text over output.
    #[must_use]
    pub fn display_text(&self) -> Option<&ContentText> {
        self.authored_summary
            .as_ref()
            .filter(|text| !text.text.is_empty())
            .or(self.output_preview.as_ref())
            .or(self.authored_summary.as_ref())
    }
}
