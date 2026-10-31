//! Decimal precision for descriptive LLM input, never rendering or locators.

pub(super) fn round_request_number(value: f64) -> f64 {
    let scaled = value * 10_000.0;
    if !scaled.is_finite() {
        return value;
    }
    let rounded = scaled.round() / 10_000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

pub(super) fn size_attribute(value: f32) -> String {
    let rounded = round_request_number(f64::from(value));
    // A positive authored scale must not become an invalid zero-sized tag.
    if value > 0.0 && rounded == 0.0 {
        value.to_string()
    } else {
        rounded.to_string()
    }
}
