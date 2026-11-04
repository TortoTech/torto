//! Platform-independent timing and stops for focused reading.
use std::time::Duration;

pub const MIN_DURATION: Duration = Duration::from_millis(160);
pub const MAX_DURATION: Duration = Duration::from_millis(360);

pub fn duration(distance: f32) -> Duration {
    let seconds = (distance.abs() / 1000.0).clamp(0.16, 0.36);
    if seconds.is_finite() {
        Duration::from_secs_f32(seconds).clamp(MIN_DURATION, MAX_DURATION)
    } else {
        MIN_DURATION
    }
}

/// Divide oversized content into balanced, reversible stops, including its ends.
pub fn overflow_target(
    current: f32,
    top: f32,
    bottom: f32,
    step: f32,
    forward: bool,
) -> Option<f32> {
    let distance = (bottom - top).max(0.0);
    let count = (distance / step.max(1.0)).ceil().max(1.0);
    let stride = distance / count;
    let anchor = if stride <= 0.001 {
        0.0
    } else {
        ((current - top) / stride).round().clamp(0.0, count)
    };
    if forward && current < bottom - 1.0 {
        let next = (anchor + 1.0).min(count);
        Some(if next == count {
            bottom
        } else {
            (top + next * stride).min(bottom)
        })
    } else if !forward && current > top + 1.0 {
        let next = (anchor - 1.0).max(0.0);
        Some(if next == 0.0 {
            top
        } else {
            (top + next * stride).max(top)
        })
    } else {
        None
    }
}

/// Keep intermediate stops on text-line starts; preserve the block's exact ends.
pub fn line_aligned_overflow_target(
    current: f32, top: f32, bottom: f32, step: f32, forward: bool, line_starts: &[f32],
) -> Option<f32> {
    let target = overflow_target(current, top, bottom, step, forward)?;
    if (target-top).abs()<0.01 || (target-bottom).abs()<0.01 { return Some(target); }
    Some(line_starts.iter().copied()
        .filter(|p| p.is_finite() && *p>=top && *p<=bottom && if forward { *p>current+1.0 } else { *p<current-1.0 })
        .min_by(|a,b| (a-target).abs().total_cmp(&(b-target).abs()))
        .unwrap_or(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_stops_are_balanced_and_reversible() {
        let middle = overflow_target(0.0, 0.0, 900.0, 600.0, true).unwrap();
        assert_eq!(middle, 450.0);
        assert_eq!(
            overflow_target(middle, 0.0, 900.0, 600.0, true),
            Some(900.0)
        );
        assert_eq!(
            overflow_target(900.0, 0.0, 900.0, 600.0, false),
            Some(middle)
        );
        assert_eq!(overflow_target(middle, 0.0, 900.0, 600.0, false), Some(0.0));
        assert_eq!(overflow_target(900.0, 0.0, 900.0, 600.0, true), None);
        assert_eq!(duration(20.0), MIN_DURATION);
        assert_eq!(duration(2000.0), MAX_DURATION);
    }
    #[test]
    fn text_stops_follow_lines_and_reverse_without_losing_the_ends() {
        let lines = (0..40).map(|i| i as f32 * 27.0).collect::<Vec<_>>();
        let middle = line_aligned_overflow_target(0.0, 0.0, 900.0, 600.0, true, &lines).unwrap();
        assert!(lines.contains(&middle));
        assert_eq!(line_aligned_overflow_target(middle,0.0,900.0,600.0,true,&lines),Some(900.0));
        assert_eq!(line_aligned_overflow_target(900.0,0.0,900.0,600.0,false,&lines),Some(middle));
        assert_eq!(line_aligned_overflow_target(middle,0.0,900.0,600.0,false,&lines),Some(0.0));
    }
}
