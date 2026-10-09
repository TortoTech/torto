//! Recover PDF heading groups using outline text and local geometry. Source
//! glyphs keep their physical coordinates even when a margin ordinal moves.
use super::*;

fn key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn title_end(lines: &[Line], start: usize, expected: &str) -> Option<usize> {
    let expected = key(expected);
    let mut text = String::new();
    for end in start..(start + 4).min(lines.len()) {
        let line = &lines[end];
        if end > start {
            let previous = &lines[end - 1];
            let size = line.size.max(previous.size);
            let aligned = (line.rect.x0 - previous.rect.x0).abs() <= size
                || (line.rect.center().x - previous.rect.center().x).abs() <= size;
            if !aligned
                || line.baseline - previous.baseline > size * 3.0
                || (line.size - lines[start].size).abs() > lines[start].size * 0.25
            {
                break;
            }
        }
        text.push_str(&key(&line.text()));
        if text == expected {
            return Some(end + 1);
        }
        if !expected.starts_with(&text) {
            break;
        }
    }
    None
}

fn indices(lines: &[Line]) -> Vec<usize> {
    lines
        .iter()
        .flat_map(|l| l.glyphs.iter().map(|g| g.index))
        .collect()
}

fn margin_ordinal(line: &Line, title: &Line, expected: &str) -> Option<Vec<usize>> {
    if line.baseline <= title.baseline
        || line.baseline - title.baseline > title.size * 3.0
        || (line.rect.x0 - title.rect.x0).abs() > title.size.max(line.glyphs[0].size) * 3.0
    {
        return None;
    }
    let end = line
        .glyphs
        .iter()
        .position(|g| g.text.chars().any(char::is_alphabetic))?;
    let prefix = &line.glyphs[..end];
    let text: String = prefix.iter().map(|g| g.text.as_str()).collect();
    if heading_ordinal_key(&text).as_deref() != Some(expected)
        || prefix
            .iter()
            .filter(|g| !g.text.trim().is_empty())
            .all(|g| g.size <= line.size * 1.2)
        || !text.ends_with(char::is_whitespace)
    {
        return None;
    }
    Some(prefix.iter().map(|g| g.index).collect())
}

pub(super) fn mark(page: &mut NativePage, labels: &[(String, u8)]) {
    let grouped = lines(page.glyphs.clone());
    let mut claimed = HashSet::new();
    for (label, level) in labels {
        let split = crate::source::split_heading_label(label);
        let mut candidates = Vec::new();
        for start in 0..grouped.len() {
            if let Some(end) = title_end(&grouped, start, label) {
                candidates.push(NativeHeading {
                    ordinal: Vec::new(),
                    title: indices(&grouped[start..end]),
                    level: *level,
                });
            }
            let Some((ordinal, title)) = &split else {
                continue;
            };
            if heading_ordinal_key(&grouped[start].text()).as_ref() == Some(ordinal)
                && let Some(end) = title_end(&grouped, start + 1, title)
                && end > start + 1
                && grouped[start + 1].baseline - grouped[start].baseline
                    <= grouped[start].size.max(grouped[start + 1].size) * 3.0
                && (grouped[start].rect.x0 - grouped[start + 1].rect.x0).abs()
                    <= grouped[start].size.max(grouped[start + 1].size) * 2.0
            {
                candidates.push(NativeHeading {
                    ordinal: indices(&grouped[start..start + 1]),
                    title: indices(&grouped[start + 1..end]),
                    level: *level,
                });
            }
            if let Some(end) = title_end(&grouped, start, title) {
                let ordinal = grouped
                    .get(end)
                    .and_then(|body| margin_ordinal(body, &grouped[end - 1], ordinal))
                    .unwrap_or_default();
                candidates.push(NativeHeading {
                    ordinal,
                    title: indices(&grouped[start..end]),
                    level: *level,
                });
            }
        }
        // Prefer a verified pair to its title-only candidate. Ambiguous repeated
        // labels are left untouched rather than selecting the first occurrence.
        if candidates.iter().any(|c| !c.ordinal.is_empty()) {
            candidates.retain(|c| !c.ordinal.is_empty());
        }
        candidates.retain(|c| {
            c.title
                .iter()
                .chain(&c.ordinal)
                .all(|g| !claimed.contains(g))
        });
        let [candidate] = candidates.as_slice() else {
            continue;
        };
        for index in &candidate.title {
            page.glyphs[*index].tag = Some(format!("H{level}"));
        }
        claimed.extend(candidate.title.iter().chain(&candidate.ordinal).copied());
        page.headings.push(candidate.clone());
    }
}

pub(super) fn recover(
    builder: &mut Builder,
    prose: &mut Vec<Line>,
    groups: &[NativeHeading],
) -> Vec<(Rect, Item)> {
    let mut result = Vec::new();
    for group in groups {
        let available = prose
            .iter()
            .flat_map(|l| l.glyphs.iter().map(|g| g.index))
            .collect::<HashSet<_>>();
        if !group
            .title
            .iter()
            .chain(&group.ordinal)
            .all(|g| available.contains(g))
        {
            continue;
        }
        let title = group.title.iter().copied().collect::<HashSet<_>>();
        let ordinal = group.ordinal.iter().copied().collect::<HashSet<_>>();
        let take = |selected: &HashSet<usize>| {
            lines(
                prose
                    .iter()
                    .flat_map(|l| l.glyphs.iter())
                    .filter(|g| selected.contains(&g.index))
                    .cloned(),
            )
        };
        let title_lines = take(&title);
        let ordinal_lines = take(&ordinal);
        let rect = title_lines
            .iter()
            .map(|l| l.rect)
            .reduce(|a, b| a.union(b))
            .unwrap();
        let mut blocks = Vec::new();
        if !ordinal_lines.is_empty() {
            blocks.push(Block::Text(
                builder.text(&ordinal_lines, TextBlockKind::HeadingOrdinal(group.level)),
            ));
        }
        blocks.push(Block::Text(
            builder.text(&title_lines, TextBlockKind::Heading(group.level)),
        ));
        for line in prose.iter_mut() {
            line.glyphs
                .retain(|g| !title.contains(&g.index) && !ordinal.contains(&g.index));
            if !line.glyphs.is_empty() {
                *line = make_line(std::mem::take(&mut line.glyphs));
            }
        }
        prose.retain(|l| !l.glyphs.is_empty());
        result.push((rect, Item::Heading(blocks)));
    }
    result
}
