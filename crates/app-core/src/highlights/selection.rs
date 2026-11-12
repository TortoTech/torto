//! Source-based selection edits, shared by desktop and mobile. Notes remain
//! independent records; merging or removing plain highlights never discards them.
use super::*;
use rebook_publication::{BookSource, SourceRange};

#[derive(Clone, Copy)]
pub enum SelectionAction {
    Add,
    Toggle,
    Remove,
}

pub fn normalize(ranges: &[SourceRange]) -> Vec<SourceRange> {
    let mut result = ranges.to_vec();
    result.sort_by(|a, b| {
        (
            a.start.spine.as_str(),
            &a.start.node,
            a.start.text_offset,
            &a.end.node,
            a.end.text_offset,
        )
            .cmp(&(
                b.start.spine.as_str(),
                &b.start.node,
                b.start.text_offset,
                &b.end.node,
                b.end.text_offset,
            ))
    });
    let mut merged: Vec<SourceRange> = Vec::new();
    for range in result {
        if let Some(last) = merged.last_mut()
            && last.start.spine == range.start.spine
            && last.start.node == range.start.node
            && last.end.spine == last.start.spine
            && last.end.node == last.start.node
            && range.end.spine == range.start.spine
            && range.end.node == range.start.node
            && range.start.text_offset <= last.end.text_offset
        {
            last.end.text_offset = last.end.text_offset.max(range.end.text_offset);
        } else if merged.last() != Some(&range) {
            merged.push(range);
        }
    }
    merged
}
pub fn same_ranges(left: &[SourceRange], right: &[SourceRange]) -> bool {
    normalize(left) == normalize(right)
}
pub fn intersects(left: &[SourceRange], right: &[SourceRange]) -> bool {
    left.iter().any(|a| {
        right.iter().any(|b| {
            a == b
                || a.start.spine == b.start.spine
                    && a.end.spine == a.start.spine
                    && b.end.spine == b.start.spine
                    && a.start.node == a.end.node
                    && b.start.node == b.end.node
                    && a.start.node == b.start.node
                    && a.start.text_offset < b.end.text_offset
                    && b.start.text_offset < a.end.text_offset
        })
    })
}
pub fn contains_highlight(rows: &[StoredHighlight], ranges: &[SourceRange]) -> bool {
    rows.iter()
        .any(|a| a.note.is_none() && same_ranges(&a.ranges, ranges))
}

/// Rebuild the complete union quote in document order, parsing only touched
/// spine items. Do not concatenate overlapping quotes or compare node IDs as order.
fn resolve_quote(
    source: &dyn BookSource,
    ranges: &[SourceRange],
) -> HighlightResult<(Vec<SourceRange>, String)> {
    let mut resolved = Vec::new();
    let mut quotes = Vec::new();
    for (index, spine) in source.book().sections.iter().enumerate() {
        if !ranges.iter().any(|r| r.start.spine == spine.id) {
            continue;
        }
        let section = source.parse_section(index)?;
        for text in section.blocks.iter().flat_map(|b| b.text_blocks()) {
            let Some(owner) = &text.source else {
                continue;
            };
            if owner.start.node != owner.end.node {
                continue;
            }
            let mut parts = ranges
                .iter()
                .filter(|r| {
                    r.start.spine == owner.start.spine
                        && r.start.node == owner.start.node
                        && r.end.node == owner.end.node
                })
                .collect::<Vec<_>>();
            parts.sort_by_key(|r| r.start.text_offset);
            if parts.is_empty() {
                continue;
            }
            // Keep the same source-coordinate lengths as ReaderSession: ruby
            // annotations and generated math/image labels consume no prose chars.
            let prose = text
                .content
                .iter()
                .map(|inline| match inline {
                    rebook_publication::Inline::Text(run) => run.text.clone(),
                    rebook_publication::Inline::Ruby(run) => run.base_text(),
                    rebook_publication::Inline::Math(run) => {
                        run.original_text().unwrap_or_default()
                    }
                    rebook_publication::Inline::Image(_) => String::new(),
                    rebook_publication::Inline::Break => "\n".into(),
                })
                .collect::<String>();
            let chars = prose.chars().collect::<Vec<_>>();
            if chars.len() as u64
                != owner
                    .end
                    .text_offset
                    .saturating_sub(owner.start.text_offset)
            {
                continue;
            }
            let mut block_quote = String::new();
            for part in parts {
                let start = part.start.text_offset.max(owner.start.text_offset);
                let end = part.end.text_offset.min(owner.end.text_offset);
                if end <= start {
                    continue;
                }
                let mut range = part.clone();
                range.start.text_offset = start;
                range.end.text_offset = end;
                block_quote.extend(
                    &chars[(start - owner.start.text_offset) as usize
                        ..(end - owner.start.text_offset) as usize],
                );
                resolved.push(range);
            }
            if !block_quote.is_empty() {
                quotes.push(block_quote);
            }
        }
    }
    if !same_ranges(&resolved, ranges) {
        return Err(std::io::Error::other("Cannot resolve the complete highlight range").into());
    }
    Ok((resolved, quotes.join("\n\n")))
}

impl HighlightStore {
    /// Runs on a persistence worker. Optimistic comparison prevents a source
    /// lookup from holding SQLite's write lock or overwriting a concurrent edit.
    pub fn save_selection(
        &self,
        source: &dyn BookSource,
        requested: StoredHighlight,
        action: SelectionAction,
    ) -> HighlightResult<Vec<StoredHighlight>> {
        for _ in 0..4 {
            let rows = self.repository.highlights_for_book(&requested.book_id)?;
            let mut remove = Vec::new();
            let mut replacement = requested.clone();
            if requested.note.is_some() {
                // Editing/adding a note at the same location is not another highlight.
                if let Some(existing) = rows
                    .iter()
                    .find(|a| a.id == requested.id && a.note.is_some())
                {
                    replacement.id.clone_from(&existing.id);
                    replacement.created_at = existing.created_at;
                    replacement.ranges.clone_from(&existing.ranges);
                    replacement.quote.clone_from(&existing.quote);
                }
            } else if matches!(action, SelectionAction::Remove)
                || matches!(action, SelectionAction::Toggle)
                    && contains_highlight(&rows, &replacement.ranges)
            {
                remove = rows
                    .iter()
                    .filter(|a| a.note.is_none() && same_ranges(&a.ranges, &replacement.ranges))
                    .map(|a| a.id.clone())
                    .collect();
                if self
                    .repository
                    .apply_selection_edit(&requested.book_id, &rows, &remove, None)?
                {
                    return self.repository.highlights_for_book(&requested.book_id);
                }
                continue;
            } else {
                // A new range may bridge two old ones. Close the whole overlap
                // component; merely adjacent, nonoverlapping highlights stay separate.
                loop {
                    let mut changed = false;
                    for row in &rows {
                        if row.note.is_some()
                            || remove.contains(&row.id)
                            || !intersects(&replacement.ranges, &row.ranges)
                        {
                            continue;
                        }
                        remove.push(row.id.clone());
                        replacement.ranges.extend(row.ranges.clone());
                        replacement.ranges = normalize(&replacement.ranges);
                        changed = true;
                    }
                    if !changed {
                        break;
                    }
                }
                if let Some(oldest) = rows
                    .iter()
                    .filter(|r| remove.contains(&r.id))
                    .min_by_key(|r| (r.created_at, &r.id))
                {
                    replacement.id.clone_from(&oldest.id);
                    replacement.created_at = oldest.created_at;
                    if same_ranges(&oldest.ranges, &replacement.ranges) {
                        replacement.ranges.clone_from(&oldest.ranges);
                        replacement.quote.clone_from(&oldest.quote);
                    } else {
                        (replacement.ranges, replacement.quote) =
                            resolve_quote(source, &replacement.ranges)?;
                    }
                }
            }
            if self.repository.apply_selection_edit(
                &requested.book_id,
                &rows,
                &remove,
                Some(&replacement),
            )? {
                return self.repository.highlights_for_book(&requested.book_id);
            }
        }
        Err(std::io::Error::other("Highlights changed concurrently; please retry").into())
    }
}

#[cfg(test)]
mod tests;
