use serde::de::DeserializeOwned;

/// Repairs JSON emitted by an LLM before applying the normal typed validation.
pub(super) fn parse<T>(input: &str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let candidate = json_candidate(input);
    let repaired = jsonrepair_rs::jsonrepair(candidate)
        .map_err(|error| format!("JSON repair failed: {error}"))?;
    serde_json::from_str(&repaired)
        .map_err(|error| format!("repaired JSON failed validation: {error}"))
}

fn json_candidate(input: &str) -> &str {
    let trimmed = input.trim();
    trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .or_else(|| {
            let object_start = trimmed.find('{');
            let object_end = trimmed.rfind('}');
            let array_start = trimmed.find('[');
            let array_end = trimmed.rfind(']');
            match (object_start, object_end, array_start, array_end) {
                (Some(start), Some(end), _, _) | (_, _, Some(start), Some(end)) if start <= end => {
                    Some(&trimmed[start..=end])
                }
                _ => None,
            }
        })
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::parse;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Example {
        title: String,
        items: Vec<u8>,
    }

    #[test]
    fn valid_json_preserves_its_typed_value() {
        let parsed: Example = parse(r#"{"title":"Chapter","items":[1,2]}"#).unwrap();
        assert_eq!(
            parsed,
            Example {
                title: "Chapter".into(),
                items: vec![1, 2],
            }
        );
    }

    #[test]
    fn repairs_common_llm_json_errors() {
        let parsed: Example =
            parse("```json\n{'title':'Chapter \\(one\\)','items':[1,2,],}\n```").unwrap();
        assert_eq!(parsed.title, "Chapter (one)");
        assert_eq!(parsed.items, vec![1, 2]);
    }

    #[test]
    fn repaired_json_still_requires_the_typed_schema() {
        let error = parse::<Example>(r"{'title':'Chapter'}").unwrap_err();
        assert!(error.contains("missing field `items`"));
    }
}
