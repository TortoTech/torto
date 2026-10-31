use super::*;
use std::ops::Range;

pub const BATCH_CHARS: usize = 5_000;

fn chars(block: &Block) -> usize {
    let text = |t: &TextBlock| text_block_text(t).chars().count();
    match block {
        Block::Text(t) => text(t),
        Block::Quote(q) => q.body.iter().chain(q.attribution.iter()).map(text).sum(),
        Block::Table(t) => t.text_blocks().map(text).sum(),
        Block::Figure(f) => f.text_blocks().map(text).sum(),
        Block::Note(n) => n.blocks.iter().map(chars).sum(),
        Block::Image(i) => i.alt.chars().count(),
        _ => 0,
    }
}

// Authored composite blocks are already indivisible. Keep adjacent media/caption
// runs together too, even when the parser represented them as separate blocks.
pub fn semantic_units(section: &Section, range: Range<usize>) -> Vec<Range<usize>> {
    let media = |b: &Block| matches!(b, Block::Image(_) | Block::Table(_) | Block::Figure(_));
    let caption = |b: &Block| matches!(b, Block::Text(t) if matches!(t.kind,TextBlockKind::Caption | TextBlockKind::QuoteAttribution));
    let refs: Vec<_> = section.blocks.iter().collect();
    let mut group_ends = vec![0; section.blocks.len()];
    for group in rebook_layout::semantic_paragraph_groups(&refs) {
        if group.start >= range.start && group.end <= range.end {
            group_ends[group.start] = group.end;
        }
    }
    // This only packs candidates into one request; the model still determines
    // whether they form one example or several independent groups.
    for run in code_blocks::runs(section, range.clone()) {
        group_ends[run.start] = group_ends[run.start].max(run.end);
    }
    let mut units = Vec::new();
    let mut start = range.start;
    while start < range.end {
        let mut end = group_ends[start].max(start + 1);
        while end < range.end {
            let a = &section.blocks[end - 1];
            let b = &section.blocks[end];
            if (code_blocks::body(a).is_some() && caption(b))
                || (caption(a) && code_blocks::body(b).is_some())
                || (media(a) && (media(b) || caption(b)))
                || (caption(a) && (media(b) || caption(b)))
                || (matches!(a, Block::Quote(_))
                    && matches!(b, Block::Text(t) if t.kind == TextBlockKind::QuoteAttribution))
            {
                end = group_ends[end].max(end + 1);
            } else {
                break;
            }
        }
        units.push(start..end);
        start = end;
    }
    units
}

pub fn fixed_batches(section: &Section, range: Range<usize>) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = range.start;
    let mut end = start;
    let mut size = 0;
    for unit in semantic_units(section, range) {
        let count: usize = section.blocks[unit.clone()].iter().map(chars).sum();
        if end > start && size + count > BATCH_CHARS {
            batches.push(start..end);
            start = unit.start;
            size = 0;
        }
        end = unit.end;
        size += count;
    }
    if end > start {
        batches.push(start..end);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::semantic_layout::tests::{image, section, text};

    #[test]
    fn fixed_character_budget_has_no_block_limit_and_keeps_oversized_blocks() {
        let s = section(
            (0..100)
                .map(|i| text(&i.to_string(), &"a".repeat(50)))
                .collect(),
        );
        assert_eq!(fixed_batches(&s, 0..100), vec![0..100]);
        let s = section(vec![
            text("a", &"x".repeat(2500)),
            text("b", &"x".repeat(2500)),
            text("c", &"x".repeat(5001)),
            text("d", "end"),
        ]);
        assert_eq!(fixed_batches(&s, 0..4), vec![0..2, 2..3, 3..4]);
    }

    #[test]
    fn media_and_caption_remain_atomic() {
        let mut caption = text("caption", &"c".repeat(200));
        let Block::Text(t) = &mut caption else {
            unreachable!()
        };
        t.kind = TextBlockKind::Caption;
        let s = section(vec![text("a", &"a".repeat(4900)), image("image"), caption]);
        assert_eq!(fixed_batches(&s, 0..3), vec![0..1, 1..3]);
        let mut s = s;
        s.blocks.swap(1, 2);
        assert_eq!(fixed_batches(&s, 0..3), vec![0..1, 1..3]);
    }

    #[test]
    fn authored_paragraph_groups_are_atomic_even_above_the_character_budget() {
        let mut s = section(vec![
            text("before", &"a".repeat(4900)),
            text("one", &"b".repeat(3000)),
            text("two", &"c".repeat(3000)),
            text("after", "End"),
        ]);
        for b in &mut s.blocks[1..3] {
            if let Block::Text(t) = b {
                t.style.paragraph_group = Some(7);
            }
        }
        assert_eq!(fixed_batches(&s, 0..4), vec![0..1, 1..3, 3..4]);
        assert_eq!(semantic_units(&s, 0..4), vec![0..1, 1..3, 3..4]);
    }
}
