//! Recognize deliberately clipped/offscreen accessible alternatives. Absolute
//! positioning by itself is meaningful authored content and is never hidden.
use std::collections::HashMap;

pub(super) fn hidden(properties: &HashMap<String, String>) -> bool {
    let get = |key: &str| properties.get(key).map(|s| s.trim()).unwrap_or("");
    if !matches!(get("position"), "absolute" | "fixed") {
        return false;
    }
    let length = |key| super::css_length(get(key)).filter(|v| v.is_finite());
    // Use deliberately remote negative offsets, not small optical adjustments
    // or centering transforms. Require the *whole* explicit box to be outside.
    let remote = [("left", "width"), ("top", "height")]
        .iter()
        .any(|(edge, size)| {
            length(edge).is_some_and(|offset| {
                offset <= -1000.0
                    && length(size).is_some_and(|size| size >= 0.0 && offset + size < 0.0)
            })
        });
    let tiny = length("width").is_some_and(|v| (0.0..=1.0).contains(&v))
        && length("height").is_some_and(|v| (0.0..=1.0).contains(&v));
    let clip = get("clip").replace([' ', '\t', '\n', ','], "");
    let clip_path = get("clip-path").replace(' ', "");
    let clipped = matches!(clip.as_str(), "rect(0000)" | "rect(0px0px0px0px)")
        || clip_path == "inset(50%)"
        || clip_path == "inset(100%)";
    let clips_overflow = matches!(get("overflow"), "hidden" | "clip");
    let transformed = !get("transform").is_empty() && get("transform") != "none";
    // Do not infer invisibility when overflowing text or a transform can bring
    // the content back into view. A zero-area clip remains hidden regardless.
    (remote && clips_overflow && !transformed) || (tiny && clipped)
}
