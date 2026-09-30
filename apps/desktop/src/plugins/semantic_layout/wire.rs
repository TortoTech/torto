//! Compact transport only; domain values and persisted caches remain readable.
use serde_json::Value;

const KEYS: &[(&str, &str)] = &[
    ("groups", "g"),
    ("citations", "c"),
    ("formulas", "f"),
    ("kind", "k"),
    ("block", "b"),
    ("paragraph", "p"),
    ("original", "o"),
    ("latex", "l"),
    ("before", "s"),
    ("after", "e"),
    ("blocks", "bs"),
    ("id", "i"),
    ("text", "t"),
    ("type", "ty"),
    ("body", "d"),
    ("attribution", "a"),
    ("alignment", "al"),
    ("images", "im"),
    ("captions", "ca"),
    ("quote", "q"),
    ("body_index", "bi"),
    ("credit", "cr"),
    ("heading", "h"),
    ("alt", "at"),
    ("index", "ix"),
    ("style", "st"),
    ("math_texts", "mt"),
    ("citation_candidates", "cc"),
    ("citation_paragraphs", "cp"),
    ("attribution_eligible", "ae"),
    ("target_start", "ts"),
    ("target_end_exclusive", "te"),
    ("quotes_enabled", "qe"),
    ("captions_enabled", "ce"),
    ("headings_enabled", "he"),
    ("targets", "tg"),
    ("classify_headings", "ch"),
    ("classify_blocks", "cb"),
    ("complete_quote_sources", "cq"),
    ("classify_citations", "ci"),
    ("numbered_candidates", "nc"),
    ("results", "r"),
    ("image_id", "ii"),
    ("status", "ss"),
    ("equation_number", "n"),
    ("requested_ids", "ri"),
    ("metadata", "md"),
    ("next_image", "ni"),
    ("mode", "m"),
    ("retry", "rt"),
    ("inline", "in"),
    ("context", "cx"),
    ("proposal", "pr"),
    ("local_validation_error", "ve"),
    ("bold_ratio", "br"),
    ("italic_ratio", "ir"),
    ("relative_font_size", "fs"),
    ("align", "ag"),
    ("margin_before", "mb"),
    ("margin_after", "ma"),
    ("total", "tt"),
    ("items", "it"),
    ("number", "nu"),
];

const ENUMS: &[(&str, &str)] = &[
    ("quote", "q"),
    ("quote_before", "qb"),
    ("quote_inline", "qi"),
    ("quote_attribution", "qa"),
    ("figure", "fg"),
    ("section_heading", "sh"),
    ("paragraph", "p"),
    ("image", "im"),
    ("boundary", "bd"),
    ("protected_boundary", "pb"),
    ("quote_missing_attribution", "qm"),
    ("attribution_candidate", "ac"),
    ("recognized", "ok"),
    ("not_formula", "no"),
    ("unreadable", "u"),
    ("original", "o"),
    ("transcribe", "tr"),
    ("verify", "v"),
];

fn enum_key(name: &str, decode: bool) -> &str {
    ENUMS
        .iter()
        .find_map(|(long, short)| {
            if decode && name == *short {
                Some(*long)
            } else if !decode && name == *long {
                Some(*short)
            } else {
                None
            }
        })
        .unwrap_or(name)
}

fn enum_field(name: &str) -> bool {
    matches!(name, "kind" | "type" | "status" | "next_image" | "mode")
}

fn key(name: &str, decode: bool) -> &str {
    KEYS.iter()
        .find_map(|(long, short)| {
            if decode && name == *short {
                Some(*long)
            } else if !decode && name == *long {
                Some(*short)
            } else {
                None
            }
        })
        .unwrap_or(name)
}

pub(super) fn encode(value: &Value) -> Value {
    convert(value, false)
}
pub(super) fn decode(value: &Value) -> Value {
    convert(value, true)
}

fn convert(value: &Value, decode: bool) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(name, value)| {
                    let long = if decode { key(name, true) } else { name };
                    let value = if enum_field(long) && value.is_string() {
                        Value::String(enum_key(value.as_str().unwrap(), decode).to_owned())
                    } else {
                        convert(value, decode)
                    };
                    (key(name, decode).to_owned(), value)
                })
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.iter().map(|v| convert(v, decode)).collect()),
        // Never rewrite book text, exact-match contexts or LaTeX.
        _ => value.clone(),
    }
}

pub(super) fn options(value: &Value) -> Value {
    fn schema(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(name, value)| {
                        let value = match name.as_str() {
                            "properties" => Value::Object(
                                value
                                    .as_object()
                                    .unwrap()
                                    .iter()
                                    .map(|(name, value)| {
                                        let mut property = schema(value);
                                        if enum_field(name)
                                            && let Some(values) = property
                                                .get_mut("enum")
                                                .and_then(Value::as_array_mut)
                                        {
                                            for value in values {
                                                if let Some(text) = value.as_str() {
                                                    *value = Value::String(
                                                        enum_key(text, false).to_owned(),
                                                    );
                                                }
                                            }
                                        }
                                        if let Some(object) = property.as_object_mut() {
                                            let description = object
                                                .get("description")
                                                .and_then(Value::as_str)
                                                .unwrap_or("");
                                            object.insert(
                                                "description".into(),
                                                Value::String(format!("{name}. {description}")),
                                            );
                                        }
                                        (key(name, false).to_owned(), property)
                                    })
                                    .collect(),
                            ),
                            "required" => Value::Array(
                                value
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| {
                                        Value::String(key(v.as_str().unwrap(), false).to_owned())
                                    })
                                    .collect(),
                            ),
                            "description" => Value::String(prompt(value.as_str().unwrap())),
                            _ => schema(value),
                        };
                        (name.clone(), value)
                    })
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(schema).collect()),
            _ => value.clone(),
        }
    }
    schema(value)
}

pub(super) fn prompt(text: &str) -> String {
    // Only rewrite code identifiers; natural prose and quoted source examples
    // must keep words like "before", "after" and "paragraph" intact.
    let mut result = String::new();
    let mut word = String::new();
    let mut code = false;
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            if code {
                let mapped = key(&word, false);
                result.push_str(if mapped == word {
                    enum_key(&word, false)
                } else {
                    mapped
                });
            } else {
                result.push_str(&word);
            }
            word.clear();
            if c != '\0' {
                result.push(c);
            }
            if c == '`' {
                code = !code;
            }
        }
    }
    result
}

pub(super) fn instructions(text: &str) -> String {
    let fields = KEYS
        .iter()
        .map(|(long, short)| format!("{short}={long}"))
        .collect::<Vec<_>>()
        .join(", ");
    let enums = ENUMS
        .iter()
        .map(|(long, short)| format!("{short}={long}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}\n\nCompact JSON transport: use the supplied schema and compact keys exclusively. Field meanings: {fields}. Enum meanings (only in k/ty/ss/ni/m): {enums}. All source strings, IDs and LaTeX remain exact.\n",
        prompt(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn transport_round_trip_preserves_text_and_schema_keywords() {
        let unique = KEYS
            .iter()
            .map(|(_, short)| short)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), KEYS.len());
        let value = json!({"groups":[],"formulas":[{"block":1,"paragraph":0,"original":"latex before type","latex":"\\frac{a}{b}","before":"groups","after":""}]});
        assert_eq!(decode(&encode(&value)), value);
        assert_eq!(encode(&value)["f"][0]["o"], "latex before type");
        let schema = options(
            &json!({"output_schema":{"type":"object","properties":{"formulas":{"type":"array","items":{"type":"object","properties":{"latex":{"type":"string"}},"required":["latex"]}}},"required":["formulas"]}}),
        );
        assert_eq!(schema["output_schema"]["type"], "object");
        assert_eq!(
            schema["output_schema"]["properties"]["f"]["items"]["required"],
            json!(["l"])
        );
        assert_eq!(
            prompt("before `before` and `quote_inline` after"),
            "before `s` and `qi` after"
        );
        let value = json!({"groups":[{"kind":"quote_inline","body":[1]}],"blocks":[{"type":"paragraph","text":"quote_inline original"}]});
        assert_eq!(decode(&encode(&value)), value);
        assert_eq!(encode(&value)["g"][0]["k"], "qi");
    }
}
