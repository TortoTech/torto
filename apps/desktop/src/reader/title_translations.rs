//! TOC labels and their source headings share one accepted translation.
use super::*;

fn ordinal_title(text: &str) -> Option<(&str, &str)> {
    static PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(?:(?:chapter|part|book|volume|section)\s+(?:[0-9]+|[ivxlcdm]+)|第[零〇一二三四五六七八九十百千两0-9０-９]+[章节篇部卷回]|[0-9]+(?:\.[0-9]+)*[.)、．）]?)\s*[:：.\-–—]?\s*").unwrap()
    });
    let text = text.trim();
    let prefix = PREFIX.find(text)?;
    Some((&text[..prefix.end()], &text[prefix.end()..]))
}

fn preserve_ordinal(original: &str, translated: &str) -> String {
    if let Some((ordinal, _)) = ordinal_title(original)
        && ordinal_title(&plain(translated)).is_none()
    {
        return format!("{} {}", ordinal.trim(), translated.trim());
    }
    translated.to_owned()
}

fn heading_text(block: &Block) -> Option<String> {
    let Block::Text(text) = block else {
        return None;
    };
    text.kind.is_heading().then(|| {
        text.content
            .iter()
            .flat_map(Inline::text_runs)
            .map(|run| run.text.as_str())
            .collect()
    })
}

// A separate ordinal (possibly followed by a title-page image) owns the TOC
// anchor, but only the following title text may reuse its translated wording.
fn split_title(section: &Section, row: &rebook_reader::TocViewItem) -> Option<usize> {
    let (ordinal, title) = ordinal_title(&row.label)?;
    if title.is_empty() {
        return None;
    }
    let normalize = |text: &str| {
        text.chars()
            .filter(|ch| ch.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    for (index, block) in section.blocks.iter().take(8).enumerate() {
        let Some(text) = heading_text(block) else {
            continue;
        };
        if !ordinal_title(&text).is_some_and(|(_, title)| title.is_empty()) {
            continue;
        }
        if rebook_publication::heading_ordinal_key(ordinal)
            != rebook_publication::heading_ordinal_key(&text)
        {
            continue;
        }
        if row
            .target
            .as_ref()
            .and_then(PublicationUrl::fragment)
            .is_some_and(|fragment| {
                !section.anchors.iter().any(|anchor| {
                    anchor.fragment == fragment
                        && block_source_range(block)
                            .is_some_and(|source| source.start.node == anchor.source.node)
                })
            })
        {
            continue;
        }
        let next = section
            .blocks
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, block)| {
                !matches!(block, Block::Image(_) | Block::LineBreak | Block::PageBreak)
            })?;
        if heading_text(next.1).is_some_and(|text| normalize(&text) == normalize(title)) {
            return Some(next.0);
        }
    }
    None
}

fn plain(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        output.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(end) = tail.find('>') {
            let tag = tail[1..end]
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            if tag.starts_with("t-")
                || ["strong", "em", "b", "i", "u", "sup", "sub", "small", "big"].contains(&tag)
            {
                rest = &tail[end + 1..];
                continue;
            }
        }
        output.push('<');
        rest = &tail[1..];
    }
    output.push_str(rest);
    output
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

pub(super) fn linked_rows(
    section: &Section,
    index: usize,
    rows: &[rebook_reader::TocViewItem],
) -> Vec<String> {
    let Some(Block::Text(heading)) = section.blocks.get(index) else {
        return Vec::new();
    };
    if heading
            .content
            .iter()
            .any(|i| !matches!(i, Inline::Text(r) if r.style.inline_role == InlineRole::Normal && r.style.link_role != LinkRole::FootnoteReference))
    {
        return Vec::new();
    }
    let Some(source) = &heading.source else {
        return Vec::new();
    };
    let candidates = rows
        .iter()
        .filter(|row| {
            row.target.as_ref().is_some_and(|target| {
                target.path() == section.href.path()
                    && (split_title(section, row).is_some()
                        || target.fragment().is_none_or(|fragment| {
                            section.anchors.iter().any(|a| {
                                a.fragment == fragment
                                    && a.source.spine == source.start.spine
                                    && a.source.node == source.start.node
                            })
                        }))
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }
    if !matches!(heading.kind, TextBlockKind::Heading(_))
        && heading
            .content
            .iter()
            .filter_map(|i| {
                if let Inline::Text(t) = i {
                    Some(t.text.len())
                } else {
                    None
                }
            })
            .sum::<usize>()
            > 1024
    {
        return Vec::new();
    }
    let first_heading = matches!(heading.kind, TextBlockKind::Heading(_))
        .then(|| {
            section.blocks.iter().position(
                |b| matches!(b,Block::Text(t) if matches!(t.kind,TextBlockKind::Heading(_))),
            )
        })
        .flatten();
    let original = heading
        .content
        .iter()
        .filter_map(|i| {
            if let Inline::Text(r) = i {
                Some(r.text.as_str())
            } else {
                None
            }
        })
        .collect::<String>();
    let normalize = |s: &str| {
        s.trim_start_matches(|c: char| c.is_ascii_digit() || ".):- ".contains(c))
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let normalized = normalize(&original);
    candidates
        .into_iter()
        .filter(|row| {
            if let Some(title) = split_title(section, row) {
                return title == index;
            }
            let text_match = !normalized.is_empty() && normalize(&row.label) == normalized;
            if !matches!(heading.kind, TextBlockKind::Heading(_)) && !text_match {
                return false;
            }
            row.target.as_ref().is_some_and(|target| {
                target.path() == section.href.path()
                    && match target.fragment() {
                        Some(fragment) => section.anchors.iter().any(|a| {
                            a.fragment == fragment
                                && a.source.spine == source.start.spine
                                && a.source.node == source.start.node
                        }),
                        None => first_heading == Some(index) || text_match,
                    }
            })
        })
        .map(|row| row.id.clone())
        .collect()
}

impl DesktopReader {
    fn title_rows(&self, section: usize, block: usize) -> Vec<String> {
        self.semantic_layout
            .title_links
            .get(&(section, block))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn reuse_title_translation(
        &self,
        section: usize,
        input: &TranslationBlockInput,
    ) -> Option<BlockTranslation> {
        if input.segment_index.is_some() {
            return None;
        }
        let rows = self.title_rows(section, input.block_index);
        let text = rows
            .iter()
            .find_map(|id| self.translation.canonical_titles.get(id))?;
        let text = if ordinal_title(&input.text).is_none() {
            ordinal_title(text)
                .map_or(text.as_str(), |(_, title)| title)
                .to_owned()
        } else {
            text.clone()
        };
        Some(BlockTranslation {
            block_index: input.block_index,
            segment_index: None,
            text,
        })
    }

    pub(super) fn canonicalize_title_translation(
        &mut self,
        section: usize,
        translation: &mut BlockTranslation,
    ) {
        if translation.segment_index.is_some() {
            return;
        }
        let rows = self.title_rows(section, translation.block_index);
        if rows.is_empty() {
            return;
        }
        if let Some(accepted) = rows
            .iter()
            .find_map(|id| self.translation.canonical_titles.get(id))
        {
            translation.text.clone_from(accepted);
        }
        let original = self
            .reader
            .toc_items()
            .into_iter()
            .find(|row| rows.contains(&row.id))
            .map(|row| row.label.clone())
            .unwrap_or_default();
        let canonical = preserve_ordinal(&original, &translation.text);
        let label = plain(&canonical);
        for id in rows {
            self.translation
                .canonical_titles
                .insert(id.clone(), canonical.clone());
            self.translation.toc_labels.insert(id, label.clone());
        }
        if let Some(original) = self
            .semantic_layout
            .originals
            .get(&section)
            .and_then(|section| section.blocks.get(translation.block_index))
            .and_then(heading_text)
            && ordinal_title(&original).is_none()
        {
            translation.text = ordinal_title(&canonical)
                .map_or(canonical.as_str(), |(_, title)| title)
                .to_owned();
        } else {
            translation.text = canonical;
        }
    }

    pub(super) fn accept_toc_titles(&mut self, ids: &[String], translations: &[BlockTranslation]) {
        for translation in translations {
            let Some(id) = ids.get(translation.block_index) else {
                continue;
            };
            if translation.text.trim().is_empty() {
                continue;
            }
            let original = self
                .reader
                .toc_items()
                .into_iter()
                .find(|row| row.id == *id)
                .map(|row| row.label.clone())
                .unwrap_or_default();
            let accepted = self
                .translation
                .canonical_titles
                .entry(id.clone())
                .or_insert_with(|| preserve_ordinal(&original, &translation.text));
            self.translation
                .toc_labels
                .insert(id.clone(), plain(accepted));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_heading_reuses_only_title_text_and_keeps_toc_ordinal_in_both_completion_orders() {
        for toc_first in [false, true] {
            let (mut reader, mut section, _) =
                super::super::semantic_layout::tests::fixture_with_toc(vec![
                    rebook_publication::TocEntry {
                        label: "CHAPTER 1 Accessible design".into(),
                        href: Some(PublicationUrl::parse("chapter.xhtml").unwrap()),
                        children: vec![],
                    },
                ]);
            let mut ordinal = section.blocks[0].clone();
            let Block::Text(text) = &mut ordinal else {
                panic!();
            };
            text.kind = TextBlockKind::Heading(2);
            let Inline::Text(run) = &mut text.content[0] else {
                panic!();
            };
            run.text = "CHAPTER 1".into();
            let mut title = ordinal.clone();
            let Block::Text(text) = &mut title else {
                panic!();
            };
            text.source.as_mut().unwrap().start.node = "title".into();
            text.source.as_mut().unwrap().end.node = "title".into();
            let Inline::Text(run) = &mut text.content[0] else {
                panic!();
            };
            run.text = "Accessible design".into();
            section.blocks = vec![ordinal, title];
            let row = reader.reader.toc_items()[0].clone();
            assert!(linked_rows(&section, 0, std::slice::from_ref(&row)).is_empty());
            assert_eq!(
                linked_rows(&section, 1, std::slice::from_ref(&row)),
                vec![row.id.clone()]
            );
            reader
                .semantic_layout
                .originals
                .insert(0, Arc::new(section));
            reader
                .semantic_layout
                .title_links
                .insert((0, 1), vec![row.id.clone()]);
            let toc = BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "第一章 无障碍设计".into(),
            };
            if toc_first {
                reader.accept_toc_titles(std::slice::from_ref(&row.id), std::slice::from_ref(&toc));
            }
            let mut title = BlockTranslation {
                block_index: 1,
                segment_index: None,
                text: "无障碍设计".into(),
            };
            reader.canonicalize_title_translation(0, &mut title);
            reader.accept_toc_titles(std::slice::from_ref(&row.id), &[toc]);
            assert_eq!(title.text, "无障碍设计");
            assert!(ordinal_title(&reader.translation.toc_labels[&row.id]).is_some());
            let reused = reader
                .reuse_title_translation(
                    0,
                    &TranslationBlockInput {
                        block_index: 1,
                        segment_index: None,
                        text: "Accessible design".into(),
                    },
                )
                .unwrap();
            assert_eq!(reused.text, "无障碍设计");
        }
    }
    #[test]
    fn toc_target_links_to_its_heading_and_never_a_same_named_other_section() {
        let (mut reader, _, _) = super::super::semantic_layout::tests::fixture();
        let original = reader.semantic_source.original().parse_section(0).unwrap();
        let mut section = original.clone();
        let Block::Text(text) = &mut section.blocks[0] else {
            panic!()
        };
        text.kind = TextBlockKind::Heading(1);
        let row = rebook_reader::TocViewItem {
            id: "chapter".into(),
            label: "Heading".into(),
            target: Some(section.href.clone()),
            depth: 0,
            ancestors: vec![],
            has_children: false,
        };
        assert_eq!(
            linked_rows(&section, 0, std::slice::from_ref(&row)),
            vec!["chapter"]
        );
        reader
            .semantic_layout
            .originals
            .insert(0, Arc::new(section));
        reader.accept_toc_titles(
            &["chapter".into()],
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "统一标题".into(),
            }],
        );
        reader.accept_toc_titles(
            &["chapter".into()],
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "另一个翻译".into(),
            }],
        );
        assert_eq!(reader.translation.toc_labels["chapter"], "统一标题");
    }

    #[test]
    fn heading_and_toc_share_the_first_accepted_translation_in_both_completion_orders() {
        for toc_first in [false, true] {
            let (mut reader, mut section, _) =
                super::super::semantic_layout::tests::fixture_with_toc(vec![
                    rebook_publication::TocEntry {
                        label: "Heading".into(),
                        href: Some(PublicationUrl::parse("chapter.xhtml").unwrap()),
                        children: vec![],
                    },
                ]);
            let Block::Text(t) = &mut section.blocks[0] else {
                panic!()
            };
            t.kind = TextBlockKind::Heading(1);
            reader
                .semantic_layout
                .originals
                .insert(0, Arc::new(section));
            let id = reader.reader.toc_items()[0].id.clone();
            reader
                .semantic_layout
                .title_links
                .insert((0, 0), vec![id.clone()]);
            let toc = BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "目录译文".into(),
            };
            let mut heading = BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "<strong>标题译文</strong>".into(),
            };
            if toc_first {
                reader.accept_toc_titles(std::slice::from_ref(&id), std::slice::from_ref(&toc));
            }
            reader.canonicalize_title_translation(0, &mut heading);
            reader.accept_toc_titles(std::slice::from_ref(&id), std::slice::from_ref(&toc));
            let expected = if toc_first {
                "目录译文"
            } else {
                "标题译文"
            };
            assert_eq!(reader.translation.toc_labels[&id], expected);
            assert_eq!(plain(&heading.text), expected);
            let reused = reader
                .reuse_title_translation(
                    0,
                    &TranslationBlockInput {
                        block_index: 0,
                        segment_index: None,
                        text: "Heading".into(),
                    },
                )
                .unwrap();
            assert_eq!(reused.text, heading.text);
        }
    }
}
