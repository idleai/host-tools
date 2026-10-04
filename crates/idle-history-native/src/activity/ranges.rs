use std::ops::Range;

use editchain_engine::{
    OpId,
    activity::{Field, File, TextRange},
    queries::{ChainQueries, ContentField},
};

use crate::history::{Failure, FailureCode};

use super::records::{self, MAX_ITEMS, MAX_TEXT};

/// Map only exact recorded replacements after verifying their resulting snapshot.
pub(super) fn edits(
    queries: &ChainQueries,
    id: OpId,
    file: &File,
    after: &str,
) -> Result<Vec<TextRange>, Failure> {
    if file.text_edits.len() > MAX_ITEMS {
        return Err(invalid("The edit range query limit was reached."));
    }
    if file.text_edits.is_empty() {
        return Ok(Vec::new());
    }
    let mut current = records::text(queries, id, ContentField::FileBase)?;
    let mut spans: Vec<Range<usize>> = Vec::new();
    for (index, edit) in file.text_edits.iter().enumerate() {
        let start = utf16_offset(&current, edit.offset_utf16)
            .ok_or_else(|| invalid("An edit starts outside the recorded buffer."))?;
        let end = edit
            .offset_utf16
            .checked_add(edit.length_utf16)
            .and_then(|end| utf16_offset(&current, end))
            .ok_or_else(|| invalid("An edit ends outside the recorded buffer."))?;
        let inserted = records::text(queries, id, ContentField::Record(Field::TextEdit(index)))?;
        let length = current
            .len()
            .saturating_sub(end.saturating_sub(start))
            .saturating_add(inserted.len());
        if length > MAX_TEXT {
            return Err(invalid(
                "The intermediate edit exceeds the decoration size limit.",
            ));
        }
        spans = move_spans(spans, start..end, inserted.len());
        current.replace_range(start..end, &inserted);
    }
    if current != after {
        return Err(invalid(
            "The recorded edits do not produce the selected snapshot.",
        ));
    }
    let map = TextMap::new(after);
    spans
        .into_iter()
        .map(|span| {
            map.range(span)
                .ok_or_else(|| invalid("An edit range cannot be mapped to editor coordinates."))
        })
        .collect()
}

fn move_spans(spans: Vec<Range<usize>>, edit: Range<usize>, inserted: usize) -> Vec<Range<usize>> {
    let mut next = Vec::new();
    let inserted_end = edit.start.saturating_add(inserted);
    for span in spans {
        if span.end <= edit.start {
            next.push(span);
        } else if span.start >= edit.end {
            next.push(
                span.start
                    .saturating_sub(edit.end)
                    .saturating_add(inserted_end)
                    ..span
                        .end
                        .saturating_sub(edit.end)
                        .saturating_add(inserted_end),
            );
        } else {
            if span.start < edit.start {
                next.push(span.start..edit.start);
            }
            if span.end > edit.end {
                next.push(
                    inserted_end
                        ..span
                            .end
                            .saturating_sub(edit.end)
                            .saturating_add(inserted_end),
                );
            }
        }
    }
    if inserted > 0 {
        next.push(edit.start..inserted_end);
    }
    next.sort_by_key(|span| span.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for span in next {
        if let Some(previous) = merged
            .last_mut()
            .filter(|previous| previous.end >= span.start)
        {
            previous.end = previous.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}

pub(super) fn exposure(file: &File, map: &TextMap<'_>) -> Result<Vec<TextRange>, Failure> {
    if file.text_ranges.len().max(file.ranges.len()) > MAX_ITEMS {
        return Err(invalid("The exposure range query limit was reached."));
    }
    let mut ranges = Vec::new();
    for span in &file.ranges {
        let bytes = usize::try_from(span.start)
            .ok()
            .zip(usize::try_from(span.end).ok())
            .filter(|(start, end)| start <= end)
            .ok_or_else(|| invalid("An exposure byte range is invalid."))?;
        ranges.push(
            map.range(bytes.0..bytes.1).ok_or_else(|| {
                invalid("An exposure byte range cannot be mapped to this snapshot.")
            })?,
        );
    }
    if !file.text_ranges.is_empty() {
        for range in &file.text_ranges {
            if range.start > range.end
                || map.offset(range.start).is_none()
                || map.offset(range.end).is_none()
            {
                return Err(invalid(
                    "An exposure UTF-16 range cannot be mapped to this snapshot.",
                ));
            }
        }
        if !ranges.is_empty() && ranges != file.text_ranges {
            return Err(invalid("The exposure byte and UTF-16 ranges disagree."));
        }
        ranges.clone_from(&file.text_ranges);
    }
    Ok(ranges)
}

fn utf16_offset(text: &str, offset: u64) -> Option<usize> {
    let mut units = 0_u64;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units = units.checked_add(u64::try_from(character.len_utf16()).ok()?)?;
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

pub(super) struct TextMap<'a> {
    text: &'a str,
    lines: Vec<Range<usize>>,
}

impl<'a> TextMap<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0;
        let mut characters = text.char_indices().peekable();
        while let Some((offset, character)) = characters.next() {
            if character == '\r' || character == '\n' {
                lines.push(start..offset);
                start = offset.saturating_add(1);
                if character == '\r' && characters.peek().is_some_and(|(_, next)| *next == '\n') {
                    let _newline = characters.next();
                    start = start.saturating_add(1);
                }
            }
        }
        lines.push(start..text.len());
        Self { text, lines }
    }

    fn offset(&self, [line, column]: [u32; 2]) -> Option<usize> {
        let range = self.lines.get(usize::try_from(line).ok()?)?;
        let offset = utf16_offset(self.text.get(range.clone())?, u64::from(column))?;
        range.start.checked_add(offset)
    }

    fn position(&self, offset: usize) -> Option<[u32; 2]> {
        let line = self
            .lines
            .partition_point(|line| line.start <= offset)
            .saturating_sub(1);
        let range = self.lines.get(line)?;
        if offset > range.end {
            return None;
        }
        let column = self.text.get(range.start..offset)?.encode_utf16().count();
        Some([u32::try_from(line).ok()?, u32::try_from(column).ok()?])
    }

    fn range(&self, range: Range<usize>) -> Option<TextRange> {
        Some(TextRange {
            start: self.position(range.start)?,
            end: self.position(range.end)?,
        })
    }
}

fn invalid(message: &str) -> Failure {
    Failure::new(FailureCode::Unavailable, message)
}
