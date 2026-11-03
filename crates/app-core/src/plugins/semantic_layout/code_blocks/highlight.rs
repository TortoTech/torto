use super::*;
use std::{ops::Range, sync::LazyLock};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

// Immutable grammar/theme data is shared; per-container parser state remains
// worker-local. Oversized snippets retain plain code instead of blocking layout.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);
const MAX_BYTES: usize = 128 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024;

fn colors(
    text: &str,
    language: &str,
    theme: &Theme,
) -> Option<Vec<(Range<usize>, rebook_publication::Rgba)>> {
    let syntax = SYNTAXES.find_syntax_by_token(language)?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut result = Vec::new();
    let mut offset = 0;
    for line in LinesWithEndings::from(text) {
        for (style, token) in highlighter.highlight_line(line, &SYNTAXES).ok()? {
            let end = offset + token.len();
            let c = style.foreground;
            result.push((
                offset..end,
                rebook_publication::Rgba {
                    red: c.r,
                    green: c.g,
                    blue: c.b,
                    alpha: 255,
                },
            ));
            offset = end;
        }
    }
    (offset == text.len()).then_some(result)
}

pub(super) fn apply(body: &mut [TextBlock], language: &str) {
    let mut text = String::new();
    for block in body.iter() {
        for inline in &block.content {
            match inline {
                Inline::Text(run) => {
                    if run.text.len() > MAX_BYTES.saturating_sub(text.len()) {
                        return;
                    }
                    text.push_str(&run.text);
                }
                Inline::Break => text.push('\n'),
                _ => return,
            }
            if text.len() > MAX_BYTES {
                return;
            }
        }
        text.push('\n');
    }
    if text.lines().any(|line| line.len() > MAX_LINE_BYTES) {
        return;
    }
    let (Some(light), Some(dark)) = (
        THEMES.themes.get("InspiredGitHub"),
        THEMES.themes.get("base16-ocean.dark"),
    ) else {
        return;
    };
    let language = language.to_ascii_lowercase();
    let (Some(light), Some(dark)) = (
        colors(&text, &language, light),
        colors(&text, &language, dark),
    ) else {
        return;
    };
    let mut spans = Vec::new();
    let (mut l, mut d) = (0, 0);
    while l < light.len() && d < dark.len() {
        let start = light[l].0.start.max(dark[d].0.start);
        let end = light[l].0.end.min(dark[d].0.end);
        if start < end {
            spans.push((
                start..end,
                SyntaxColors {
                    light: light[l].1,
                    dark: dark[d].1,
                },
            ));
        }
        if light[l].0.end == end {
            l += 1;
        }
        if dark[d].0.end == end {
            d += 1;
        }
    }
    let mut offset = 0;
    let mut cursor = 0;
    for block in body {
        let mut content = Vec::new();
        for inline in &block.content {
            match inline {
                Inline::Text(run) => {
                    let end = offset + run.text.len();
                    while cursor < spans.len() && spans[cursor].0.end <= offset {
                        cursor += 1;
                    }
                    let mut i = cursor;
                    let mut start = offset;
                    while start < end {
                        let Some((range, colors)) = spans.get(i) else {
                            return;
                        };
                        let stop = end.min(range.end);
                        let mut piece: TextRun = run.clone();
                        piece.text = run.text[start - offset..stop - offset].into();
                        piece.style.syntax_colors = Some(*colors);
                        content.push(Inline::Text(piece));
                        start = stop;
                        i += 1;
                    }
                    offset = end;
                }
                Inline::Break => {
                    content.push(Inline::Break);
                    offset += 1;
                }
                _ => unreachable!("unsupported inline content rejected above"),
            }
        }
        block.content = content;
        offset += 1;
    }
}
