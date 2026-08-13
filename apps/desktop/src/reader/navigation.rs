use rebook_layout::LayoutViewport;
use rebook_publication::PublicationUrl;
use rebook_reader::{PageDirection, ReaderSnapshot};

use super::{
    DesktopReader, FollowUp, MarkRetention, ProgressChange, SceneChange, SnapshotEffects,
    logical_dimension,
};

pub(super) const fn snapshot_reanchors_focus(effects: SnapshotEffects) -> bool {
    matches!(effects.scene, SceneChange::Overlays)
}

impl DesktopReader {
    pub(in crate::reader) fn go_to_toc(&mut self, id: &str, target: &PublicationUrl) {
        let result = self.reader.go_to_href(target);
        match result {
            Ok(result) => {
                let focus_anchor = self.reader.source_anchor_for_href(target);
                self.apply_snapshot(result.snapshot, SnapshotEffects::navigation());
                if self.is_focus_mode() {
                    self.focus_toc_override = Some(id.to_owned());
                    if let Some(item) = self.reader.toc_items().iter().find(|item| item.id == id) {
                        self.snapshot.active_toc_id = Some(item.id.clone());
                        self.snapshot.active_toc_path.clone_from(&item.ancestors);
                        if item.has_children {
                            self.snapshot.active_toc_path.push(item.id.clone());
                        }
                    }
                    self.focus_anchor = focus_anchor.or_else(|| {
                        self.reader
                            .current_page()
                            .leading_source_range()
                            .map(|range| range.start.clone())
                    });
                    self.focus_units.clear();
                    self.focus_unit_index = 0;
                    self.focus_target_offset = None;
                    self.ui.focus_scroll_motion = None;
                }
            }
            Err(error) => self.error = Some(format!("目录跳转失败：{error}")),
        }
    }

    pub(in crate::reader) fn go_to_adjacent_section(&mut self, direction: PageDirection) {
        self.focus_toc_override = None;
        let current = self.snapshot.location.section_index;
        let target = match direction {
            PageDirection::Previous => current.checked_sub(1),
            PageDirection::Next => {
                (current + 1 < self.reader.section_count()).then_some(current + 1)
            }
        };
        let Some(target) = target else {
            return;
        };
        match self.reader.go_to_section(target) {
            Ok(result) => self.apply_snapshot(result.snapshot, SnapshotEffects::navigation()),
            Err(error) => self.error = Some(format!("章节跳转失败：{error}")),
        }
    }

    pub(in crate::reader) fn resize_canvas(&mut self, width: f64, height: f64) {
        let width = logical_dimension(width);
        let height = logical_dimension(height);
        if width == 0 || height == 0 || self.canvas_size == Some((width, height)) {
            return;
        }
        let Ok(viewport) = LayoutViewport::new(width, height) else {
            return;
        };
        let result = self.reader.resize(viewport);
        match result {
            Ok(snapshot) => {
                self.canvas_size = Some((width, height));
                // Resizing repaginates the current spread and can expose source
                // ranges that were outside the previous viewport. Re-run the
                // incremental scheduler so those newly visible blocks are
                // translated without requiring an artificial page turn.
                self.apply_snapshot(snapshot, SnapshotEffects::viewport_change());
            }
            Err(error) => self.error = Some(format!("调整页面失败：{error}")),
        }
    }

    pub(in crate::reader) fn prefetch(&mut self) {
        let result = self
            .reader
            .prefetch_adjacent()
            .err()
            .map(|error| format!("章节预取失败：{error}"));
        self.error = result;
    }

    pub(in crate::reader) fn toggle_toc(&mut self, id: &str) {
        if !self.ui.expanded_toc.remove(id) {
            self.ui.expanded_toc.insert(id.to_owned());
        }
    }

    pub(in crate::reader) fn install_snapshot(&mut self, snapshot: ReaderSnapshot) {
        self.ui
            .expanded_toc
            .extend(snapshot.active_toc_path.iter().cloned());
        self.snapshot = snapshot;
        if let Some(id) = self.focus_toc_override.as_deref()
            && let Some(item) = self.reader.toc_items().iter().find(|item| item.id == id)
        {
            self.snapshot.active_toc_id = Some(item.id.clone());
            self.snapshot.active_toc_path.clone_from(&item.ancestors);
            if item.has_children {
                self.snapshot.active_toc_path.push(item.id.clone());
            }
        }
    }

    pub(in crate::reader) fn apply_snapshot(
        &mut self,
        snapshot: ReaderSnapshot,
        effects: SnapshotEffects,
    ) {
        self.focus_toc_override = None;
        let previous_section = self.snapshot.location.section_index;
        let target_position = rebook_reader::ReaderPosition {
            section_index: snapshot.location.section_index,
            segment_index: snapshot.location.segment_index,
            page_index: snapshot.location.page_index,
        };
        self.pending_page_turn = None;
        self.install_snapshot(snapshot);
        // Focus navigation owns a source anchor that can be more precise than the
        // ReaderSession's page-level location. A source refresh or resize rebuilds
        // pagination asynchronously, but must not replace the paragraph selected
        // by the user with the current page's leading range. Only an explicit
        // navigation operation establishes a new anchor here.
        if self.is_focus_mode() && snapshot_reanchors_focus(effects) {
            self.focus_anchor = self
                .reader
                .current_page()
                .leading_source_range()
                .map(|range| range.start.clone());
        }
        if previous_section != target_position.section_index {
            self.scroll_section = None;
            self.focus_units.clear();
            self.focus_unit_index = 0;
            self.focus_target_offset = None;
            self.ui.focus_scroll_motion = None;
        }
        if self.is_focus_mode() && matches!(effects.scene, SceneChange::StaticContent) {
            self.focus_units.clear();
            self.focus_target_offset = None;
            self.ui.focus_scroll_motion = None;
        }
        if self.is_scroll_mode() {
            self.scroll_target_position = Some(target_position);
        } else {
            self.scroll_target_position = None;
            self.scroll_viewport = None;
        }
        self.selection_toolbar_visible = false;
        self.ui.focus_actions_visible = false;
        self.annotation_note_draft = None;
        self.selection_anchor = None;
        self.selection = None;
        self.selected_image = None;
        self.image_pointer_state = super::ImagePointerState::Idle;
        match effects.marks {
            MarkRetention::Keep => {}
            MarkRetention::ClearSelectedHighlight => self.selected_highlight_id = None,
            MarkRetention::ClearAll => {
                self.selected_highlight_id = None;
                self.focused_mark = None;
            }
        }
        match effects.scene {
            SceneChange::Overlays => self.bump_scene_revision(),
            SceneChange::StaticContent => self.invalidate_page_scenes(),
        }
        self.error = None;
        if matches!(effects.progress, ProgressChange::Persist) {
            self.persist_progress();
        }
        if matches!(effects.prefetch, FollowUp::Run) {
            self.prefetch();
        }
        if matches!(effects.translation, FollowUp::Run) {
            self.queue_visible_section_translation();
        }
    }

    pub(in crate::reader) fn persist_progress(&self) {
        let Some(store) = &self.progress_store else {
            return;
        };
        let locator = self.reader.current_locator();
        if let Err(error) = store.save_progress(&self.book_id, &locator) {
            tracing::warn!(%error, book_id = %self.book_id, "failed to persist reading progress");
        }
    }

    pub(in crate::reader) fn progress(&self) -> f64 {
        self.snapshot.total_progression
    }
}
