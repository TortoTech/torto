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
        } else if self.ui.is_animating() {
            crate::ui::request_repaint_in(ctx, Duration::from_millis(16));
        } else if self.pending_page_turn.is_some()
            || self.pending_reading_unit_turn.is_some()
            || self.pending_toc_navigation.is_some()
        {
            // Waiting for background layout does not need animation-rate frames.
            crate::ui::request_repaint_in(ctx, Duration::from_millis(50));
        }
        if let Some(deadline) = self.ui.toolbar_hide_at {
            crate::ui::request_repaint_in(ctx, deadline.saturating_duration_since(Instant::now()));
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
        let mut toolbar_delta = delta;
        if self
            .ui
            .toolbar_hide_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.ui.toolbar_hide_at = None;
            if !self.ui.toolbar_hovered && self.ui.overlay != ReaderOverlay::Menu {
                self.ui.toolbar_motion.animate_to(0.0);
                // The elapsed wait belongs to the hide timer, not the new
                // animation. Preserve its fade when waking from an idle frame.
                toolbar_delta = Duration::ZERO;
            }
        }
        self.ui.toolbar_motion.advance(toolbar_delta);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_header_waits_for_presented_body_then_uses_existing_hide_timer() {
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        let classic = crate::reader::chrome::SidebarState {
            open: false,
            pinned: false,
            width: 300.0,
        };
        let mode = reader.reading_mode;
        reader.adopt_opening_chrome(mode, classic.for_mode(mode), classic);
        assert_eq!(reader.classic_sidebar_state(), classic);
        assert_eq!(reader.ui.toolbar_motion.value, 1.0);
        let now = Instant::now();
        reader.ui.schedule_toolbar_hide(now);
        reader.advance_motion(now + Duration::from_secs(10));
        assert!(reader.ui.opening_toolbar_pending);
        assert_eq!(reader.ui.toolbar_hide_at, None);
        assert_eq!(reader.ui.toolbar_motion.value, 1.0);
        let ctx = egui::Context::default();
        reader.mark_open_presented(&ctx);
        let deadline = reader.ui.toolbar_hide_at.unwrap();
        assert!(!reader.ui.opening_toolbar_pending);
        assert!(deadline >= now + crate::reader::TOOLBAR_HIDE_DELAY);
        reader.mark_open_presented(&ctx);
        assert_eq!(reader.ui.toolbar_hide_at, Some(deadline));
        reader.advance_motion(deadline);
        assert!(reader.ui.toolbar_motion.is_animating());
        reader.advance_motion(deadline + crate::reader::TOOLBAR_MOTION_DURATION);
        assert_eq!(reader.ui.toolbar_motion.value, 0.0);
        assert!(!reader.ui.needs_motion_tick());
    }

    #[test]
    fn header_hover_keeps_it_open_and_handoff_preserves_sidebar_action() {
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        let mode = reader.reading_mode;
        let classic = crate::reader::chrome::SidebarState {
            open: false,
            ..Default::default()
        };
        let sidebar = crate::reader::chrome::SidebarState {
            open: true,
            pinned: false,
            width: 300.0,
        };
        reader.adopt_opening_chrome(mode, sidebar, classic);
        assert!(reader.ui.sidebar_open);
        assert!(!reader.ui.sidebar_pinned);
        assert_eq!(reader.ui.sidebar_width, 300.0);
        let now = Instant::now();
        reader.set_toolbar_hovered(true);
        reader.mark_open_presented(&egui::Context::default());
        assert_eq!(reader.ui.toolbar_hide_at, None);
        assert_eq!(reader.ui.toolbar_motion.value, 1.0);
        reader.ui.set_toolbar_hovered(false, now);
        assert_eq!(
            reader.ui.toolbar_hide_at,
            Some(now + crate::reader::TOOLBAR_HIDE_DELAY)
        );
        reader.ui.classic_sidebar = classic;
        reader
            .ui
            .install_sidebar(classic.for_mode(crate::preferences::ReadingMode::Focus));
        assert!(!reader.ui.sidebar_open && !reader.ui.sidebar_pinned);
        reader.reading_mode = crate::preferences::ReadingMode::Focus;
        reader.leave_focus_mode_for_pdf();
        assert!(!reader.ui.sidebar_open && reader.ui.sidebar_pinned);
    }

    #[test]
    fn switching_modes_restores_classic_sidebar_without_reopening_focus_sidebar() {
        use crate::preferences::{ReaderPreferences, ReadingMode};
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        let preferences = ReaderPreferences::default();
        let mut settings = crate::settings::AppliedSettings {
            spread: preferences.spread,
            reading_mode: ReadingMode::Classic,
            hide_cursor_in_focus_mode: preferences.hide_cursor_in_focus_mode,
            interface_typography: preferences.interface_typography,
            typography: preferences.typography,
            typesetting: preferences.typesetting,
            plugin_settings: reader.plugin_settings.clone(),
            language: preferences.language,
            theme: preferences.theme,
            selection_granularity: preferences.selection_granularity,
            shortcuts: preferences.shortcuts,
            sync_settings: reader.sync_settings.clone(),
            sync_password: reader.sync_password.clone(),
        };
        let classic = crate::reader::chrome::SidebarState {
            open: false,
            pinned: false,
            width: 310.0,
        };
        reader.ui.classic_sidebar = classic;
        reader.apply_global_settings(&settings);
        assert_eq!(reader.reading_mode, ReadingMode::Classic);
        assert_eq!(reader.classic_sidebar_state(), classic);
        settings.reading_mode = ReadingMode::Focus;
        reader.apply_global_settings(&settings);
        assert_eq!(reader.reading_mode, ReadingMode::Focus);
        assert!(!reader.ui.sidebar_open && !reader.ui.sidebar_pinned);
        reader.set_sidebar_open(true);
        settings.reading_mode = ReadingMode::Classic;
        reader.apply_global_settings(&settings);
        assert_eq!(reader.classic_sidebar_state(), classic);
    }

    #[test]
    fn idle_reader_waits_for_toolbar_deadline_and_paces_layout_polling() {
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        reader.ui.toolbar_motion = crate::reader::Motion::settled(0.0);
        reader.ui.sidebar_motion = crate::reader::Motion::settled(0.0);
        reader.ui.assistant_motion = crate::reader::Motion::settled(0.0);
        reader.ui.menu_motion = crate::reader::Motion::settled(0.0);
        reader.ui.focus_scroll_motion = None;
        reader.ui.toolbar_hide_at = None;
        let ctx = egui::Context::default();
        for pass in 0..8 {
            if pass == 5 {
                reader.ui.toolbar_hide_at = Some(Instant::now() + Duration::from_secs(2));
            } else if pass == 6 {
                reader.ui.toolbar_hide_at = None;
                reader.pending_page_turn = Some(rebook_reader::PageDirection::Next);
            } else if pass == 7 {
                reader.pending_page_turn = None;
            }
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(f64::from(pass) * 0.1),
                    ..Default::default()
                },
                |_| reader.request_frame_repaint(&ctx),
            );
            let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
            if pass == 4 || pass == 7 {
                assert_eq!(delay, Duration::MAX);
            } else if pass == 5 {
                assert!(delay > Duration::from_secs(1));
            } else if pass == 6 {
                assert_eq!(delay, Duration::from_millis(50));
            }
        }
        let now = Instant::now();
        reader.ui.toolbar_motion = crate::reader::Motion::settled(1.0);
        reader.ui.toolbar_hovered = false;
        reader.ui.overlay = ReaderOverlay::None;
        reader.ui.last_motion_tick = Some(now);
        reader.ui.toolbar_hide_at = Some(now + Duration::from_secs(1));
        reader.advance_motion(now + Duration::from_secs(2));
        assert!(reader.ui.toolbar_motion.is_animating());
        assert_eq!(reader.ui.toolbar_motion.value, 1.0);
        reader.advance_motion(now + Duration::from_secs(2) + Duration::from_millis(30));
        assert!(reader.ui.toolbar_motion.value < 1.0);
    }
}
