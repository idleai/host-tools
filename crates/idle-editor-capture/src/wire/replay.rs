//! Validate full snapshots against exact VS Code UTF-16 replacements.

use super::{EditorChange, MAX_EDITOR_BUFFER_BYTES};

pub(super) fn validate(
    before: &str,
    after: &str,
    changes: &[EditorChange],
) -> Result<(), &'static str> {
    // Ordinary typing changes one range. Compare the complete prefix, inserted
    // text, and suffix directly, avoiding two whole-buffer UTF-16 conversions.
    // Surrogate-interior offsets keep the exact UTF-16 replay below.
    if let [change] = changes
        && let Some(matches) = single(before, after, change)
    {
        return result(matches);
    }
    let mut text: Vec<_> = before.encode_utf16().collect();
    // Native multi-cursor changes run from the end toward the start. Compare
    // their disjoint pieces once instead of moving the suffix for every cursor.
    if let Some(matches) = descending(&text, after, changes) {
        return result(matches);
    }
    for change in changes {
        let start = usize::try_from(change.offset).map_err(|_error| "invalid edit offset")?;
        let end = start
            .checked_add(usize::try_from(change.length).map_err(|_error| "invalid edit length")?)
            .ok_or("edit overflow")?;
        if start > end || end > text.len() {
            return Err("edit outside source buffer");
        }
        drop(text.splice(start..end, change.text.encode_utf16()));
        if text.len() > MAX_EDITOR_BUFFER_BYTES.saturating_mul(2) {
            return Err("editor buffer exceeds capture limit");
        }
    }
    result(text.iter().copied().eq(after.encode_utf16()))
}

fn descending(before: &[u16], after: &str, changes: &[EditorChange]) -> Option<bool> {
    let mut boundary = before.len();
    for change in changes {
        let start = usize::try_from(change.offset).ok()?;
        let end = start.checked_add(usize::try_from(change.length).ok()?)?;
        if end > boundary {
            return None;
        }
        boundary = start;
    }
    let mut actual = after.encode_utf16();
    let mut cursor = 0;
    for change in changes.iter().rev() {
        let start = usize::try_from(change.offset).ok()?;
        let end = start.checked_add(usize::try_from(change.length).ok()?)?;
        let expected = before
            .get(cursor..start)?
            .iter()
            .copied()
            .chain(change.text.encode_utf16());
        for unit in expected {
            if actual.next() != Some(unit) {
                return Some(false);
            }
        }
        cursor = end;
    }
    Some(before.get(cursor..)?.iter().copied().eq(actual))
}

fn single(before: &str, after: &str, change: &EditorChange) -> Option<bool> {
    let range = change.byte_range(before)?;
    let start = range.start;
    let end = range.end;
    let inserted_end = start.checked_add(change.text.len())?;
    let suffix = before.get(end..)?;
    let expected_length = inserted_end.checked_add(suffix.len())?;
    Some(
        after.len() == expected_length
            && after.get(..start) == before.get(..start)
            && after.get(start..inserted_end) == Some(change.text.as_str())
            && after.get(inserted_end..) == Some(suffix),
    )
}

impl EditorChange {
    /// Resolve the UTF-16 replacement to UTF-8 byte boundaries in the source.
    /// Returns `None` for out-of-range or surrogate-interior offsets.
    #[must_use]
    pub fn byte_range(&self, before: &str) -> Option<std::ops::Range<usize>> {
        let start = utf16_boundary(before, usize::try_from(self.offset).ok()?)?;
        let removed = utf16_boundary(before.get(start..)?, usize::try_from(self.length).ok()?)?;
        Some(start..start.checked_add(removed)?)
    }
}

fn utf16_boundary(text: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    if text.is_ascii() {
        return (target <= text.len()).then_some(target);
    }
    let mut units = 0_usize;
    for (byte, character) in text.char_indices() {
        if units == target {
            return Some(byte);
        }
        units = units.saturating_add(character.len_utf16());
        if units > target {
            return None;
        }
    }
    (units == target).then_some(text.len())
}

fn result(matches: bool) -> Result<(), &'static str> {
    if matches {
        Ok(())
    } else {
        Err("editor changes do not replay to recorded after text")
    }
}

#[cfg(test)]
mod tests {
    use super::{EditorChange, validate};

    #[test]
    fn single_range_validation_matches_utf16_semantics_at_every_unicode_boundary() {
        let before = "a😀b🦀éz";
        let units: Vec<_> = before.encode_utf16().collect();
        for start in 0..=units.len() {
            for end in start..=units.len() {
                for replacement in ["", "X", "🙂", "e\u{301}"] {
                    let mut expected = units.clone();
                    drop(expected.splice(start..end, replacement.encode_utf16()));
                    let changes = [EditorChange {
                        offset: u32::try_from(start).expect("fixture offset"),
                        length: u32::try_from(end.saturating_sub(start)).expect("fixture length"),
                        text: replacement.into(),
                    }];
                    match String::from_utf16(&expected) {
                        Ok(after) => {
                            assert!(
                                validate(before, &after, &changes).is_ok(),
                                "valid UTF-16 replacement {start}..{end}"
                            );
                            assert!(
                                validate(before, &format!("{after}!"), &changes).is_err(),
                                "incorrect after-text must fail"
                            );
                        }
                        Err(_) => assert!(
                            validate(before, before, &changes).is_err(),
                            "unpaired surrogate cannot match valid UTF-8"
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn multiple_changes_keep_emitted_order_and_invalid_offsets_are_rejected() {
        let changes = [
            EditorChange {
                offset: 1,
                length: 2,
                text: "X".into(),
            },
            EditorChange {
                offset: 2,
                length: 1,
                text: "Y".into(),
            },
        ];
        assert!(
            validate("a😀b", "aXY", &changes).is_ok(),
            "ordered multi-range replay"
        );
        assert!(
            validate("a😀b", "aXY!", &changes).is_err(),
            "full destination comparison"
        );
        let outside = [EditorChange {
            offset: 9,
            length: 0,
            text: String::new(),
        }];
        assert!(
            validate("a😀b", "a😀b", &outside).is_err(),
            "out-of-range empty changes still fail"
        );
    }

    #[test]
    fn disjoint_changes_match_ordered_utf16_splices_including_adjacent_insertions() {
        let before = "a😀b🦀éz";
        let units: Vec<_> = before.encode_utf16().collect();
        for split in 0..=units.len() {
            for start in 0..=split {
                for end in split..=units.len() {
                    for text in ["", "X", "🙂"] {
                        let changes = [
                            EditorChange {
                                offset: u32::try_from(split).expect("offset"),
                                length: u32::try_from(end.saturating_sub(split)).expect("length"),
                                text: text.into(),
                            },
                            EditorChange {
                                offset: u32::try_from(start).expect("offset"),
                                length: u32::try_from(split.saturating_sub(start)).expect("length"),
                                text: "Y".into(),
                            },
                        ];
                        let mut expected = units.clone();
                        drop(expected.splice(split..end, text.encode_utf16()));
                        drop(expected.splice(start..split, "Y".encode_utf16()));
                        match String::from_utf16(&expected) {
                            Ok(after) => {
                                assert!(
                                    validate(before, &after, &changes).is_ok(),
                                    "exact ordered replacements {start}..{split}..{end}"
                                );
                                assert!(
                                    validate(before, &format!("{after}!"), &changes).is_err(),
                                    "complete after-text comparison"
                                );
                            }
                            Err(_) => assert!(
                                validate(before, before, &changes).is_err(),
                                "surrogate fragments cannot silently change encoding"
                            ),
                        }
                    }
                }
            }
        }
    }
}
