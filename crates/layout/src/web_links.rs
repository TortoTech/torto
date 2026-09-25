use super::*;

// Keep original characters and source offsets. Only the display pass folds them.
pub(super) fn detect(content: &mut Vec<Inline>) {
    let mut out = Vec::new();
    for inline in std::mem::take(content) {
        let Inline::Text(run) = inline else {
            out.push(inline);
            continue;
        };
        if run.link.is_some()
            || run.style.inline_citation != 0
            || run.style.inline_role != InlineRole::Normal
            || run.style.link_role != LinkRole::Normal
        {
            out.push(Inline::Text(run));
            continue;
        }
        let mut at = 0;
        for (start, ch) in run.text.char_indices() {
            if start < at || !ch.is_ascii_alphanumeric() {
                continue;
            }
            if run.text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-'))
            {
                continue;
            }
            let mut end = run.text[start..]
                .char_indices()
                .find(|(_, c)| {
                    !c.is_ascii()
                        || c.is_whitespace()
                        || matches!(c, '<' | '>' | '"' | ',' | ';' | '[' | ']')
                })
                .map_or(run.text.len(), |(i, _)| start + i);
            while end > start && run.text[..end].ends_with(['.', ',', ';', ':', '!', '?']) {
                end -= 1;
            }
            while end > start
                && run.text[..end].ends_with(')')
                && run.text[start..end].matches(')').count()
                    > run.text[start..end].matches('(').count()
            {
                end -= 1;
            }
            let Some(link) = PublicationUrl::website(&run.text[start..end]) else {
                continue;
            };
            if at < start {
                let mut prefix = run.clone();
                prefix.text = run.text[at..start].into();
                out.push(Inline::Text(prefix));
            }
            let mut linked = run.clone();
            linked.text = run.text[start..end].into();
            linked.link = Some(link);
            out.push(Inline::Text(linked));
            at = end;
        }
        if at < run.text.len() {
            let mut tail = run.clone();
            tail.text = run.text[at..].into();
            out.push(Inline::Text(tail));
        }
    }
    *content = out;
}
