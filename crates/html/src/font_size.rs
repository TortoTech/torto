//! CSS font sizes resolved into the reading IR's default-font-relative scale.

#[derive(Clone, Copy, Debug)]
pub(super) enum FontSize {
    Absolute(f32),
    Parent(f32),
    Root(f32),
}

#[cfg(test)]
mod tests;

impl FontSize {
    pub(super) fn parse(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        let keyword = match value.as_str() {
            "xx-small" => Self::Absolute(0.6),
            "x-small" => Self::Absolute(0.75),
            "small" => Self::Absolute(8.0 / 9.0),
            "medium" | "initial" => Self::Absolute(1.0),
            "large" => Self::Absolute(1.2),
            "x-large" => Self::Absolute(1.5),
            "xx-large" => Self::Absolute(2.0),
            "xxx-large" => Self::Absolute(3.0),
            "smaller" => Self::Parent(1.0 / 1.2),
            "larger" => Self::Parent(1.2),
            "inherit" | "unset" => Self::Parent(1.0),
            _ => return Self::parse_length(&value),
        };
        Some(keyword)
    }

    fn parse_length(value: &str) -> Option<Self> {
        // Absolute CSS lengths use a 16px initial size, then scale with the
        // reader's chosen font size along with the rest of the book.
        let (number, unit) = ["rem", "em", "px", "pt", "%"]
            .into_iter()
            .find_map(|unit| value.strip_suffix(unit).map(|number| (number, unit)))
            .unwrap_or((value, ""));
        let number: f32 = number.parse().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        Some(match unit {
            "rem" => Self::Root(number),
            "em" => Self::Parent(number),
            "%" => Self::Parent(number / 100.0),
            "px" => Self::Absolute(number / 16.0),
            "pt" => Self::Absolute(number / 12.0),
            "" if number == 0.0 => Self::Absolute(0.0),
            _ => return None,
        })
    }

    pub(super) fn resolve(self, parent: f32, root: f32) -> Option<f32> {
        let size = match self {
            Self::Absolute(size) => size,
            Self::Parent(factor) => parent * factor,
            Self::Root(factor) => root * factor,
        };
        size.is_finite().then_some(size)
    }
}
