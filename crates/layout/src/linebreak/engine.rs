//! Unicode boundary analysis through Parley's public low-level engine.
use parley_engine::{Analysis, AnalysisOptions, Analyzer};
use std::cell::RefCell;

thread_local! {
    static SCRATCH: RefCell<(Analyzer, Analysis)> = RefCell::new((Analyzer::new(), Analysis::new()));
}

pub(super) fn legal_breaks(text: &str) -> Vec<usize> {
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let (analyzer, analysis) = &mut *scratch;
        analyzer.analyze(text, &AnalysisOptions::default(), analysis);
        let mut result = Vec::new();
        result.push(0);
        result.extend(
            text.char_indices()
                .zip(analysis.char_info())
                .filter_map(|((offset, _), info)| {
                    info.is_soft_wrap_opportunity().then_some(offset)
                }),
        );
        if result.last() != Some(&text.len()) {
            result.push(text.len());
        }
        // Keep reusable scratch bounded, without retaining any source string.
        if text.len() > 256 * 1024 {
            *scratch = (Analyzer::new(), Analysis::new());
        }
        result
    })
}
