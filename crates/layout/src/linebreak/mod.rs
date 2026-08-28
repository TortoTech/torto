//! Paragraph line-breaking strategies.
//!
//! The optimization core is renderer-independent. Integrations with shaping
//! engines live in sibling adapter modules.

pub mod knuth_plass;

pub(crate) mod parley;

pub use parley::{MeasuredCluster, ParagraphPlan, SpacingAdjustment, plan_measured_text};
