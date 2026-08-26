use serde::de::DeserializeOwned;

/// Repairs JSON emitted by an LLM before applying the normal typed validation.
pub(super) fn parse<T>(input: &str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    rebook_assistant::parse_llm_json(input)
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
