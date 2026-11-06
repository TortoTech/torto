//! Authored hyperlinks resolved from durable source coordinates.
use crate::{Block, Inline, LinkRole, PublicationUrl, SourceAnchor};

pub fn at_source(
    blocks: &[Block],
    anchor: &SourceAnchor,
) -> Option<(PublicationUrl, LinkRole, String)> {
    for text in blocks.iter().flat_map(Block::text_blocks) {
        let Some(range) = &text.source else { continue };
        if range.start.spine != anchor.spine || range.start.node != anchor.node {
            continue;
        }
        let mut offset = range.start.text_offset;
        for inline in &text.content {
            let len = match inline {
                Inline::Text(run) => run.text.chars().count(),
                Inline::Ruby(run) => run.source_char_len(),
                Inline::Math(run) => run.source_char_len().max(1),
                Inline::Image(_) | Inline::Break => 1,
            } as u64;
            if (offset..offset.saturating_add(len)).contains(&anchor.text_offset) {
                // Ruby base runs retain authored link roles too.
                for run in inline.text_runs() {
                    let end = offset.saturating_add(run.text.chars().count() as u64);
                    if anchor.text_offset < end {
                        return run
                            .link
                            .clone()
                            .map(|target| (target, run.style.link_role, run.text.clone()));
                    }
                    offset = end;
                }
                return None;
            }
            offset = offset.saturating_add(len);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlockStyle, RubyRun, SourceRange, SpineItemId, TextBlock, TextBlockKind, TextRun, TextStyle,
    };
    #[test]
    fn unicode_and_ruby_links_keep_authored_targets_and_roles() {
        let start = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 3,
        };
        let run = |text: &str, link: Option<PublicationUrl>| TextRun {
            text: text.into(),
            link,
            style: TextStyle {
                link_role: LinkRole::Normal,
                ..Default::default()
            },
        };
        let target = PublicationUrl::parse("next.xhtml#title").unwrap();
        let blocks = vec![Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            style: BlockStyle::default(),
            source: Some(SourceRange {
                start: start.clone(),
                end: SourceAnchor {
                    text_offset: 8,
                    ..start.clone()
                },
            }),
            content: vec![
                Inline::Text(run("中文", None)),
                Inline::Ruby(Box::new(RubyRun {
                    base: vec![run("目录", Some(target.clone()))],
                    annotation: vec![run("もくじ", None)],
                    below: false,
                })),
            ],
        })];
        assert!(
            at_source(
                &blocks,
                &SourceAnchor {
                    text_offset: 4,
                    ..start.clone()
                }
            )
            .is_none()
        );
        assert_eq!(
            at_source(
                &blocks,
                &SourceAnchor {
                    text_offset: 6,
                    ..start.clone()
                }
            )
            .unwrap()
            .0,
            target
        );
        assert!(
            at_source(
                &blocks,
                &SourceAnchor {
                    text_offset: 7,
                    ..start
                }
            )
            .is_none()
        );
    }
}
