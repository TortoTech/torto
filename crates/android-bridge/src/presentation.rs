//! Display controls are projected from existing AI results; toggles never call a model.
use crate::*;
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
    for_ranges(s, &ranges)
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
pub(crate) fn toggle(s: &mut ReaderState, control: &ReadingControl) -> Result<Option<SourceRange>> {
    let anchor = match control.kind.as_str() {
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
                s.session.refresh_source().map_err(error)?;
                if let Some(anchor) = anchor {
                    s.session.go_to_source(&anchor.start).map_err(error)?;
                    s.focus_anchor = Some(anchor)
                }
                s.scroll_center = true;
                scroll::invalidate(s);
                render(s)
            })
        })
        .await
    }
}
