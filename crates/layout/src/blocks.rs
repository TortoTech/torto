//! Semantic block classification before vertical flow construction.
//!
//! Reading formats converge on `publication::Block`. This module assigns the
//! pagination policy consumed by region builders without depending on EPUB,
//! PDF, a renderer, or a UI toolkit.

use rebook_publication::Block;

use crate::flow::BlockBreakability;

/// Backend-neutral role of one normalized Reading IR block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRole {
    Prose,
    Quote,
    Table,
    Figure,
    Image,
    Separator,
    VerticalGlue,
    ForcedBreak,
}

/// Pagination policy resolved before a block is compiled into `FlowItem`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockPolicy {
    pub role: BlockRole,
    pub breakability: BlockBreakability,
    pub keep_with_next: bool,
    pub forced_break_before: bool,
}

impl BlockPolicy {
    #[must_use]
    pub const fn for_block(block: &Block) -> Self {
        match block {
            Block::Text(_) => Self::new(BlockRole::Prose, BlockBreakability::Splittable),
            Block::Quote(_) => Self::new(BlockRole::Quote, BlockBreakability::Splittable),
            Block::Table(_) => Self::new(BlockRole::Table, BlockBreakability::Splittable),
            Block::Figure(_) => Self::new(BlockRole::Figure, BlockBreakability::KeepTogether),
            Block::Image(_) => Self::new(BlockRole::Image, BlockBreakability::KeepTogether),
            Block::Separator => Self {
                role: BlockRole::Separator,
                breakability: BlockBreakability::KeepTogether,
                keep_with_next: true,
                forced_break_before: false,
            },
            Block::LineBreak => Self::new(BlockRole::VerticalGlue, BlockBreakability::KeepTogether),
            Block::PageBreak => Self {
                role: BlockRole::ForcedBreak,
                breakability: BlockBreakability::KeepTogether,
                keep_with_next: false,
                forced_break_before: true,
            },
        }
    }

    const fn new(role: BlockRole, breakability: BlockBreakability) -> Self {
        Self {
            role,
            breakability,
            keep_with_next: false,
            forced_break_before: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_page_break_is_a_forced_flow_boundary() {
        let policy = BlockPolicy::for_block(&Block::PageBreak);
        assert_eq!(policy.role, BlockRole::ForcedBreak);
        assert!(policy.forced_break_before);
    }

    #[test]
    fn semantic_separator_stays_with_following_content() {
        let policy = BlockPolicy::for_block(&Block::Separator);
        assert_eq!(policy.role, BlockRole::Separator);
        assert!(policy.keep_with_next);
        assert_eq!(policy.breakability, BlockBreakability::KeepTogether);
    }

    #[test]
    fn authored_line_break_compiles_to_vertical_glue() {
        assert_eq!(
            BlockPolicy::for_block(&Block::LineBreak).role,
            BlockRole::VerticalGlue
        );
    }
}
