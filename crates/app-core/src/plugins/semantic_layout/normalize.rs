use super::*;

// Strip retired heading objects before typed deserialization as even malformed
// legacy fields must not discard independent quotes, captions or formulas.
pub(super) fn decode_response(value: &Value) -> Result<Response, String> {
    let mut value = wire::decode(value);
    if let Some(groups) = value.get_mut("groups").and_then(Value::as_array_mut) {
        groups.retain(|group| group.get("kind").and_then(Value::as_str) != Some("section_heading"));
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

// Remove retired headings and proposals that provably do not change existing semantics.
// All remaining IDs, associations and conflicts still go through validation.
pub(super) fn response(
    response: &mut Response,
    section: &Section,
    context: std::ops::Range<usize>,
) -> usize {
    let before = response.groups.len() + response.citations.len() + response.formulas.len();
    let mut seen = HashSet::new();
    response.groups.retain(|group| {
        if matches!(group, Proposal::SectionHeading { .. }) { return false; }
        if !seen.insert(serde_json::to_string(group).expect("serializable proposal")) {
            return false;
        }
        let ids = proposal_ids(group);
        if !ids.iter().all(|id| context.contains(id) && *id < section.blocks.len()) {
            return true;
        }
        let unchanged = match group {
            Proposal::SectionHeading { .. } => unreachable!(),
            Proposal::Figure { images, captions } => {
                consecutive(images) && consecutive(captions)
                    && images.iter().all(|id| matches!(&section.blocks[*id], Block::Image(_)))
                    && captions.iter().all(|id| matches!(&section.blocks[*id], Block::Text(t) if t.kind == TextBlockKind::Caption))
                    && (images.last().and_then(|id| id.checked_add(1)) == captions.first().copied()
                        || captions.last().and_then(|id| id.checked_add(1)) == images.first().copied())
            }
            _ => false,
        };
        !unchanged
    });
    let mut seen = HashSet::new();
    response.citations.retain(|id| seen.insert(id.clone()));
    let mut seen = HashSet::new();
    response.formulas.retain(|formula| {
        seen.insert(serde_json::to_string(formula).expect("serializable formula"))
    });
    before - response.groups.len() - response.citations.len() - response.formulas.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::semantic_layout::tests::{image, section, text};

    #[test]
    fn ignores_known_semantics_and_retired_headings_but_keeps_invalid_figures() {
        let mut s = section(vec![text("h", "Heading"), image("i"), text("c", "Caption")]);
        let Block::Text(h) = &mut s.blocks[0] else {
            unreachable!()
        };
        h.kind = TextBlockKind::Heading(2);
        let Block::Text(c) = &mut s.blocks[2] else {
            unreachable!()
        };
        c.kind = TextBlockKind::Caption;
        let mut r: Response = serde_json::from_value(json!({"groups":[
            {"kind":"section_heading","block":0},
            {"kind":"figure","images":[1],"captions":[2]},
            {"kind":"section_heading","block":2},
            {"kind":"section_heading","block":99},
            {"kind":"figure","images":[99],"captions":[2]}
        ],"citations":[],"formulas":[]}))
        .unwrap();
        assert_eq!(response(&mut r, &s, 0..3), 4);
        assert_eq!(r.groups.len(), 1);
        assert!(validate_window(&r.groups, &s, &RecognitionRoles::default(), 0..3, 0..3).is_err());
        // Retired heading proposals are ignored regardless of their context or ID.
        r.groups = vec![Proposal::SectionHeading { block: 0 }];
        assert_eq!(response(&mut r, &s, 1..3), 1);
    }
}
