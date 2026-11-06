//! Shared release policy for a finger-following single-page slide.
pub fn commit(distance: f32, width: f32, velocity: f32) -> bool {
    if !distance.is_finite() || !width.is_finite() || width <= 0.0 || !velocity.is_finite() {
        return false;
    }
    if velocity.abs() > 350.0 {
        return velocity.signum() == distance.signum();
    }
    distance.abs() > width * 0.25
}

pub fn resisted_offset(distance: f32, width: f32) -> f32 {
    let limit = (width * 0.18).clamp(48.0, 80.0);
    distance.signum() * (limit + (distance.abs() - limit).max(0.0) * 0.18)
        .min(distance.abs()).min(width * 0.28)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_can_commit_cancel_or_reverse_in_both_directions() {
        for sign in [-1.0, 1.0] {
            assert!(!commit(sign * 40.0, 360.0, 0.0));
            assert!(commit(sign * 100.0, 360.0, 0.0));
            assert!(commit(sign * 40.0, 360.0, sign * 400.0));
            assert!(!commit(sign * 100.0, 360.0, -sign * 400.0));
            assert!(resisted_offset(sign * 300.0, 360.0).abs() <= 360.0 * 0.28);
        }
        assert!(!commit(f32::NAN, 360.0, 0.0));
        assert!(!commit(100.0, 0.0, 0.0));
    }
}
