use percent_encoding::percent_decode_str;

use crate::plugins::{CHAT_CITATION_PREFIX, chat_citation_marker_from_link};

const MAX_REFERENCE_SUGGESTIONS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChatReferenceKind {
    Book,
    Section,
    Paragraph,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatReference {
    pub(super) id: String,
    pub(super) kind: ChatReferenceKind,
    pub(super) label: String,
    pub(super) description: String,
    pub(super) link: String,
    pub(super) excerpt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatReferenceToken {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChatCitation {
    pub(super) section_index: usize,
    pub(super) node: Option<String>,
}

pub(super) fn parse_chat_citation(locator: &str) -> Option<ChatCitation> {
    let remainder = locator.strip_prefix(CHAT_CITATION_PREFIX)?;
    let (section, node) = remainder
        .split_once('/')
        .map_or((remainder, None), |(section, node)| (section, Some(node)));
    let section_index = section.parse().ok()?;
    let node = node
        .filter(|node| !node.is_empty())
        .and_then(|node| percent_decode_str(node).decode_utf8().ok())
        .map(std::borrow::Cow::into_owned);
    Some(ChatCitation {
        section_index,
        node,
    })
}

pub(super) fn chat_reference_token(
    input: &str,
    cursor_char_index: usize,
    selected: &[ChatReference],
) -> Option<ChatReferenceToken> {
    let end = char_to_byte_index(input, cursor_char_index);
    let before_cursor = &input[..end];
    let start = before_cursor.rfind('@')?;
    if input[..start]
        .chars()
        .next_back()
        .is_some_and(is_reference_word_character)
    {
        return None;
    }
    let query = &before_cursor[start + '@'.len_utf8()..];
    if query.contains('\n') || query.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let normalized_query = normalize_reference_search_text(query);
    if selected.iter().any(|reference| {
        let label = normalize_reference_search_text(&reference.label);
        normalized_query == label || normalized_query.starts_with(&format!("{label} "))
    }) {
        return None;
    }
    Some(ChatReferenceToken {
        start,
        end,
        query: query.to_owned(),
    })
}

pub(super) fn chat_reference_suggestions(
    options: &[ChatReference],
    selected: &[ChatReference],
    query: &str,
) -> Vec<ChatReference> {
    let normalized_query = normalize_reference_search_text(query);
    if normalized_query.is_empty() {
        return options
            .iter()
            .filter(|reference| !selected.iter().any(|item| item.id == reference.id))
            .take(MAX_REFERENCE_SUGGESTIONS)
            .cloned()
            .collect();
    }
    let mut scored = options
        .iter()
        .enumerate()
        .filter(|(_, reference)| !selected.iter().any(|item| item.id == reference.id))
        .filter_map(|(index, reference)| {
            let label = normalize_reference_search_text(&reference.label);
            let description = normalize_reference_search_text(&reference.description);
            let excerpt =
                normalize_reference_search_text(reference.excerpt.as_deref().unwrap_or(""));
            let searchable = format!("{label} {description} {excerpt}");
            let label_index = label.find(&normalized_query).unwrap_or(80);
            let search_index = searchable.find(&normalized_query)?;
            let kind_score = match reference.kind {
                ChatReferenceKind::Paragraph => 0,
                ChatReferenceKind::Section => 12,
                ChatReferenceKind::Book => 24,
            };
            Some((
                kind_score + label_index.min(80) + search_index.min(80),
                index,
                reference.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(score, index, _)| (*score, *index));
    scored
        .into_iter()
        .take(MAX_REFERENCE_SUGGESTIONS)
        .map(|(_, _, reference)| reference)
        .collect()
}

pub(super) fn insert_chat_reference(
    input: &str,
    token: &ChatReferenceToken,
    reference: &ChatReference,
) -> (String, usize) {
    let insert_text = format!("@{} ", reference.label);
    let mut next = String::with_capacity(input.len() + insert_text.len());
    next.push_str(&input[..token.start]);
    next.push_str(&insert_text);
    next.push_str(&input[token.end..]);
    let cursor_char_index = input[..token.start].chars().count() + insert_text.chars().count();
    (next, cursor_char_index)
}

pub(super) fn move_suggestion_index(current: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if forward {
        (current + 1) % count
    } else {
        (current + count - 1) % count
    }
}

pub(super) fn build_chat_prompt_with_references(
    content: &str,
    references: &[ChatReference],
    english: bool,
) -> String {
    if references.is_empty() {
        return content.to_owned();
    }
    let base = if content.trim().is_empty() {
        if english {
            "Answer using the referenced content."
        } else {
            "请结合我引用的内容回答。"
        }
    } else {
        content.trim()
    };
    let reference_text = references
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            let kind = match (english, reference.kind) {
                (true, ChatReferenceKind::Book) => "Full text",
                (true, ChatReferenceKind::Section) => "Chapter",
                (true, ChatReferenceKind::Paragraph) => "Paragraph",
                (false, ChatReferenceKind::Book) => "全文",
                (false, ChatReferenceKind::Section) => "章节",
                (false, ChatReferenceKind::Paragraph) => "段落",
            };
            let mut lines = vec![
                format!("{}. {kind}: {}", index + 1, reference.label),
                format!(
                    "{}: {}",
                    if english { "Location" } else { "位置" },
                    reference.description
                ),
            ];
            if reference.kind != ChatReferenceKind::Book
                && let Some(citation) = chat_citation_marker_from_link(&reference.link)
            {
                lines.push(format!("citation: {citation}"));
            }
            if let Some(excerpt) = &reference.excerpt {
                lines.push(format!(
                    "{}: {excerpt}",
                    if english { "Excerpt" } else { "摘录" }
                ));
            }
            if reference.kind == ChatReferenceKind::Book {
                lines.push(if english {
                    "Use searchBook and getContent to read the book.".into()
                } else {
                    "使用 searchBook 和 getContent 读取本书。".into()
                });
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "{base}\n\n{}\n\n{reference_text}",
        if english {
            "Use the following user-selected book content as the primary context. Use its supplied citations:"
        } else {
            "优先使用以下用户选择的书籍内容作为上下文，引用时使用其提供的 citation："
        }
    )
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

fn normalize_reference_search_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_reference_word_character(value: char) -> bool {
    value.is_alphanumeric() || matches!(value, '_' | '.' | '@' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(
        id: &str,
        kind: ChatReferenceKind,
        label: &str,
        description: &str,
    ) -> ChatReference {
        ChatReference {
            id: id.into(),
            kind,
            label: label.into(),
            description: description.into(),
            link: id.into(),
            excerpt: None,
        }
    }

    #[test]
    fn reference_token_tracks_a_unicode_cursor_and_replaces_only_the_active_token() {
        let input = "请解释 @段";
        let token = chat_reference_token(input, input.chars().count(), &[]).unwrap();
        assert_eq!(token.query, "段");
        let paragraph = reference("p1", ChatReferenceKind::Paragraph, "段落一", "当前页");
        let (next, cursor) = insert_chat_reference(input, &token, &paragraph);
        assert_eq!(next, "请解释 @段落一 ");
        assert_eq!(cursor, next.chars().count());
    }

    #[test]
    fn reference_suggestions_match_labels_descriptions_and_excerpts() {
        let mut paragraph = reference("p1", ChatReferenceKind::Paragraph, "反馈循环", "当前段落");
        paragraph.excerpt = Some("Systems thinking".into());
        let options = vec![
            paragraph,
            reference("book", ChatReferenceKind::Book, "全文", "整本书"),
        ];
        assert_eq!(
            chat_reference_suggestions(&options, &[], "段落")[0].id,
            "p1"
        );
        assert_eq!(
            chat_reference_suggestions(&options, &[], "SYSTEMS")[0].id,
            "p1"
        );
        assert_eq!(
            chat_reference_suggestions(&options, &[], "全文")[0].id,
            "book"
        );
    }

    #[test]
    fn bare_reference_token_keeps_full_text_and_chapter_ahead_of_paragraphs() {
        let mut options = vec![
            reference("book", ChatReferenceKind::Book, "全文", "整本书"),
            reference("section", ChatReferenceKind::Section, "第七章", "当前章节"),
        ];
        options.extend((0..10).map(|index| {
            reference(
                &format!("paragraph-{index}"),
                ChatReferenceKind::Paragraph,
                &format!("段落 {index}"),
                "当前页",
            )
        }));

        let suggestions = chat_reference_suggestions(&options, &[], "");
        assert_eq!(suggestions.len(), MAX_REFERENCE_SUGGESTIONS);
        assert_eq!(suggestions[0].id, "book");
        assert_eq!(suggestions[1].id, "section");
    }

    #[test]
    fn keyboard_navigation_wraps_in_both_directions() {
        assert_eq!(move_suggestion_index(2, 3, true), 0);
        assert_eq!(move_suggestion_index(0, 3, false), 2);
    }

    #[test]
    fn full_text_reference_adds_tool_guidance_without_eagerly_copying_the_book() {
        let prompt = build_chat_prompt_with_references(
            "概括主旨",
            &[reference("book", ChatReferenceKind::Book, "全文", "整本书")],
            false,
        );
        assert!(prompt.contains("概括主旨"));
        assert!(prompt.contains("searchBook"));
        assert!(prompt.contains("getContent"));
        assert!(prompt.contains("citation"));
        assert!(!prompt.contains("link: book"));
        assert!(!prompt.contains("locator:"));
    }

    #[test]
    fn chapter_reference_supplies_a_required_citation_link() {
        let mut chapter = reference("section", ChatReferenceKind::Section, "第七章", "当前章节");
        chapter.link = "link://j/6".into();

        let prompt = build_chat_prompt_with_references("概括本章", &[chapter], false);

        assert!(prompt.contains("引用时使用其提供的 citation"));
        assert!(prompt.contains("citation: 【6†source】"));
    }

    #[test]
    fn citation_locator_keeps_the_section_and_full_node_id() {
        assert_eq!(
            parse_chat_citation("link://j/3/chapter/paragraph-2"),
            Some(ChatCitation {
                section_index: 3,
                node: Some("chapter/paragraph-2".into()),
            })
        );
        assert_eq!(
            parse_chat_citation("link://j/4"),
            Some(ChatCitation {
                section_index: 4,
                node: None,
            })
        );
        assert_eq!(
            parse_chat_citation("link://j/5/chapter%2F%E6%AE%B5%E8%90%BD%20%232"),
            Some(ChatCitation {
                section_index: 5,
                node: Some("chapter/段落 #2".into()),
            })
        );
        assert_eq!(parse_chat_citation("link:/j/5/legacy"), None);
        assert_eq!(parse_chat_citation("https://example.com"), None);
    }
}
