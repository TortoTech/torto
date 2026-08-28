use std::time::{Duration, Instant};

use super::{DesktopReader, ReaderOverlay};

impl DesktopReader {
    pub(in crate::reader) fn request_frame_repaint(&self, ctx: &egui::Context) {
        if self
            .ui
            .focus_scroll_motion
            .is_some_and(super::Motion::is_animating)
        {
            // Let the native presentation loop pace focus scrolling. A fixed
            // 16 ms timer produces uneven frame intervals on displays whose
            // refresh period is not exactly 60 Hz.
            ctx.request_repaint();
        } else if self.ui.needs_motion_tick()
            || self.pending_page_turn.is_some()
            || self.pending_reading_unit_turn.is_some()
            || self.pending_toc_navigation.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    pub(in crate::reader) fn set_sidebar_open(&mut self, open: bool) {
        if open {
            self.ui.focus_footnotes_visible = false;
            self.ui.focus_footnote_scroll_delta = 0.0;
        } else {
            self.ui.toc_keyboard_row = None;
            self.ui.last_auto_scrolled_toc_keyboard_row = None;
        }
        self.ui.sidebar_open = open;
        if self
            .ui
            .sidebar_motion
            .animate_to(if open { 1.0 } else { 0.0 })
        {
            self.ui.last_motion_tick = Some(Instant::now());
        }
    }

    pub(in crate::reader) fn set_toolbar_hovered(&mut self, hovered: bool) -> bool {
        self.ui.set_toolbar_hovered(hovered, Instant::now())
    }

    pub(in crate::reader) fn toggle_menu(&mut self) {
        if self.ui.overlay == ReaderOverlay::Menu {
            self.close_overlay();
        } else {
            self.set_overlay(ReaderOverlay::Menu);
        }
    }

    pub(in crate::reader) fn close_overlay(&mut self) {
        self.set_overlay(ReaderOverlay::None);
    }

    pub(in crate::reader) fn set_overlay(&mut self, overlay: ReaderOverlay) {
        if overlay != ReaderOverlay::None {
            self.ui.focus_footnotes_visible = false;
            self.ui.focus_footnote_scroll_delta = 0.0;
        }
        let was_menu_open = self.ui.overlay == ReaderOverlay::Menu;
        self.ui.overlay = overlay;
        let menu_changed = self
            .ui
            .menu_motion
            .animate_to(if overlay == ReaderOverlay::Menu {
                1.0
            } else {
                0.0
            });
        let now = Instant::now();
        if overlay == ReaderOverlay::Menu {
            self.ui.reveal_toolbar(now);
        } else if was_menu_open && !self.ui.toolbar_hovered {
            self.ui.schedule_toolbar_hide(now);
        }
        if menu_changed {
            self.ui.last_motion_tick = Some(now);
        }
    }

    pub(in crate::reader) fn advance_motion(&mut self, now: Instant) {
        let delta = self
            .ui
            .last_motion_tick
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        let sidebar_was_animating = self.ui.sidebar_motion.is_animating();
        let assistant_was_animating = self.ui.assistant_motion.is_animating();
        if self
            .ui
            .toolbar_hide_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.ui.toolbar_hide_at = None;
            if !self.ui.toolbar_hovered && self.ui.overlay != ReaderOverlay::Menu {
                self.ui.toolbar_motion.animate_to(0.0);
            }
        }
        self.ui.toolbar_motion.advance(delta);
        self.ui.sidebar_motion.advance(delta);
        self.ui.assistant_motion.advance(delta);
        self.ui.menu_motion.advance(delta);
        if let Some(motion) = self.ui.focus_scroll_motion.as_mut() {
            motion.advance(delta);
        }
        if let Some(target) = self
            .ui
            .focus_scroll_motion
            .filter(|motion| !motion.is_animating())
            .map(|motion| motion.target)
        {
            // Apply the exact endpoint once after the last interpolated frame,
            // then release the animation state.
            self.focus_target_offset = Some(target);
            self.ui.focus_scroll_motion = None;
        }
        self.translation.dismiss_if_due(now);

        if (sidebar_was_animating && !self.ui.sidebar_motion.is_animating())
            || (assistant_was_animating && !self.ui.assistant_motion.is_animating())
        {
            // Side panels resize live. Bump once more at the settled dimensions so
            // the final frame cannot retain an intermediate scene revision.
            self.bump_scene_revision();
        }
        let assistant_settled = assistant_was_animating && !self.ui.assistant_motion.is_animating();
        if !self.ui.assistant_motion.is_animating() && self.ui.assistant_motion.target <= 0.0 {
            self.ui.assistant_panel = None;
        }
        if assistant_settled {
            self.log_diagnostic_snapshot("assistant.motion.settled", None);
        }
        if !self.ui.needs_motion_tick() {
            self.ui.last_motion_tick = None;
        }
    }

    pub(in crate::reader) fn advance_frame(&mut self, now: Instant) {
        self.advance_motion(now);
        self.notice_timer.advance(&mut self.notice, now);
        self.error_timer.advance(&mut self.error, now);
        self.chat.error_timer.advance(&mut self.chat.error, now);
        self.retry_pending_page_turn();
        self.retry_pending_reading_unit_turn();
        self.retry_pending_toc_navigation();
    }

    pub(in crate::reader) fn apply_pending_focus_wheel_turn(&mut self) {
        let Some(direction) = self.pending_focus_wheel_turn.take() else {
            return;
        };
        if self.is_focus_mode() && !self.scroll_within_tall_focus_unit(direction) {
            self.move_focus_unit(direction);
        }
    }

    pub(in crate::reader) fn next_transient_message_deadline(&self) -> Option<Instant> {
        [
            self.notice_timer.dismiss_at,
            self.error_timer.dismiss_at,
            self.chat.error_timer.dismiss_at,
            self.translation.dismiss_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}
