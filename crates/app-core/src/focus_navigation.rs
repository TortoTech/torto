//! Platform-independent timing and stops for focused reading.
use std::time::Duration;

/// A semantic unit can span spine files or share one with other units. Layout
/// pages are deliberately excluded: typography changes must not lose history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitKey {
    sections: std::ops::Range<usize>,
    index: usize,
}
impl UnitKey {
    pub fn capture(reader: &mut rebook_reader::ReaderSession) -> Self {
        Self {
            sections: reader.current_reading_unit_sections(),
            index: reader.reading_unit_location().index,
        }
    }
}

/// Per-open-book memory only. Peeking/preloading must only read this history.
/// Retain the most recently left units without keeping their rendered pages.
pub struct UnitHistory<T>(std::collections::VecDeque<(UnitKey, T)>);
impl<T> Default for UnitHistory<T> {
    fn default() -> Self { Self(std::collections::VecDeque::new()) }
}
impl<T> UnitHistory<T> {
    pub fn remember(&mut self, key: UnitKey, value: T) {
        if let Some(index) = self.0.iter().position(|(k, _)| *k == key) {
            self.0.remove(index);
        }
        self.0.push_back((key, value));
        if self.0.len() > 128 { self.0.pop_front(); }
    }
    pub fn get(&self, key: &UnitKey) -> Option<&T> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, value)| value)
    }
}

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
    fn unit_history_is_bounded_replaces_old_positions_and_distinguishes_units() {
        let mut history = UnitHistory::default();
        for index in 0..129 {
            history.remember(UnitKey { sections: 0..2, index }, index);
        }
        assert!(history.get(&UnitKey { sections: 0..2, index: 0 }).is_none());
        let key = UnitKey { sections: 0..2, index: 1 };
        history.remember(key.clone(), 500);
        history.remember(UnitKey { sections: 2..3, index: 1 }, 900);
        assert_eq!(history.get(&key), Some(&500));
        assert_eq!(history.get(&UnitKey { sections: 2..3, index: 1 }), Some(&900));
        assert_eq!(history.0.len(), 128);
    }
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
