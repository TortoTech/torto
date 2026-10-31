use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn symbol_text_round_trips_without_becoming_protocol_markup() {
        for symbol in ["<", ">", "&lt;", "⺅㇏", "a\u{301}", "<t-img-9/>"] {
            let encoded = encode_text(symbol);
            assert!(!encoded.contains(['<', '>']));
            assert_eq!(decode_text(&encoded), symbol);
            let tagged = format!("<t-img-0 keep=\"1\">{encoded}</t-img-0>");
            let token = at(&tagged, 0).unwrap();
            assert!(valid_pair(&token, &token));
        }
    }
}

pub(super) struct Token<'a> {
    pub id: usize,
    pub range: std::ops::Range<usize>,
    pub text: Option<&'a str>,
    pub literal: bool,
}

pub(super) fn at(text: &str, start: usize) -> Result<Token<'_>, ()> {
    let tail = text
        .get(start..)
        .ok_or(())?
        .strip_prefix(IMAGE_PLACEHOLDER_PREFIX)
        .ok_or(())?;
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(());
    }
    let id = tail[..digits].parse().map_err(|_| ())?;
    let rest = &tail[digits..];
    let header = start + IMAGE_PLACEHOLDER_PREFIX.len() + digits;
    if rest.starts_with("/>") {
        return Ok(Token {
            id,
            range: start..header + 2,
            text: None,
            literal: false,
        });
    }
    let (literal, length) = if rest.starts_with('>') {
        (false, 1)
    } else if rest.starts_with(" keep=\"1\">") {
        (true, 10)
    } else {
        return Err(());
    };
    let body = header + length;
    let closing = format!("</t-img-{id}>");
    let end = text[body..].find(&closing).ok_or(())? + body;
    let value = &text[body..end];
    if value.contains('<') || value.contains('>') {
        return Err(());
    }
    Ok(Token {
        id,
        range: start..end + closing.len(),
        text: Some(value),
        literal,
    })
}

pub(super) fn tokens(text: &str) -> Result<Vec<Token<'_>>, ()> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(start) = text[cursor..].find(IMAGE_PLACEHOLDER_PREFIX) {
        let token = at(text, cursor + start)?;
        cursor = token.range.end;
        result.push(token);
    }
    Ok(result)
}

pub(super) fn push(output: &mut String, index: usize, run: &rebook_publication::InlineImageRun) {
    if let Some(text) = &run.image.ocr_text {
        output.push_str(&format!(
            "<t-img-{index}{}>{}</t-img-{index}>",
            if text.literal { " keep=\"1\"" } else { "" },
            encode_text(&text.text)
        ));
    } else {
        output.push_str(&format!("<t-img-{index}/>"));
    }
}

fn encode_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(super) fn decode_text(text: &str) -> String {
    // Decode exactly one layer; a literal "&lt;" must remain that text.
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub(super) fn valid_pair(expected: &Token<'_>, actual: &Token<'_>) -> bool {
    if expected.id != actual.id
        || expected.literal != actual.literal
        || expected.text.is_some() != actual.text.is_some()
    {
        return false;
    }
    match (expected.text, actual.text) {
        (Some(source), Some(translated)) => {
            (!expected.literal || source == translated) && valid_text(translated)
        }
        _ => true,
    }
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= 4096
        && !text.chars().any(|c| c.is_control() && c != '\n')
        && !text.contains(['<', '>'])
}

pub(super) fn normalize_spacing(text: &str) -> String {
    let normalize = |value: &str| {
        value
            .split('\n')
            .map(super::normalize_translation_line_spacing)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let Ok(tokens) = tokens(text) else {
        return normalize(text);
    };
    let mut prefix = String::from("<t-protected-img-");
    while text.contains(&prefix) {
        prefix.push('-');
    }
    let mut protected = text.to_owned();
    let mut saved = Vec::new();
    for (index, token) in tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.text.is_some())
        .rev()
    {
        let marker = format!("{prefix}{index}/>");
        saved.push((marker.clone(), text[token.range.clone()].to_owned()));
        protected.replace_range(token.range.clone(), &marker);
    }
    let mut output = normalize(&protected);
    for (marker, original) in saved {
        output = output.replace(&marker, &original);
    }
    output
}
