use crate::highlights::StoredHighlight;
use rebook_publication::{SourceAnchor, SourceRange};
use rebook_reader::{
    NavigationAttempt, NavigationOutcome, PageDirection, ReaderPosition, SelectionGranularity,
};

use super::{
    DesktopReader, FocusFootnote, FollowUp, MarkRetention, ProgressChange, SidebarTab,
    SnapshotEffects, focus_anchor_block_index, focus_unit_contains_source_range,
    focus_unit_matches_highlight_ranges,
};

impl DesktopReader {
    fn focus_unit_at_canvas(&mut self, x: f32, y: f32) -> Option<usize> {
        if !self.is_focus_mode() {
            return None;
        }
        let hit = self.hit_test_canvas(x, y, true).ok().flatten()?;
        let selection = self
            .reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Paragraph)
            .ok()
            .flatten()?;
        let range = selection.ranges.first()?;
        self.focus_units
            .iter()
            .position(|unit| focus_unit_contains_source_range(unit, range))
    }

    pub(in crate::reader) fn focus_clicked_unit(&mut self, x: f32, y: f32) {
        self.ui.focus_actions_visible = false;
        self.focus_toc_override = None;
        if let Some(index) = self.focus_unit_at_canvas(x, y) {
            self.select_focus_unit(index);
        }
    }

    pub(in crate::reader) fn current_focus_unit_is_image(&self) -> bool {
        self.focus_units
            .get(self.focus_unit_index)
            .is_some_and(|unit| unit.is_image)
    }

    pub(in crate::reader) fn current_focus_note(&self) -> Option<String> {
        if self.selection.is_some() && self.focus_selection_anchor.is_some() {
            let (ranges, _) = self.focus_annotation_payload()?;
            return self
                .highlights
                .iter()
                .find(|highlight| highlight.ranges == ranges)
                .and_then(|highlight| highlight.note.clone())
                .filter(|note| !note.trim().is_empty());
        }
        self.focus_note_at(self.focus_unit_index)
    }

    pub(in crate::reader) fn focus_note_at(&self, index: usize) -> Option<String> {
        let unit = self.focus_units.get(index)?;
        self.highlights
            .iter()
            .find(|highlight| focus_unit_matches_highlight_ranges(unit, &highlight.ranges))
            .and_then(|highlight| highlight.note.clone())
            .filter(|note| !note.trim().is_empty())
    }

    pub(in crate::reader) fn toggle_focus_highlight(&mut self) {
        let Some((ranges, _)) = self.focus_annotation_payload() else {
            return;
        };
        let existing = self
            .highlights
            .iter()
            .find(|highlight| highlight.ranges == ranges)
            .map(|highlight| highlight.id.clone());
        if let Some(id) = existing {
            self.remove_highlight(&id);
        } else {
            self.create_focus_highlight(None);
        }
    }

    pub(in crate::reader) fn create_focus_highlight(&mut self, note: Option<String>) {
        let Some((ranges, text)) = self.focus_annotation_payload() else {
            return;
        };
        let note = note.and_then(|note| {
            let note = note.trim().to_owned();
            (!note.is_empty()).then_some(note)
        });
        if let Some(index) = self
            .highlights
            .iter()
            .position(|highlight| highlight.ranges == ranges)
        {
            let Some(note) = note else {
                return;
            };
            let mut highlight = self.highlights[index].clone();
            highlight.note = Some(note);
            match self.highlight_store.update(&highlight) {
                Ok(true) => {
                    self.highlights[index] = highlight;
                    self.annotation_note_draft = None;
                    self.bump_scene_revision();
                    self.error = None;
                }
                Ok(false) => {
                    self.error = Some("The annotation no longer exists".into());
                }
                Err(error) => self.error = Some(format!("Failed to save annotation: {error}")),
            }
            return;
        }
        let highlight = StoredHighlight::with_note(self.book_id.clone(), ranges, text, note);
        match self.highlight_store.insert(&highlight) {
            Ok(()) => {
                self.highlights.insert(0, highlight);
                self.annotation_note_draft = None;
                self.bump_scene_revision();
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Failed to save highlight: {error}")),
        }
    }

    fn focus_annotation_payload(&self) -> Option<(Vec<SourceRange>, String)> {
        let units = self
            .focus_action_units()
            .iter()
            .filter(|unit| !unit.is_image)
            .collect::<Vec<_>>();
        let ranges = units
            .iter()
            .flat_map(|unit| unit.paint_ranges.iter().cloned())
            .collect::<Vec<_>>();
        let text = units
            .iter()
            .map(|unit| unit.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!ranges.is_empty() && !text.is_empty()).then_some((ranges, text))
    }

    fn hit_test_canvas(
        &mut self,
        x: f32,
        y: f32,
        exact: bool,
    ) -> Result<Option<rebook_reader::ReaderTextHit>, rebook_reader::ReaderError> {
        if self.is_scroll_mode() {
            let Some((position, page_x, page_y)) = self.scroll_page_coordinates(x, y) else {
                return Ok(None);
            };
            self.reader.hit_test_page(position, page_x, page_y, exact)
        } else {
            self.reader.hit_test_current_spread(x, y, exact)
        }
    }

    pub(in crate::reader) fn footnote_source_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<Option<(ReaderPosition, SourceRange)>, rebook_reader::ReaderError> {
        if self.is_scroll_mode() {
            let Some((position, page_x, page_y)) = self.scroll_page_coordinates(x, y) else {
                return Ok(None);
            };
            self.reader
                .footnote_source_at_page(position, page_x, page_y)
                .map(|source| source.map(|source| (position, source)))
        } else {
            self.reader.footnote_source_at_current_spread(x, y)
        }
    }

    pub(in crate::reader) fn website_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Option<(String, egui::Rect)> {
        let (url, bounds, offset) = if self.is_scroll_mode() {
            let (position, px, py) = self.scroll_page_coordinates(x, y)?;
            let (url, bounds) = self
                .reader
                .website_region_at_page(position, px, py)
                .ok()
                .flatten()?;
            (url, bounds, egui::vec2(x - px, y - py))
        } else {
            let (url, bounds) = self
                .reader
                .website_region_at_current_spread(x, y)
                .ok()
                .flatten()?;
            (url, bounds, egui::Vec2::ZERO)
        };
        Some((
            url,
            egui::Rect::from_min_max(
                egui::pos2(bounds[0], bounds[1]),
                egui::pos2(bounds[2], bounds[3]),
            )
            .translate(offset),
        ))
    }

    pub(in crate::reader) fn footnote_reference_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Option<(SourceRange, u32)> {
        let identity = if self.is_scroll_mode() {
            let (position, px, py) = self.scroll_page_coordinates(x, y)?;
            self.reader
                .footnote_reference_at_page(position, px, py)
                .ok()
                .flatten()?
        } else {
            self.reader
                .footnote_reference_at_current_spread(x, y)
                .ok()
                .flatten()?
                .1
        };
        self.focus_units
            .iter()
            .flat_map(|unit| &unit.footnotes)
            .filter_map(super::FocusFootnote::reference)
            .find(|(source, number)| {
                rebook_publication::source_block_identity(source) == identity.0
                    && *number == identity.1
            })
    }

    pub(in crate::reader) fn citation_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Option<(SourceRange, u32)> {
        if self.is_scroll_mode() {
            let (position, px, py) = self.scroll_page_coordinates(x, y)?;
            self.reader
                .inline_citation_at_page(position, px, py)
                .ok()
                .flatten()
        } else {
            self.reader
                .inline_citation_at_current_spread(x, y)
                .ok()
                .flatten()
                .map(|(_, hit)| hit)
        }
    }

    pub(in crate::reader) fn classic_footnotes_at_canvas(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<Option<Vec<FocusFootnote>>, rebook_reader::ReaderError> {
        if self.is_focus_mode() {
            return Ok(None);
        }
        let Some((position, source)) = self.footnote_source_at_canvas(x, y)? else {
            return Ok(None);
        };
        let Ok(section) = self.source.parse_section(position.section_index) else {
            return Ok(None);
        };
        let Some(block_index) = focus_anchor_block_index(&section.blocks, Some(&source.start))
        else {
            return Ok(None);
        };
        let footnotes = self.resolve_focus_footnotes(
            &section.blocks[block_index],
            position.section_index,
            &section,
            &mut std::collections::HashMap::new(),
        );
        Ok((!footnotes.is_empty()).then_some(footnotes))
    }

    fn source_ranges_contain_canvas_point(
        &mut self,
        ranges: &[rebook_publication::SourceRange],
        x: f32,
        y: f32,
    ) -> Result<bool, rebook_reader::ReaderError> {
        if self.is_scroll_mode() {
            let Some((position, page_x, page_y)) = self.scroll_page_coordinates(x, y) else {
                return Ok(false);
            };
            self.reader
                .source_ranges_contain_point_on_page(position, ranges, page_x, page_y)
        } else {
            self.reader.source_ranges_contain_point(ranges, x, y)
        }
    }

    pub(in crate::reader) fn request_exit(&mut self) {
        self.persist_progress();
        self.ui.focus_footnote_scroll_positions.clear();
        self.ui.focus_footnotes_visible = false;
        self.ui.focus_footnote_scroll_delta = 0.0;
        self.exit_requested = true;
    }

    pub(in crate::reader) fn begin_text_selection(&mut self, x: f32, y: f32) {
        self.focus_selection_anchor = None;
        self.selection_toolbar_visible = false;
        self.annotation_note_draft = None;
        match self.hit_test_canvas(x, y, true) {
            Ok(anchor) => {
                self.selection_anchor = anchor;
                self.selection = None;
                self.selected_highlight_id = None;
                self.bump_scene_revision();
            }
            Err(error) => self.error = Some(format!("选择文字失败：{error}")),
        }
    }

    pub(in crate::reader) fn update_text_selection(&mut self, x: f32, y: f32) {
        let Some(anchor) = self.selection_anchor.clone() else {
            return;
        };
        let result = self.hit_test_canvas(x, y, false).and_then(|focus| {
            focus.map_or(Ok(None), |focus| {
                self.reader.selection_between_with_granularity(
                    &anchor,
                    &focus,
                    self.selection_granularity,
                )
            })
        });
        match result {
            Ok(selection) if self.selection != selection => {
                self.selection = selection;
                self.bump_scene_revision();
            }
            Ok(_) => {}
            Err(error) => self.error = Some(format!("选择文字失败：{error}")),
        }
    }

    pub(in crate::reader) fn finish_text_selection(&mut self, x: f32, y: f32, moved: bool) {
        if moved {
            self.update_text_selection(x, y);
            if self.selection.is_none() {
                self.selection_anchor = None;
            }
            self.selection_toolbar_visible = self.selection.is_some();
            return;
        }

        if self.selection_granularity != SelectionGranularity::Free {
            match self.hit_test_canvas(x, y, true).and_then(|hit| {
                hit.map_or(Ok(None), |hit| {
                    self.reader
                        .selection_between_with_granularity(&hit, &hit, self.selection_granularity)
                        .map(|selection| selection.map(|selection| (hit, selection)))
                })
            }) {
                Ok(Some((hit, selection))) => {
                    self.selection_anchor = Some(hit);
                    self.selection = Some(selection);
                    self.selection_toolbar_visible = true;
                    self.annotation_note_draft = None;
                    self.selected_highlight_id = None;
                    self.bump_scene_revision();
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(format!("Text selection failed: {error}"));
                    return;
                }
            }
        }

        self.selection_toolbar_visible = false;
        self.annotation_note_draft = None;
        self.selection_anchor = None;
        self.focus_selection_anchor = None;
        self.selection = None;
        self.bump_scene_revision();
        let candidates = self
            .highlights
            .iter()
            .map(|highlight| (highlight.id.clone(), highlight.ranges.clone()))
            .collect::<Vec<_>>();
        let activated = candidates.into_iter().find_map(|(id, ranges)| {
            self.source_ranges_contain_canvas_point(&ranges, x, y)
                .ok()
                .filter(|contains| *contains)
                .map(|_| id)
        });
        if let Some(id) = activated {
            self.selected_highlight_id = Some(id);
            self.ui.sidebar_tab = SidebarTab::Highlights;
            self.set_sidebar_open(true);
        } else {
            self.selected_highlight_id = None;
        }
    }

    pub(in crate::reader) fn cancel_text_selection(&mut self) {
        self.selection_toolbar_visible = false;
        self.annotation_note_draft = None;
        self.selection_anchor = None;
        self.focus_selection_anchor = None;
        if self.selection.take().is_some() {
            self.bump_scene_revision();
        }
    }

    pub(in crate::reader) fn create_highlight(&mut self, note: Option<String>) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let highlight = StoredHighlight::with_note(
            self.book_id.clone(),
            selection.ranges,
            selection.text,
            note,
        );
        match self.highlight_store.insert(&highlight) {
            Ok(()) => {
                self.highlights.insert(0, highlight);
                self.selection_toolbar_visible = false;
                self.annotation_note_draft = None;
                self.selection_anchor = None;
                self.selection = None;
                self.selected_highlight_id = None;
                self.bump_scene_revision();
                self.error = None;
            }
            Err(error) => self.error = Some(format!("保存高亮失败：{error}")),
        }
    }

    pub(in crate::reader) fn remove_highlight(&mut self, id: &str) {
        match self.highlight_store.remove(id) {
            Ok(true) => {
                self.highlights.retain(|highlight| highlight.id != id);
                if self.selected_highlight_id.as_deref() == Some(id) {
                    self.selected_highlight_id = None;
                }
                self.bump_scene_revision();
                self.error = None;
            }
            Ok(false) => {}
            Err(error) => self.error = Some(format!("删除高亮失败：{error}")),
        }
    }

    pub(in crate::reader) fn go_to_highlight(&mut self, id: &str) {
        let Some(anchor) = self
            .highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .and_then(|highlight| highlight.ranges.first())
            .map(|range| range.start.clone())
        else {
            return;
        };
        match self.reader.go_to_source(&anchor) {
            Ok(result) => {
                let focus = self.is_focus_mode();
                if focus {
                    self.reanchor_focus_view(anchor.clone());
                }
                self.apply_snapshot(
                    result.snapshot,
                    SnapshotEffects {
                        marks: MarkRetention::Keep,
                        // Focus mode saves the position below, once the highlight's
                        // own anchor has replaced the page the snapshot names.
                        progress: if focus {
                            ProgressChange::Keep
                        } else {
                            ProgressChange::Persist
                        },
                        ..SnapshotEffects::navigation()
                    },
                );
                if focus {
                    self.focus_anchor = Some(anchor);
                    self.persist_progress();
                }
                self.selected_highlight_id = Some(id.to_owned());
            }
            Err(error) => self.error = Some(format!("高亮跳转失败：{error}")),
        }
    }

    /// Points the focus-mode view at `anchor` and drops the focus units and
    /// scroll targets that still describe the previous reading unit.
    ///
    /// Focus units belong to one reading unit, and the scroll viewport keeps the
    /// session position on them. A highlight in another chapter therefore
    /// leaves the reader on the paragraph it came from, so the jump has to
    /// re-anchor before the next frame rebuilds the units.
    fn reanchor_focus_view(&mut self, anchor: SourceAnchor) {
        self.focus_anchor = Some(anchor);
        self.scroll_section = None;
        self.invalidate_focus_units();
        self.focus_target_offset = None;
        self.ui.focus_scroll_motion = None;
    }

    pub(in crate::reader) fn set_sidebar_tab(&mut self, tab: SidebarTab) {
        if self.ui.sidebar_tab != tab {
            self.ui.toc_keyboard_row = None;
            self.ui.last_auto_scrolled_toc_keyboard_row = None;
        }
        self.ui.sidebar_tab = tab;
    }

    pub(in crate::reader) fn turn_page(&mut self, direction: PageDirection) {
        if self.pending_page_turn.is_some() {
            return;
        }
        self.pending_page_turn = Some(direction);
        self.retry_pending_page_turn();
    }

    pub(in crate::reader) fn retry_pending_page_turn(&mut self) {
        let Some(direction) = self.pending_page_turn else {
            return;
        };
        let previous_section = self.snapshot.location.section_index;
        let previous_segment = self.snapshot.location.segment_index;
        let result = self.reader.try_turn_page(direction);
        if result.is_err() {
            self.pending_page_turn = None;
        }
        match result {
            Ok(NavigationAttempt::Pending) => {}
            Ok(NavigationAttempt::Ready(result)) => {
                let moved = result.outcome == NavigationOutcome::Moved;
                if !moved && direction == PageDirection::Next {
                    self.pending_page_turn = None;
                    self.open_completion_page();
                    return;
                }
                let section_changed = result.snapshot.location.section_index != previous_section;
                let segment_changed = result.snapshot.location.segment_index != previous_segment;
                self.apply_snapshot(
                    result.snapshot,
                    SnapshotEffects {
                        prefetch: if moved && (section_changed || segment_changed) {
                            FollowUp::Run
                        } else {
                            FollowUp::None
                        },
                        translation: if moved { FollowUp::Run } else { FollowUp::None },
                        progress: if moved {
                            ProgressChange::Persist
                        } else {
                            ProgressChange::Keep
                        },
                        ..SnapshotEffects::navigation()
                    },
                );
            }
            Err(error) => self.error = Some(format!("翻页失败：{error}")),
        }
    }
}
