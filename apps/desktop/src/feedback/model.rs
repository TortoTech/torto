use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Payload {
    pub request_id: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<Screenshot>,
    pub context: Context,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Screenshot {
    pub data: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    #[default]
    Bug,
    Feature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Context {
    pub source: String,
    pub app: App,
    pub os: Os,
    pub display: Display,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book: Option<Book>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reader: Option<Reader>,
}

#[derive(Clone)]
pub(crate) struct OpenContext {
    pub context: Context,
    // Used only by the preparation worker to determine PDF text source. Never uploaded.
    pub pdf_id: Option<String>,
}

impl OpenContext {
    pub fn shelf(ctx: &egui::Context) -> Self {
        let rect = ctx.content_rect();
        let scale = ctx.pixels_per_point();
        Self {
            context: Context {
                source: "shelf".into(),
                app: App {
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                os: Os {
                    name: std::env::consts::OS.into(),
                    version: "unknown".into(),
                    arch: std::env::consts::ARCH.into(),
                },
                display: Display {
                    width: (rect.width() * scale).clamp(1.0, 100_000.0) as u32,
                    height: (rect.height() * scale).clamp(1.0, 100_000.0) as u32,
                    scale: scale.clamp(0.1, 20.0),
                },
                book: None,
                location: None,
                reader: None,
            },
            pdf_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct App {
    pub version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Os {
    pub name: String,
    pub version: String,
    pub arch: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Display {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Book {
    pub title: String,
    pub authors: Vec<String>,
    pub languages: Vec<String>,
    pub format: String,
    pub layout: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Location {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<FocusLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FocusLocation {
    pub range: FocusRange,
    pub ranges: Vec<FocusRange>,
    pub total_ranges: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FocusRange {
    pub start: FocusAnchor,
    pub end: FocusAnchor,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FocusAnchor {
    pub spine: String,
    pub node: String,
    pub text_offset: u64,
}

impl FocusRange {
    fn from_source(source: &rebook_publication::SourceRange) -> Option<Self> {
        let anchor = |value: &rebook_publication::SourceAnchor| {
            let valid_id = |id: &str| !id.trim().is_empty() && id.encode_utf16().count() <= 500;
            (valid_id(value.spine.as_str())
                && valid_id(&value.node)
                && value.text_offset <= 9_007_199_254_740_991)
                .then(|| FocusAnchor {
                    spine: value.spine.as_str().into(),
                    node: value.node.clone(),
                    text_offset: value.text_offset,
                })
        };
        if source.start.spine != source.end.spine
            || source.start.node == source.end.node
                && source.start.text_offset > source.end.text_offset
        {
            return None;
        }
        Some(Self {
            start: anchor(&source.start)?,
            end: anchor(&source.end)?,
        })
    }

    pub fn label(&self) -> String {
        format!(
            "{} / {} : {} → {} / {} : {}",
            self.start.spine,
            self.start.node,
            self.start.text_offset,
            self.end.spine,
            self.end.node,
            self.end.text_offset
        )
    }
}

impl FocusLocation {
    pub fn from_source(
        range: &rebook_publication::SourceRange,
        members: &[rebook_publication::SourceRange],
    ) -> Option<Self> {
        let mut value = Self {
            range: FocusRange::from_source(range)?,
            ranges: Vec::new(),
            total_ranges: members.len().max(1),
        };
        let fallback = [range.clone()];
        let members = if members.is_empty() {
            &fallback[..]
        } else {
            members
        };
        for member in members.iter().take(64) {
            if member.start.spine != range.start.spine {
                return None;
            }
            value.ranges.push(FocusRange::from_source(member)?);
            // Match the service's escaped JSON byte budget without shortening identifiers.
            let json = serde_json::to_string(&value)
                .ok()?
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('@', "@\u{200b}");
            if json.len() > 16_000 {
                value.ranges.pop();
                break;
            }
        }
        (!value.ranges.is_empty()).then_some(value)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Reader {
    pub mode: String,
    pub typesetting: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_reflow: Option<String>,
    pub translation: Translation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Translation {
    pub enabled: bool,
    pub target_language: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub version: u8,
    pub endpoint: String,
    pub payload: Payload,
    pub retry_until: Option<u64>,
}

pub(super) fn character_count(value: &str) -> usize {
    static VISIBLE: OnceLock<regex::Regex> = OnceLock::new();
    let visible = VISIBLE.get_or_init(|| {
        regex::Regex::new(r"[^\s\p{Default_Ignorable_Code_Point}\p{Mark}\p{Control}]")
            .expect("feedback character regex")
    });
    value
        .graphemes(true)
        .filter(|part| visible.is_match(part))
        .count()
}

pub(super) fn valid_description(value: &str) -> bool {
    (10..=5000).contains(&character_count(value)) && value.encode_utf16().count() <= 10_000
}

pub(crate) fn clipped(value: &str, max: usize) -> String {
    let mut used = 0;
    value
        .trim()
        .chars()
        .take_while(|c| {
            used += c.len_utf16();
            used <= max
        })
        .collect()
}

pub(crate) fn book_document(path: &str) -> Option<String> {
    (!path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.contains(':')
        && !path.split(['/', '\\']).any(|part| part == ".."))
    .then(|| clipped(path, 1000))
}
