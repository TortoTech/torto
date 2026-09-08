use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rebook_engine::{NavigationToken, PageDirection, PreparedNavigation};

/// Supported transition types in the demo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransitionKind {
    None,
    #[default]
    Slide,
}

impl TransitionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Slide => "Slide",
        }
    }
}

/// Transition configuration policy.
#[derive(Debug, Clone, Copy)]
pub struct TransitionPolicy {
    /// Default keyboard animation duration.
    pub default_duration: Duration,
    /// Slop threshold in logical pixels before claiming horizontal drag.
    pub slop_threshold: f32,
    /// Distance threshold for drag completion (fraction of spread width).
    pub drag_distance_threshold_ratio: f32,
    /// Velocity threshold for fling gesture (logical pixels / sec).
    pub fling_velocity_threshold: f32,
    /// Minimum duration for drag settle animation.
    pub min_settle_duration: Duration,
    /// Maximum duration for drag settle animation.
    pub max_settle_duration: Duration,
}

impl Default for TransitionPolicy {
    fn default() -> Self {
        Self {
            default_duration: Duration::from_millis(200),
            slop_threshold: 8.0,
            drag_distance_threshold_ratio: 0.28,
            fling_velocity_threshold: 700.0,
            min_settle_duration: Duration::from_millis(120),
            max_settle_duration: Duration::from_millis(280),
        }
    }
}

impl TransitionPolicy {
    /// Computes duration to settle from `progress` to `target` (0.0 or 1.0).
    #[must_use]
    pub fn settle_duration(self, progress: f32, target: f32) -> Duration {
        let remaining = (target - progress).abs();
        let calculated = self.default_duration.mul_f32(remaining);
        calculated.clamp(self.min_settle_duration, self.max_settle_duration)
    }
}

/// A timed velocity sample for gesture tracking.
#[derive(Debug, Clone, Copy)]
pub struct VelocitySample {
    pub time: Instant,
    pub x: f32,
}

/// Pointer drag tracking state.
#[derive(Debug, Clone)]
pub struct DragGesture {
    pub pointer_id: u64,
    pub start_x: f32,
    pub current_x: f32,
    pub started_at: Instant,
    pub samples: VecDeque<VelocitySample>,
}

impl DragGesture {
    pub fn new(pointer_id: u64, start_x: f32) -> Self {
        let now = Instant::now();
        let mut samples = VecDeque::with_capacity(16);
        samples.push_back(VelocitySample {
            time: now,
            x: start_x,
        });
        Self {
            pointer_id,
            start_x,
            current_x: start_x,
            started_at: now,
            samples,
        }
    }

    pub fn record_move(&mut self, x: f32) {
        self.current_x = x;
        let now = Instant::now();
        // Retain recent samples within 150ms window
        while let Some(front) = self.samples.front() {
            if now.duration_since(front.time) > Duration::from_millis(150) && self.samples.len() > 2
            {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        self.samples.push_back(VelocitySample { time: now, x });
    }

    pub fn horizontal_delta(&self) -> f32 {
        self.current_x - self.start_x
    }

    /// Computes velocity in pixels per second.
    pub fn compute_velocity_x(&self) -> f32 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let dt = last.time.duration_since(first.time).as_secs_f32();
        if dt <= 0.0001 {
            0.0
        } else {
            (last.x - first.x) / dt
        }
    }
}

/// State machine governing page transition animations.
pub enum TransitionState {
    Idle,
    TrackingSlop {
        gesture: DragGesture,
    },
    Preparing {
        direction: PageDirection,
        token: NavigationToken,
        requested_at: Instant,
    },
    Interactive {
        prepared: PreparedNavigation,
        progress: f32,
        velocity: f32,
        gesture: DragGesture,
    },
    Settling {
        prepared: PreparedNavigation,
        from: f32,
        to: f32,
        started_at: Instant,
        duration: Duration,
    },
}

impl TransitionState {
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub const fn is_active(&self) -> bool {
        !self.is_idle()
    }
}

/// Standard ease-out cubic curve: 1 - (1 - t)^3.
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let clamped = t.clamp(0.0, 1.0);
    let inv = 1.0 - clamped;
    1.0 - inv * inv * inv
}

/// Evaluates progress for settling animation given elapsed time.
#[inline]
pub fn evaluate_settle_progress(
    from: f32,
    to: f32,
    elapsed: Duration,
    duration: Duration,
) -> (f32, bool) {
    if duration.is_zero() {
        return (to, true);
    }
    let t = elapsed.as_secs_f32() / duration.as_secs_f32();
    if t >= 1.0 {
        (to, true)
    } else {
        let curve_t = ease_out_cubic(t);
        let progress = from + (to - from) * curve_t;
        (progress, false)
    }
}

/// Computes x translation offsets for source and destination spread surfaces.
///
/// Returns `(source_x, destination_x)`.
#[inline]
pub fn slide_transforms(
    direction: PageDirection,
    progress: f32,
    viewport_width: f32,
) -> (f32, f32) {
    let direction_sign = match direction {
        PageDirection::Next => 1.0,
        PageDirection::Previous => -1.0,
    };
    let source_x = -direction_sign * progress * viewport_width;
    let destination_x = direction_sign * (1.0 - progress) * viewport_width;
    (source_x, destination_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ease_out_cubic() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out_cubic(0.5) > 0.5); // Ease-out is faster initially
    }

    #[test]
    fn test_slide_transforms_next() {
        let (src, dst) = slide_transforms(PageDirection::Next, 0.0, 800.0);
        assert_eq!(src, 0.0);
        assert_eq!(dst, 800.0);

        let (src, dst) = slide_transforms(PageDirection::Next, 0.5, 800.0);
        assert_eq!(src, -400.0);
        assert_eq!(dst, 400.0);

        let (src, dst) = slide_transforms(PageDirection::Next, 1.0, 800.0);
        assert_eq!(src, -800.0);
        assert_eq!(dst, 0.0);
    }

    #[test]
    fn test_slide_transforms_prev() {
        let (src, dst) = slide_transforms(PageDirection::Previous, 0.0, 800.0);
        assert_eq!(src, 0.0);
        assert_eq!(dst, -800.0);

        let (src, dst) = slide_transforms(PageDirection::Previous, 0.5, 800.0);
        assert_eq!(src, 400.0);
        assert_eq!(dst, -400.0);

        let (src, dst) = slide_transforms(PageDirection::Previous, 1.0, 800.0);
        assert_eq!(src, 800.0);
        assert_eq!(dst, 0.0);
    }

    #[test]
    fn test_evaluate_settle() {
        let (p, done) = evaluate_settle_progress(
            0.0,
            1.0,
            Duration::from_millis(100),
            Duration::from_millis(200),
        );
        assert!(!done);
        assert!(p > 0.5);

        let (p, done) = evaluate_settle_progress(
            0.0,
            1.0,
            Duration::from_millis(250),
            Duration::from_millis(200),
        );
        assert!(done);
        assert_eq!(p, 1.0);
    }

    #[test]
    fn test_transition_policy_settle_duration() {
        let policy = TransitionPolicy::default();
        // At progress 0.0 towards 1.0 (remaining 1.0) -> clamped to default (200ms)
        let dur = policy.settle_duration(0.0, 1.0);
        assert_eq!(dur, Duration::from_millis(200));

        // At progress 0.9 towards 1.0 (remaining 0.1) -> 20ms clamped to min (120ms)
        let dur = policy.settle_duration(0.9, 1.0);
        assert_eq!(dur, Duration::from_millis(120));

        // At progress 0.0 towards 0.0 (remaining 0.0) -> clamped to min (120ms)
        let dur = policy.settle_duration(0.0, 0.0);
        assert_eq!(dur, Duration::from_millis(120));
    }

    #[test]
    fn test_drag_gesture_tracking() {
        let mut gesture = DragGesture::new(1, 100.0);
        assert_eq!(gesture.horizontal_delta(), 0.0);
        assert_eq!(gesture.compute_velocity_x(), 0.0);

        gesture.record_move(150.0);
        assert_eq!(gesture.horizontal_delta(), 50.0);
    }
}
