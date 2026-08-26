use crate::highlights::StoredHighlight;
use rebook_publication::SourceRange;
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

    pub(in crate::reader) fn create_focus_highlight(&mut self, note: Option<String>) {
        let Some(unit) = self
            .focus_units
            .get(self.focus_unit_index)
            .filter(|unit| !unit.is_image)
            .cloned()
        else {
            return;
        };
        let note = note.and_then(|note| {
            let note = note.trim().to_owned();
            (!note.is_empty()).then_some(note)
        });
        if let Some(index) = self
            .highlights
            .iter()
            .position(|highlight| focus_unit_matches_highlight_ranges(&unit, &highlight.ranges))
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
        let highlight =
            StoredHighlight::with_note(self.book_id.clone(), unit.paint_ranges, unit.text, note);
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

    fn footnote_source_at_canvas(
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
        self.exit_requested = true;
    }

    pub(in crate::reader) fn begin_text_selection(&mut self, x: f32, y: f32) {
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
                self.apply_snapshot(
                    result.snapshot,
                    SnapshotEffects {
                        marks: MarkRetention::Keep,
                        ..SnapshotEffects::navigation()
                    },
                );
                self.selected_highlight_id = Some(id.to_owned());
            }
            Err(error) => self.error = Some(format!("高亮跳转失败：{error}")),
        }
    }

    pub(in crate::reader) fn set_sidebar_tab(&mut self, tab: SidebarTab) {
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
