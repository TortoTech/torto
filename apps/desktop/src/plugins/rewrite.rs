use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rebook_publication::{
    Block, Book, BookSource, Inline, PublicationError, PublicationUrl, RasterResource, Resource,
    Section, TextRun, TextStyle,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRewrite {
    pub section_index: usize,
    pub block_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteTransaction {
    previous: Vec<(RewriteKey, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RewriteKey {
    section_index: usize,
    block_id: String,
}

/// A derived, in-memory publication layer. The canonical book remains
/// untouched; reparsing a section overlays only model-approved text blocks.
pub struct RewriteBookSource {
    inner: Arc<dyn BookSource>,
    rewrites: RwLock<HashMap<RewriteKey, String>>,
}

impl RewriteBookSource {
    pub fn new(inner: Arc<dyn BookSource>) -> Self {
        Self {
            inner,
            rewrites: RwLock::new(HashMap::new()),
        }
    }

    pub fn apply_rewrites(&self, rewrites: &[BlockRewrite]) -> Result<RewriteTransaction, String> {
        let mut store = self
            .rewrites
            .write()
            .map_err(|_| "正文改写状态已损坏".to_owned())?;
        let mut previous = Vec::with_capacity(rewrites.len());
        for rewrite in rewrites {
            let key = RewriteKey {
                section_index: rewrite.section_index,
                block_id: rewrite.block_id.clone(),
            };
            let old = store.insert(key.clone(), rewrite.text.clone());
            previous.push((key, old));
        }
        Ok(RewriteTransaction { previous })
    }

    pub fn rollback(&self, transaction: RewriteTransaction) -> Result<(), String> {
        let mut store = self
            .rewrites
            .write()
            .map_err(|_| "正文改写状态已损坏".to_owned())?;
        for (key, previous) in transaction.previous.into_iter().rev() {
            if let Some(previous) = previous {
                store.insert(key, previous);
            } else {
                store.remove(&key);
            }
        }
        Ok(())
    }

    pub fn list_rewrites(&self, section_index: Option<usize>) -> Result<Vec<BlockRewrite>, String> {
        let store = self
            .rewrites
            .read()
            .map_err(|_| "正文改写状态已损坏".to_owned())?;
        let mut rewrites = store
            .iter()
            .filter(|(key, _)| section_index.is_none_or(|index| key.section_index == index))
            .map(|(key, text)| BlockRewrite {
                section_index: key.section_index,
                block_id: key.block_id.clone(),
                text: text.clone(),
            })
            .collect::<Vec<_>>();
        rewrites.sort_by(|left, right| {
            (left.section_index, &left.block_id).cmp(&(right.section_index, &right.block_id))
        });
        Ok(rewrites)
    }

    pub fn clear_rewrites(
        &self,
        section_index: Option<usize>,
    ) -> Result<(RewriteTransaction, Vec<BlockRewrite>), String> {
        let mut store = self
            .rewrites
            .write()
            .map_err(|_| "正文改写状态已损坏".to_owned())?;
        let keys = store
            .keys()
            .filter(|key| section_index.is_none_or(|index| key.section_index == index))
            .cloned()
            .collect::<Vec<_>>();
        let mut previous = Vec::with_capacity(keys.len());
        let mut cleared = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(text) = store.remove(&key) {
                cleared.push(BlockRewrite {
                    section_index: key.section_index,
                    block_id: key.block_id.clone(),
                    text: text.clone(),
                });
                previous.push((key, Some(text)));
            }
        }
        Ok((RewriteTransaction { previous }, cleared))
    }
}

impl BookSource for RewriteBookSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }

    fn table_of_contents_origin(&self) -> rebook_publication::TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.inner.parse_section(index)?;
        let rewrites = self
            .rewrites
            .read()
            .map_err(|_| PublicationError::InvalidPublication("正文改写状态已损坏".into()))?;
        for block in &mut section.blocks {
            let Block::Text(block) = block else {
                continue;
            };
            let Some(source) = &mut block.source else {
                continue;
            };
            let key = RewriteKey {
                section_index: index,
                block_id: source.start.node.clone(),
            };
            let Some(text) = rewrites.get(&key) else {
                continue;
            };
            let style = block.content.iter().find_map(|inline| match inline {
                Inline::Text(run) => Some(run.style),
                Inline::Math(_) | Inline::Image(_) | Inline::Break => None,
            });
            block.content = replacement_content(text, style.unwrap_or_default());
            source.end.spine = source.start.spine.clone();
            source.end.node.clone_from(&source.start.node);
            source.end.text_offset = source
                .start
                .text_offset
                .saturating_add(u64::try_from(text.chars().count()).unwrap_or(u64::MAX));
            for anchor in &mut section.anchors {
                if anchor.source.spine == source.start.spine
                    && anchor.source.node == source.start.node
                {
                    anchor.source.text_offset = anchor
                        .source
                        .text_offset
                        .clamp(source.start.text_offset, source.end.text_offset);
                }
            }
        }
        Ok(section)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.inner.raster_resource(href)
    }

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.inner.fixed_page_dimensions(section_index)
    }
}

fn replacement_content(text: &str, style: TextStyle) -> Vec<Inline> {
    let mut content = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            content.push(Inline::Break);
        }
        if !line.is_empty() {
            content.push(Inline::Text(TextRun {
                text: line.to_owned(),
                style,
                link: None,
            }));
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use rebook_publication::{
        BlockStyle, Metadata, PublicationId, SourceAnchor, SourceRange, SpineItem, SpineItemId,
        TextBlock, TextBlockKind,
    };

    use super::*;

    struct TestSource {
        book: Book,
        section: Section,
    }

    impl BookSource for TestSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Ok(self.section.clone())
        }

        fn resource(&self, _href: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }

    fn source() -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("chapter").unwrap();
        let href = PublicationUrl::parse("chapter.xhtml").unwrap();
        let range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 8,
            },
        };
        Arc::new(TestSource {
            book: Book {
                id: PublicationId::new("rewrite-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section: Section {
                id: spine,
                href,
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "original".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(range),
                })],
                anchors: Vec::new(),
            },
        })
    }

    #[test]
    fn overlays_and_rolls_back_rewrites_without_mutating_the_inner_source() {
        let source = RewriteBookSource::new(source());
        let transaction = source
            .apply_rewrites(&[BlockRewrite {
                section_index: 0,
                block_id: "paragraph-1".into(),
                text: "rewritten\ntext".into(),
            }])
            .unwrap();

        let rewritten = source.parse_section(0).unwrap();
        let Block::Text(block) = &rewritten.blocks[0] else {
            panic!("expected text block");
        };
        assert_eq!(
            block
                .content
                .iter()
                .map(|inline| match inline {
                    Inline::Text(run) => run.text.as_str(),
                    Inline::Math(run) => run.latex.as_str(),
                    Inline::Image(_) => "",
                    Inline::Break => "\n",
                })
                .collect::<String>(),
            "rewritten\ntext"
        );
        assert_eq!(block.source.as_ref().unwrap().end.text_offset, 14);

        source.rollback(transaction).unwrap();
        let original = source.parse_section(0).unwrap();
        let Block::Text(block) = &original.blocks[0] else {
            panic!("expected text block");
        };
        assert_eq!(block.source.as_ref().unwrap().end.text_offset, 8);
    }

    #[test]
    fn lists_clears_and_restores_rewrites_transactionally() {
        let source = RewriteBookSource::new(source());
        source
            .apply_rewrites(&[BlockRewrite {
                section_index: 0,
                block_id: "paragraph-1".into(),
                text: "rewritten".into(),
            }])
            .unwrap();

        assert_eq!(source.list_rewrites(None).unwrap().len(), 1);
        let (transaction, cleared) = source.clear_rewrites(Some(0)).unwrap();
        assert_eq!(cleared.len(), 1);
        assert!(source.list_rewrites(None).unwrap().is_empty());

        source.rollback(transaction).unwrap();
        assert_eq!(source.list_rewrites(Some(0)).unwrap()[0].text, "rewritten");
    }
}
