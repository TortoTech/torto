//! Independent block navigation for the focus footnote popup.
use super::{Motion, MotionCurve, PageDirection, focus_scroll_duration, focus_scroll_target};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub key: egui::Id,
    pub top: f32,
    pub height: f32,
}

#[derive(Clone, Default)]
pub(super) struct Navigation {
    pub active: Option<egui::Id>,
    pub offset: f32,
    rows: Vec<Row>,
    viewport: f32,
    maximum: f32,
    motion: Option<Motion>,
    tick: Option<Instant>,
    wheel: f32,
    last_wheel: Option<Instant>,
}

impl Navigation {
    pub fn index(&self) -> usize {
        self.rows
            .iter()
            .position(|row| Some(row.key) == self.active)
            .unwrap_or(0)
    }

    pub fn synchronize(&mut self, rows: Vec<Row>, viewport: f32, content_height: f32) {
        let maximum = (content_height - viewport).max(0.0);
        if self.rows != rows || (self.viewport - viewport).abs() > 0.5 {
            let old = self.rows.get(self.index());
            let progress = old
                .map_or(0.0, |row| {
                    (self.logical_offset() - row.top) / (row.height - self.viewport).max(1.0)
                })
                .clamp(0.0, 1.0);
            let index = self
                .active
                .and_then(|key| rows.iter().position(|row| row.key == key))
                .unwrap_or_else(|| {
                    rows.iter()
                        .position(|row| row.top + row.height > self.offset)
                        .unwrap_or(0)
                });
            if let Some(row) = rows.get(index) {
                self.active = Some(row.key);
                if !self.rows.is_empty() {
                    self.offset =
                        (row.top + progress * (row.height - viewport).max(0.0)).clamp(0.0, maximum);
                }
            }
            self.motion = None;
            self.rows = rows;
        }
        self.maximum = maximum;
        self.viewport = viewport;
        self.offset = self.offset.clamp(0.0, maximum);
    }

    pub fn logical_offset(&self) -> f32 {
        self.motion.map_or(self.offset, |motion| motion.target)
    }

    fn animate(&mut self, target: f32) {
        let target = target.clamp(0.0, self.maximum);
        let mut motion = Motion::settled_with_curve(
            self.offset,
            focus_scroll_duration(target - self.offset),
            MotionCurve::EaseInOut,
        );
        motion.animate_to(target);
        self.motion = Some(motion);
        self.tick = Some(Instant::now());
    }

    pub fn select(&mut self, key: egui::Id) {
        if let Some(row) = self.rows.iter().find(|row| row.key == key) {
            self.active = Some(key);
            self.animate(row.top);
        }
    }

    pub fn navigate(&mut self, direction: PageDirection) {
        let index = self.index();
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let top = row.top.min(self.maximum);
        let bottom = (row.top + row.height - self.viewport)
            .max(top)
            .min(self.maximum);
        if let Some(target) = focus_scroll_target(
            self.logical_offset(),
            top,
            bottom,
            self.viewport * 0.8,
            direction,
        ) {
            self.animate(target);
            return;
        }
        let next = match direction {
            PageDirection::Next if index + 1 < self.rows.len() => index + 1,
            PageDirection::Previous if index > 0 => index - 1,
            PageDirection::Next => 0,
            PageDirection::Previous => self.rows.len() - 1,
        };
        if next == index {
            return;
        }
        let row = &self.rows[next];
        self.active = Some(row.key);
        let target = if direction == PageDirection::Previous {
            (row.top + row.height - self.viewport).max(row.top)
        } else {
            row.top
        };
        self.animate(target);
    }

    pub fn wheel(&mut self, delta: f32, threshold: f32, cooldown: Duration) {
        if delta.abs() <= f32::EPSILON {
            return;
        }
        if self.wheel.signum() != delta.signum() {
            self.wheel = 0.0;
        }
        self.wheel += delta;
        if self.wheel.abs() < threshold
            || self
                .last_wheel
                .is_some_and(|last| last.elapsed() < cooldown)
        {
            return;
        }
        let direction = if self.wheel < 0.0 {
            PageDirection::Next
        } else {
            PageDirection::Previous
        };
        self.wheel = 0.0;
        self.last_wheel = Some(Instant::now());
        self.navigate(direction);
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        if let Some(motion) = &mut self.motion {
            motion.advance(now.saturating_duration_since(self.tick.unwrap_or(now)));
            self.offset = motion.value;
            self.tick = Some(now);
            if motion.is_animating() {
                return true;
            }
            self.motion = None;
        }
        false
    }

    pub fn accept_manual_scroll(&mut self, offset: f32) {
        if (offset - self.offset).abs() <= 0.5 {
            return;
        }
        self.offset = offset.clamp(0.0, self.maximum);
        self.motion = None;
        let center = self.offset + self.viewport * 0.5;
        if let Some(row) = self.rows.iter().min_by(|a, b| {
            let distance = |row: &Row| {
                if center < row.top {
                    row.top - center
                } else if center > row.top + row.height {
                    center - row.top - row.height
                } else {
                    0.0
                }
            };
            distance(a).total_cmp(&distance(b))
        }) {
            self.active = Some(row.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<Row> {
        vec![
            Row {
                key: egui::Id::new(0),
                top: 0.0,
                height: 90.0,
            },
            Row {
                key: egui::Id::new(1),
                top: 110.0,
                height: 700.0,
            },
            Row {
                key: egui::Id::new(2),
                top: 830.0,
                height: 100.0,
            },
        ]
    }
    #[test]
    fn long_note_scrolls_inside_before_switching_and_boundaries_stay_in_popup() {
        let mut nav = Navigation::default();
        nav.synchronize(rows(), 300.0, 930.0);
        nav.navigate(PageDirection::Previous);
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.logical_offset(), 630.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.logical_offset(), 110.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.logical_offset(), 310.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.logical_offset(), 510.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.logical_offset(), 630.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 0);
        assert_eq!(nav.logical_offset(), 0.0);
        nav.navigate(PageDirection::Previous);
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.logical_offset(), 630.0);
        nav.navigate(PageDirection::Previous);
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.logical_offset(), 510.0);
    }
    #[test]
    fn reflow_preserves_note_identity_and_relative_progress_and_drag_updates_active() {
        let mut nav = Navigation::default();
        nav.synchronize(rows(), 300.0, 930.0);
        nav.accept_manual_scroll(310.0);
        assert_eq!(nav.index(), 1);
        let mut resized = rows();
        resized[1].height = 1100.0;
        resized[2].top = 1230.0;
        nav.synchronize(resized, 300.0, 1330.0);
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.offset, 510.0);
        nav.select(egui::Id::new(2));
        assert_eq!(nav.index(), 2);
        assert_eq!(nav.logical_offset(), 1030.0);
    }
    #[test]
    fn short_visible_notes_can_switch_even_when_scroll_offsets_are_clamped() {
        let mut nav = Navigation::default();
        nav.synchronize(
            vec![
                Row {
                    key: egui::Id::new(0),
                    top: 0.0,
                    height: 40.0,
                },
                Row {
                    key: egui::Id::new(1),
                    top: 50.0,
                    height: 40.0,
                },
            ],
            100.0,
            90.0,
        );
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 1);
        assert_eq!(nav.logical_offset(), 0.0);
        nav.navigate(PageDirection::Next);
        assert_eq!(nav.index(), 0);
        nav.navigate(PageDirection::Previous);
        assert_eq!(nav.index(), 1);
    }
}
