//! Discrete focus navigation: one precision-scroll gesture advances one step.
use super::PageDirection;
use egui::{Event, MouseWheelUnit, TouchPhase};
use std::time::{Duration, Instant};

const GESTURE_GAP: Duration = Duration::from_millis(180);

#[derive(Default)]
pub(super) struct Navigation {
    accumulated: f32,
    last_input: Option<Instant>,
    last_turn: Option<Instant>,
    precision: bool,
    fired: bool,
}

impl Navigation {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn turn(
        &mut self,
        events: &[Event],
        now: Instant,
        threshold: f32,
        cooldown: Duration,
    ) -> Option<PageDirection> {
        let mut result = None;
        for event in events {
            let Event::MouseWheel {
                unit,
                delta,
                phase,
                modifiers,
            } = event
            else {
                continue;
            };
            if modifiers.ctrl || modifiers.command {
                continue;
            }
            let phased = *phase != TouchPhase::Move;
            if delta.y.abs() <= f32::EPSILON && !phased {
                continue;
            }
            // Windows can expose precision touchpads as fractional line deltas.
            // Other platforms provide pixel deltas or explicit gesture phases.
            let precision = *unit == MouseWheelUnit::Point
                || (*unit == MouseWheelUnit::Line && delta.y.fract().abs() > 0.001)
                || phased;
            if *phase == TouchPhase::Start
                || self
                    .last_input
                    .is_none_or(|last| now.saturating_duration_since(last) >= GESTURE_GAP)
            {
                self.accumulated = 0.0;
                self.precision = precision;
                self.fired = false;
            }
            self.precision |= precision;
            self.last_input = Some(now);
            if matches!(phase, TouchPhase::End | TouchPhase::Cancel) {
                self.accumulated = 0.0;
                // Retain the latch: inertial events can follow finger release.
                continue;
            }
            if self.precision && self.fired {
                continue;
            }
            let delta = delta.y
                * match unit {
                    MouseWheelUnit::Point => 1.0,
                    MouseWheelUnit::Line => 50.0,
                    MouseWheelUnit::Page => 240.0,
                };
            if delta.abs() <= f32::EPSILON {
                continue;
            }
            if self.accumulated.signum() != delta.signum() {
                self.accumulated = 0.0;
            }
            self.accumulated += delta;
            if self.accumulated.abs() < threshold
                || self
                    .last_turn
                    .is_some_and(|last| now.saturating_duration_since(last) < cooldown)
            {
                continue;
            }
            result = Some(if self.accumulated < 0.0 {
                PageDirection::Next
            } else {
                PageDirection::Previous
            });
            self.accumulated = 0.0;
            self.last_turn = Some(now);
            self.fired = self.precision;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, Vec2};

    fn event(unit: MouseWheelUnit, delta: f32, phase: TouchPhase) -> Event {
        Event::MouseWheel {
            unit,
            delta: Vec2::new(0.0, delta),
            phase,
            modifiers: Modifiers::NONE,
        }
    }

    fn turn(nav: &mut Navigation, event: Event, now: Instant) -> Option<PageDirection> {
        nav.turn(&[event], now, 18.0, Duration::from_millis(120))
    }

    #[test]
    fn precision_gesture_and_inertial_tail_only_advance_once() {
        for (unit, delta) in [(MouseWheelUnit::Point, -8.0), (MouseWheelUnit::Line, -0.1)] {
            let mut nav = Navigation::default();
            let start = Instant::now();
            let turns = (0..80)
                .filter_map(|frame| {
                    turn(
                        &mut nav,
                        event(unit, delta, TouchPhase::Move),
                        start + Duration::from_millis(frame * 16),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(turns, [PageDirection::Next]);
            assert_eq!(
                turn(
                    &mut nav,
                    event(unit, -30.5, TouchPhase::Move),
                    start + Duration::from_millis(1600)
                ),
                Some(PageDirection::Next)
            );
        }
    }

    #[test]
    fn explicit_phases_rearm_on_new_start_and_not_on_end() {
        let mut nav = Navigation::default();
        let start = Instant::now();
        assert_eq!(
            turn(
                &mut nav,
                event(MouseWheelUnit::Line, -1.0, TouchPhase::Start),
                start
            ),
            Some(PageDirection::Next)
        );
        for (time, phase) in [
            (150, TouchPhase::Move),
            (220, TouchPhase::End),
            (240, TouchPhase::Move),
        ] {
            assert_eq!(
                turn(
                    &mut nav,
                    event(MouseWheelUnit::Line, -1.0, phase),
                    start + Duration::from_millis(time)
                ),
                None
            );
        }
        assert_eq!(
            turn(
                &mut nav,
                event(MouseWheelUnit::Line, 1.0, TouchPhase::Start),
                start + Duration::from_millis(300)
            ),
            Some(PageDirection::Previous)
        );
    }

    #[test]
    fn mouse_notches_keep_repeated_navigation() {
        let mut nav = Navigation::default();
        let start = Instant::now();
        for time in [0, 150, 300, 450] {
            assert_eq!(
                turn(
                    &mut nav,
                    event(MouseWheelUnit::Line, -1.0, TouchPhase::Move),
                    start + Duration::from_millis(time)
                ),
                Some(PageDirection::Next)
            );
        }
    }

    #[test]
    fn gesture_threshold_and_reverse_jitter_do_not_create_extra_turns() {
        let mut nav = Navigation::default();
        let start = Instant::now();
        for frame in 0..4 {
            assert_eq!(
                turn(
                    &mut nav,
                    event(MouseWheelUnit::Point, -4.0, TouchPhase::Move),
                    start + Duration::from_millis(frame * 20)
                ),
                None
            );
        }
        assert_eq!(
            turn(
                &mut nav,
                event(MouseWheelUnit::Point, -4.0, TouchPhase::Move),
                start + Duration::from_millis(80)
            ),
            Some(PageDirection::Next)
        );
        assert_eq!(
            turn(
                &mut nav,
                event(MouseWheelUnit::Point, 40.0, TouchPhase::Move),
                start + Duration::from_millis(220)
            ),
            None
        );
    }

    #[test]
    fn zoom_and_horizontal_input_do_not_advance_focus() {
        let mut nav = Navigation::default();
        let now = Instant::now();
        let events = [
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -50.0),
                phase: TouchPhase::Move,
                modifiers: Modifiers::CTRL,
            },
            Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: Vec2::new(50.0, 0.0),
                phase: TouchPhase::Move,
                modifiers: Modifiers::NONE,
            },
        ];
        assert_eq!(
            nav.turn(&events, now, 18.0, Duration::from_millis(120)),
            None
        );
        assert_eq!(
            turn(
                &mut nav,
                event(MouseWheelUnit::Line, -1.0, TouchPhase::Move),
                now
            ),
            Some(PageDirection::Next)
        );
    }
}
