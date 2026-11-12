//! Unified list markers without changing authored source offsets or inline slots.
use std::borrow::Cow;

use rebook_publication::{Inline, TextBlock, TextBlockKind, TextRun};

pub(super) fn hanging_body(block: &TextBlock) -> bool {
    matches!(block.kind, TextBlockKind::ListItem { .. })
        && block.style.direction != rebook_publication::TextDirection::Rtl
}

pub(super) fn prefix(block: &TextBlock) -> Option<String> {
    formatted_prefix(block, false)
}

pub(super) fn visible_prefix(block: &TextBlock) -> String {
    formatted_prefix(block, true).unwrap_or_else(|| {
        let mut kind = block.kind;
        if let TextBlockKind::ListItem { marker_visible, .. } = &mut kind {
            *marker_visible = true;
        }
        super::list_marker_prefix(kind)
    })
}

fn formatted_prefix(block: &TextBlock, force_visible: bool) -> Option<String> {
    let TextBlockKind::ListItem {
        ordered: true,
        ordinal,
        marker_visible,
        ..
    } = block.kind
    else {
        return None;
    };
    let format = block.style.list_marker_format?;
    if ordinal == 0 || !marker_visible && !force_visible {
        return None;
    }
    let mut number = ordinal;
    let mut chars = Vec::new();
    let base = match format {
        rebook_publication::ListMarkerFormat::LowerAlpha => b'a',
        rebook_publication::ListMarkerFormat::UpperAlpha => b'A',
    };
    while number > 0 {
        number -= 1;
        chars.push(char::from(base + (number % 26) as u8));
        number /= 26;
    }
    chars.reverse();
    Some(format!(
        "{}.\u{00a0}",
        chars.into_iter().collect::<String>()
    ))
}

pub(super) fn normalize(block: &TextBlock, unified: bool) -> Cow<'_, TextBlock> {
    let TextBlockKind::ListItem { .. } = block.kind else {
        return Cow::Borrowed(block);
    };
    let Some(count) = block.style.authored_list_marker.filter(|_| unified) else {
        return Cow::Borrowed(block);
    };
    let mut result = block.clone();
    // Images have no source text scalars. Replace only the leading marker image,
    // keeping its inline index so supplied source-span boundaries remain valid.
    let image_marker = result
        .content
        .iter()
        .position(|inline| match inline {
            Inline::Text(run) => !run.text.trim().is_empty(),
            _ => true,
        })
        .filter(|&index| matches!(result.content[index], Inline::Image(_)));
    if let Some(index) = image_marker {
        result.content[index] = Inline::Text(TextRun {
            text: String::new(),
            style: Default::default(),
            link: None,
        });
    }
    // The generated marker sits outside the source text. Keep one zero-width
    // scalar per authored prefix scalar, as with collapsed footnote markers.
    let mut remaining = count as usize;
    for inline in &mut result.content {
        if remaining == 0 {
            break;
        }
        let Inline::Text(run) = inline else {
            break;
        };
        let consumed = run.text.chars().count().min(remaining);
        let end = run
            .text
            .char_indices()
            .nth(consumed)
            .map_or(run.text.len(), |(i, _)| i);
        run.text.replace_range(..end, &"\u{2060}".repeat(consumed));
        remaining -= consumed;
    }
    result.style.authored_list_marker = None;
    if let TextBlockKind::ListItem { marker_visible, .. } = &mut result.kind {
        *marker_visible = true;
    }
    Cow::Owned(result)
}

pub(super) fn body_indent_em(block: &TextBlock, style: &super::ReaderStyle) -> f32 {
    let TextBlockKind::ListItem { depth, .. } = block.kind else { return 0.0; };
    let display = block.content.iter().flat_map(Inline::text_runs)
        .find_map(|run| run.style.display_writing_system).unwrap_or(style.writing_system);
    super::paragraph_indent_em(&style.typesetting, display)
        + f32::from(depth) * style.typesetting.list_indent_em
}
