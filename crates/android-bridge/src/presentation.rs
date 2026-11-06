//! Local presentation controls; toggles never call a model.
use crate::*;
use rebook_app_core::plugins;
#[derive(Clone, uniffi::Record)]
pub struct ReadingControl {
    pub kind: String,
    pub source: String,
    pub active: bool,
}
pub(crate) fn controls(s: &mut ReaderState) -> Result<Vec<ReadingControl>> {
    if s.session.style().typesetting.mode != TypesettingMode::Unified {
        return Ok(vec![]);
    }
    let mut ranges = if s.focus {
        s.focus_units
            .iter()
            .find(|u| {
                s.focus_anchor.as_ref().is_some_and(|a| {
                    u.ranges
                        .iter()
                        .any(|r| r.start.spine == a.start.spine && r.start.node == a.start.node)
                })
            })
            .map(|u| u.ranges.clone())
            .unwrap_or_default()
    } else {
        let mut ranges = s
            .session
            .current_visible_content_sources()
            .map_err(error)?
            .into_iter()
            .flat_map(|(_, ranges)| ranges)
            .collect::<Vec<_>>();
        let spread = s.session.current_spread().map_err(error)?;
        ranges.extend(spread.primary.geometry_sources().cloned());
        if let Some(page) = spread.secondary {
            ranges.extend(page.geometry_sources().cloned());
        }
        ranges.extend(scroll::visible_ranges(s));
        ranges
    };
    ranges.sort_by_key(|r| {
        (
            r.start.spine.as_str().to_owned(),
            r.start.node.clone(),
            r.start.text_offset,
        )
    });
    ranges.dedup();
    let mut controls = for_ranges(s, &ranges)?;
    let candidates = sentence_candidates(s, &ranges)?;
    if !candidates.is_empty() {
        controls.push(ReadingControl {
            kind: "sentences".into(),
            source: serde_json::to_string(
                &candidates
                    .iter()
                    .map(|(range, _, _)| range)
                    .collect::<Vec<_>>(),
            )
            .map_err(error)?,
            active: candidates.iter().all(|(_, _, active)| *active),
        });
    }
    Ok(controls)
}
pub(crate) fn for_ranges(s: &ReaderState, ranges: &[SourceRange]) -> Result<Vec<ReadingControl>> {
    let mut controls = Vec::new();
    let mut formulas = Vec::new();
    for source in ranges {
        if !s
            .pipeline
            .semantic
            .formula_notes_for_sources(std::slice::from_ref(source))
            .is_empty()
            && !formulas.iter().any(|r: &SourceRange| {
                r.start.spine == source.start.spine && r.start.node == source.start.node
            })
        {
            formulas.push(source.clone());
            if formulas.len() == 128 {
                break;
            }
        }
    }
    if !formulas.is_empty() {
        controls.push(ReadingControl {
            kind: "formula".into(),
            source: serde_json::to_string(&formulas).map_err(error)?,
            active: s.pipeline.semantic.formula_notes_visible(&formulas),
        });
    }
    for range in ranges {
        if let Some((root, active)) = s.pipeline.semantic.chart_control(range) {
            let source = serde_json::to_string(&root).map_err(error)?;
            if !controls
                .iter()
                .any(|c| c.kind == "chart" && c.source == source)
            {
                controls.push(ReadingControl {
                    kind: "chart".into(),
                    source,
                    active,
                });
            }
        }
    }
    Ok(controls)
}

type SentenceCandidate = (SourceRange, plugins::ParagraphStructureKey, bool);
fn sentence_candidates(s: &ReaderState, ranges: &[SourceRange]) -> Result<Vec<SentenceCandidate>> {
    if !s.focus {
        return Ok(vec![]);
    }
    let mut candidates = Vec::new();
    for (index, section) in s.session.current_reading_unit_content().map_err(error)? {
        for block in &section.blocks {
            for range in rebook_app_core::reading_text::focus_block_structure_ranges(block) {
                if !ranges.iter().any(|current| {
                    current.start.spine == range.start.spine
                        && current.start.node == range.start.node
                }) {
                    continue;
                }
                let key = plugins::ParagraphStructureKey {
                    section_index: index,
                    node: range.start.node.clone(),
                };
                if candidates.iter().any(|(_, existing, _)| existing == &key) {
                    continue;
                }
                let active = s.pipeline.structure.is_active(&key);
                let can_structure = block
                    .text_blocks()
                    .find(|text| {
                        text.source
                            .as_ref()
                            .is_some_and(|r| r.start.node == range.start.node)
                    })
                    .is_some_and(|text| {
                        s.pipeline.structure.can_structure_text(
                            text,
                            matches!(block, rebook_publication::Block::Quote(_)),
                        )
                    });
                if active || can_structure {
                    candidates.push((range, key, active));
                }
            }
        }
    }
    Ok(candidates)
}

pub(crate) fn toggle(s: &mut ReaderState, control: &ReadingControl) -> Result<Option<SourceRange>> {
    let anchor = match control.kind.as_str() {
        "sentences" => {
            // An action queued before a turn must never modify the new focused paragraph.
            let current = controls(s)?.into_iter().find(|c| c.kind == "sentences");
            if current.as_ref().is_none_or(|c| c.source != control.source) {
                return Ok(None);
            }
            let ranges: Vec<SourceRange> = serde_json::from_str(&control.source).map_err(error)?;
            let candidates = sentence_candidates(s, &ranges)?;
            let activate = !candidates.iter().all(|(_, _, active)| *active);
            for (_, key, _) in candidates {
                s.pipeline
                    .structure
                    .set_active(key, activate)
                    .map_err(error)?;
            }
            s.focus_anchor
                .clone()
                .or_else(|| s.session.current_locator().source)
        }
        "formula" => {
            let ranges: Vec<SourceRange> = serde_json::from_str(&control.source).map_err(error)?;
            if ranges.len() > 128 || !s.pipeline.semantic.toggle_formula_notes(&ranges) {
                return Err(error("Formula notes are unavailable"));
            }
            None
        }
        "chart" => {
            let root: SourceRange = serde_json::from_str(&control.source).map_err(error)?;
            if !s.pipeline.semantic.toggle_chart(&root) {
                return Err(error("Chart is unavailable"));
            }
            s.pipeline.semantic.chart_anchor(&root)
        }
        _ => return Err(error("Unknown reading control")),
    };
    Ok(anchor)
}
#[uniffi::export]
impl MobileReader {
    pub async fn toggle_reading_control(
        self: Arc<Self>,
        control: ReadingControl,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let anchor = toggle(s, &control)?;
                reflow::refresh(s)?;
                if !s.focus && let Some(anchor) = anchor {
                    s.session.go_to_source(&anchor.start).map_err(error)?;
                }
                render(s)
            })
        })
        .await
    }
}
