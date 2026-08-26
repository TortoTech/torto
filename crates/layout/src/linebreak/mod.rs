//! Paragraph line-breaking strategies.
//!
//! The optimization core is renderer-independent. Integrations with shaping
//! engines live in sibling adapter modules.

pub mod knuth_plass;
pub mod measured;
pub mod unicode;

pub(crate) mod parley;
