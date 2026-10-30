//! Tolerant MOBI attributes, scanned without changing original byte offsets.
use std::ops::Range;

struct Attribute {
    range: Range<usize>,
    value_range: Range<usize>,
    value: usize,
}

fn scan(data: &[u8], name: &[u8]) -> Vec<Attribute> {
    let mut result = Vec::new();
    let mut position = 0;
    while let Some(relative) = data[position..].iter().position(|byte| *byte == b'<') {
        position += relative + 1;
        if data[position..].starts_with(b"!--") {
            let Some(end) = data[position + 3..]
                .windows(3)
                .position(|bytes| bytes == b"-->")
            else {
                break;
            };
            position += 3 + end + 3;
            continue;
        }
        // Consume the tag name. Attribute values are consumed below as whole
        // tokens, so an apparent attribute inside another quoted value is ignored.
        while data
            .get(position)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'>')
        {
            position += 1;
        }
        loop {
            while data.get(position).is_some_and(u8::is_ascii_whitespace) {
                position += 1;
            }
            if position == data.len() {
                break;
            }
            if data[position] == b'>' {
                position += 1;
                break;
            }
            if data[position] == b'/' {
                position += 1;
                continue;
            }
            let start = position;
            while data
                .get(position)
                .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
            {
                position += 1;
            }
            let attribute_name = &data[start..position];
            while data.get(position).is_some_and(u8::is_ascii_whitespace) {
                position += 1;
            }
            if data.get(position) != Some(&b'=') {
                continue;
            }
            position += 1;
            while data.get(position).is_some_and(u8::is_ascii_whitespace) {
                position += 1;
            }
            let quote = data
                .get(position)
                .copied()
                .filter(|b| matches!(b, b'\'' | b'"'));
            position += usize::from(quote.is_some());
            let value_start = position;
            while data.get(position).is_some_and(|b| match quote {
                Some(quote) => *b != quote,
                None => !b.is_ascii_whitespace() && !matches!(b, b'>' | b'/'),
            }) {
                position += 1;
            }
            let value_end = position;
            let closed = quote.is_none_or(|quote| data.get(position) == Some(&quote));
            if quote.is_some() && closed {
                position += 1;
            }
            let digits = &data[value_start..value_end];
            if attribute_name.eq_ignore_ascii_case(name)
                && closed
                && !digits.is_empty()
                && digits.iter().all(u8::is_ascii_digit)
                && let Some(value) = digits.iter().try_fold(0usize, |value, byte| {
                    value.checked_mul(10)?.checked_add(usize::from(byte - b'0'))
                })
            {
                result.push(Attribute {
                    range: start..position,
                    value_range: value_start..value_end,
                    value,
                });
            }
        }
    }
    result
}

pub(crate) fn values(data: &[u8], name: &[u8]) -> Vec<usize> {
    let mut result: Vec<_> = scan(data, name)
        .into_iter()
        .map(|attribute| attribute.value)
        .collect();
    result.sort_unstable();
    result.dedup();
    result
}

pub(crate) fn rewrite(
    source: &str,
    name: &str,
    mut replacement: impl FnMut(usize) -> Option<String>,
) -> String {
    let mut output = String::with_capacity(source.len());
    let mut copied = 0;
    for attribute in scan(source.as_bytes(), name.as_bytes()) {
        output.push_str(&source[copied..attribute.range.start]);
        output.push_str(
            &replacement(attribute.value)
                .unwrap_or_else(|| format!("{name}=\"{}\"", &source[attribute.value_range])),
        );
        copied = attribute.range.end;
    }
    output.push_str(&source[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_rewrites_the_same_attributes_without_changing_other_bytes() {
        let source = "前言 filepos=999 <!-- <a filepos=888> --><a title=\"filepos=777 >\" FILEPOS = '00123'>段落</a><a filepos=45/><a filepos=123>";
        assert_eq!(values(source.as_bytes(), b"filepos"), [45, 123]);
        let rewritten = rewrite(source, "filepos", |value| {
            Some(format!("href=\"pos-{value}\""))
        });
        assert_eq!(
            rewritten,
            "前言 filepos=999 <!-- <a filepos=888> --><a title=\"filepos=777 >\" href=\"pos-123\">段落</a><a href=\"pos-45\"/><a href=\"pos-123\">"
        );
    }

    #[test]
    fn rejects_partial_values_overflow_and_different_attribute_names() {
        let source = b"<a data-filepos=1 _filepos=2 filepos=12abc><a filepos='3x'><a filepos=999999999999999999999999999999999999><a filepos=''>";
        assert!(values(source, b"filepos").is_empty());
        assert!(values(b"<a filepos='12", b"filepos").is_empty());
        assert_eq!(
            rewrite("<img RECINDEX = '0002'>", "recindex", |_| None),
            "<img recindex=\"0002\">"
        );
    }
}
