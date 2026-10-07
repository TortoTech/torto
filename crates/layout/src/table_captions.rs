//! Unified presentation of title rows; authored IR and translation IDs stay intact.
use rebook_publication::{Inline, LinkRole, TableBlock, TextBlock, TextBlockKind, TextRun};
use std::borrow::Cow;

pub(super) struct Presentation<'a> {
    pub skip_title: bool,
    pub before: Cow<'a, [TextBlock]>,
    pub after: Cow<'a, [TextBlock]>,
    /// Number-only caption and promoted title to shape as one paragraph.
    pub join: Option<(bool, usize)>,
}

#[derive(PartialEq)]
enum CaptionState {
    Empty,
    Label,
    Complete,
}

/// Soften only edge line breaks at the join. Keep every source character and
/// internal bilingual line break; the authored IR is never changed.
pub(super) fn join_content(label: &[Inline], title: &[Inline]) -> Vec<Inline> {
    fn soften(content: &[Inline]) -> Vec<Inline> {
        let visible = |inline: &Inline| match inline {
            Inline::Break => false,
            Inline::Image(_) | Inline::Math(_) => true,
            Inline::Text(_) | Inline::Ruby(_) => inline
                .text_runs()
                .iter()
                .any(|run| !run.text.trim().is_empty()),
        };
        let first = content.iter().position(visible).unwrap_or(content.len());
        let last = content.iter().rposition(visible);
        content
            .iter()
            .enumerate()
            .map(|(index, inline)| {
                let mut inline = inline.clone();
                match &mut inline {
                    Inline::Break if index < first || last.is_none_or(|last| index > last) => {
                        inline = Inline::Text(TextRun {
                            text: " ".into(),
                            style: Default::default(),
                            link: None,
                        });
                    }
                    Inline::Text(run) => {
                        let leading = if index <= first {
                            run.text.len() - run.text.trim_start().len()
                        } else {
                            0
                        };
                        let trailing = if last.is_none_or(|last| index >= last) {
                            run.text.trim_end().len()
                        } else {
                            run.text.len()
                        };
                        run.text = run
                            .text
                            .char_indices()
                            .map(|(at, ch)| {
                                if (at < leading || at >= trailing) && matches!(ch, '\n' | '\r') {
                                    ' '
                                } else {
                                    ch
                                }
                            })
                            .collect();
                    }
                    _ => {}
                }
                inline
            })
            .collect()
    }
    let mut result = soften(label);
    result.push(Inline::Text(TextRun {
        text: " ".into(),
        style: Default::default(),
        link: None,
    }));
    result.extend(soften(title));
    result
}

fn caption_state(block: &TextBlock) -> CaptionState {
    let mut text = String::new();
    for inline in &block.content {
        match inline {
            Inline::Text(_) | Inline::Ruby(_) => {
                for run in inline.text_runs() {
                    if run.style.link_role != LinkRole::FootnoteReference {
                        text.push_str(&run.text);
                    }
                }
            }
            Inline::Break => text.push('\n'),
            Inline::Math(_) | Inline::Image(_) => return CaptionState::Complete,
        }
    }
    if text.trim().is_empty() {
        return CaptionState::Empty;
    }
    // Bilingual table labels occupy separate lines within one text block.
    if super::caption_labels::table_label_only(&text)
        || text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(super::caption_labels::table_label_only)
    {
        CaptionState::Label
    } else {
        CaptionState::Complete
    }
}

pub(super) fn presentation(table: &TableBlock, unified: bool) -> Presentation<'_> {
    let mut result = Presentation {
        skip_title: false,
        before: Cow::Borrowed(&table.before),
        after: Cow::Borrowed(&table.after),
        join: None,
    };
    if !unified {
        return result;
    }
    let Some(title) = table
        .rows
        .first()
        .filter(|row| row.cells.len() == 1)
        .map(|row| &row.cells[0])
        .filter(|cell| {
            cell.text.kind == TextBlockKind::Caption
                && cell.row_span == 1
                && cell.column_span > 1
                && table.rows.len() > 1
        })
    else {
        return result;
    };
    let mut label = None;
    for (after, captions) in [(false, &table.before), (true, &table.after)] {
        for (index, caption) in captions
            .iter()
            .enumerate()
            .filter(|(_, block)| block.kind == TextBlockKind::Caption)
        {
            match caption_state(caption) {
                CaptionState::Complete => return result,
                CaptionState::Label if label.is_some() => return result,
                CaptionState::Label => label = Some((after, index)),
                CaptionState::Empty => {}
            }
        }
    }
    let (after, index) = label.map_or((false, table.before.len()), |(after, index)| {
        (after, index + 1)
    });
    let captions = if after {
        &mut result.after
    } else {
        &mut result.before
    };
    // Retain independent source ranges instead of inventing one contiguous
    // range across the label and grid cell. Clone captions only, never the grid.
    captions.to_mut().insert(index, title.text.clone());
    // Keep authored bilingual label paragraphs intact. Joining their last
    // language to the first title language would reorder the translation.
    result.join = label.filter(|(after, index)| {
        let label = if *after {
            &table.after[*index]
        } else {
            &table.before[*index]
        };
        let mut text = String::new();
        for inline in &label.content {
            match inline {
                Inline::Break => text.push('\n'),
                _ => {
                    for run in inline.text_runs() {
                        text.push_str(&run.text);
                    }
                }
            }
        }
        text.lines().filter(|line| !line.trim().is_empty()).count() <= 1
    });
    result.skip_title = true;
    result
}

#[cfg(test)]
mod tests;
