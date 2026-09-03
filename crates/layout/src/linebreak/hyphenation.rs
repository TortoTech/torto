//! Dictionary-backed discretionary English hyphenation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use hyphenation::{Hyphenator as _, Language, Load as _, Standard};
use rebook_publication::{HyphenationMode, TextLanguage};

const WORD_CACHE_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum EnglishLocale {
    Us,
    Gb,
}

#[derive(Clone, Debug)]
pub(crate) struct HyphenationSpan {
    pub range: Range<usize>,
    pub language: TextLanguage,
    pub mode: HyphenationMode,
    pub suppress: bool,
}

static EN_US: OnceLock<Result<Standard, String>> = OnceLock::new();
static EN_GB: OnceLock<Result<Standard, String>> = OnceLock::new();
type WordKey = (EnglishLocale, String);

#[derive(Default)]
struct WordCache {
    entries: HashMap<WordKey, Vec<usize>>,
    order: VecDeque<WordKey>,
}

static WORD_CACHE: OnceLock<Mutex<WordCache>> = OnceLock::new();

const EN_US_DICTIONARY: &[u8] =
    include_bytes!("../../../../assets/hyphenation/en-us.standard.bincode");
const EN_GB_DICTIONARY: &[u8] =
    include_bytes!("../../../../assets/hyphenation/en-gb.standard.bincode");

pub(crate) fn break_opportunities(
    text: &str,
    spans: &[HyphenationSpan],
    publication_languages: &[String],
    blockers: &[usize],
) -> HashSet<usize> {
    if text.is_empty() || spans.is_empty() {
        return HashSet::new();
    }
    let fallback = publication_locale(publication_languages);
    let paragraph_can_use_fallback = looks_predominantly_english(text);
    let mut breaks = HashSet::new();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !bytes[cursor].is_ascii_alphabetic() {
            cursor += char_len_at(text, cursor);
            continue;
        }
        let start = cursor;
        let mut letters = 0;
        let mut authored = Vec::new();
        while cursor < bytes.len() {
            if bytes[cursor].is_ascii_alphabetic() {
                letters += 1;
                cursor += 1;
                continue;
            }
            if text[cursor..].starts_with('\u{00ad}') {
                cursor += '\u{00ad}'.len_utf8();
                authored.push(cursor);
                continue;
            }
            break;
        }
        let end = cursor;
        if letters < 5
            || looks_like_identifier_boundary(text, start, end)
            || blockers
                .iter()
                .any(|offset| start < *offset && *offset < end)
        {
            continue;
        }
        let Some((locale, mode)) =
            word_policy(start, end, spans, fallback, paragraph_can_use_fallback)
        else {
            continue;
        };
        if !authored.is_empty() {
            if mode != HyphenationMode::None {
                breaks.extend(authored.into_iter().filter(|offset| {
                    visible_letters(text, start, *offset) >= 2
                        && visible_letters(text, *offset, end) >= 3
                }));
            }
            continue;
        }
        if mode != HyphenationMode::Auto {
            continue;
        }
        let word = &text[start..end];
        breaks.extend(
            word_breaks(locale, word)
                .into_iter()
                .filter(|offset| *offset >= 2 && word.len().saturating_sub(*offset) >= 3)
                .map(|offset| start + offset),
        );
    }
    breaks
}

fn word_policy(
    start: usize,
    end: usize,
    spans: &[HyphenationSpan],
    fallback: Option<EnglishLocale>,
    paragraph_can_use_fallback: bool,
) -> Option<(EnglishLocale, HyphenationMode)> {
    let mut covered = start;
    let mut locale = None;
    let mut mode = HyphenationMode::Auto;
    for span in spans
        .iter()
        .filter(|span| span.range.end > start && span.range.start < end)
    {
        if span.range.start > covered || span.suppress {
            return None;
        }
        let span_locale = locale_for_language(span.language, fallback, paragraph_can_use_fallback)?;
        if locale.is_some_and(|current| current != span_locale) {
            return None;
        }
        locale = Some(span_locale);
        mode = match (mode, span.mode) {
            (_, HyphenationMode::None) => HyphenationMode::None,
            (HyphenationMode::Auto, HyphenationMode::Manual) => HyphenationMode::Manual,
            (current, _) => current,
        };
        covered = covered.max(span.range.end.min(end));
        if covered >= end {
            break;
        }
    }
    (covered >= end).then_some((locale?, mode))
}

fn publication_locale(languages: &[String]) -> Option<EnglishLocale> {
    languages
        .iter()
        .find_map(|language| match TextLanguage::from_bcp47(language) {
            TextLanguage::EnglishUs | TextLanguage::English => Some(EnglishLocale::Us),
            TextLanguage::EnglishGb => Some(EnglishLocale::Gb),
            TextLanguage::Unspecified | TextLanguage::Other => None,
        })
}

fn locale_for_language(
    language: TextLanguage,
    fallback: Option<EnglishLocale>,
    paragraph_can_use_fallback: bool,
) -> Option<EnglishLocale> {
    match language {
        TextLanguage::EnglishUs => Some(EnglishLocale::Us),
        TextLanguage::EnglishGb => Some(EnglishLocale::Gb),
        TextLanguage::English => fallback.or(Some(EnglishLocale::Us)),
        TextLanguage::Unspecified if paragraph_can_use_fallback => fallback,
        TextLanguage::Unspecified | TextLanguage::Other => None,
    }
}

fn dictionary(locale: EnglishLocale) -> Option<&'static Standard> {
    let slot = match locale {
        EnglishLocale::Us => &EN_US,
        EnglishLocale::Gb => &EN_GB,
    };
    slot.get_or_init(|| {
        let (language, bytes) = match locale {
            EnglishLocale::Us => (Language::EnglishUS, EN_US_DICTIONARY),
            EnglishLocale::Gb => (Language::EnglishGB, EN_GB_DICTIONARY),
        };
        Standard::from_reader(language, &mut Cursor::new(bytes)).map_err(|error| error.to_string())
    })
    .as_ref()
    .ok()
}

fn word_breaks(locale: EnglishLocale, word: &str) -> Vec<usize> {
    let key = (locale, word.to_ascii_lowercase());
    let cache = WORD_CACHE.get_or_init(|| Mutex::new(WordCache::default()));
    if let Ok(mut cache) = cache.lock() {
        if let Some(cached) = cache.entries.get(&key).cloned() {
            if let Some(index) = cache.order.iter().position(|candidate| candidate == &key) {
                cache.order.remove(index);
            }
            cache.order.push_back(key);
            return cached;
        }
        let breaks = dictionary(locale)
            .map(|dictionary| dictionary.hyphenate(word).breaks)
            .unwrap_or_default();
        if cache.entries.len() >= WORD_CACHE_LIMIT
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, breaks.clone());
        breaks
    } else {
        dictionary(locale)
            .map(|dictionary| dictionary.hyphenate(word).breaks)
            .unwrap_or_default()
    }
}

fn looks_predominantly_english(text: &str) -> bool {
    let mut latin = 0_u32;
    let mut competing = 0_u32;
    for character in text.chars() {
        let value = character as u32;
        if character.is_ascii_alphabetic() {
            latin += 1;
        } else if matches!(
            value,
            0x0370..=0x052f
                | 0x0590..=0x08ff
                | 0x3040..=0x30ff
                | 0x3400..=0x9fff
                | 0xac00..=0xd7af
        ) {
            competing += 1;
        }
    }
    latin >= 5 && latin * 100 >= (latin + competing * 2) * 70
}

fn looks_like_identifier_boundary(text: &str, start: usize, end: usize) -> bool {
    const IDENTIFIER_PUNCTUATION: &[u8] = b"/@_\\";
    start
        .checked_sub(1)
        .and_then(|index| text.as_bytes().get(index))
        .is_some_and(|byte| IDENTIFIER_PUNCTUATION.contains(byte))
        || text
            .as_bytes()
            .get(end)
            .is_some_and(|byte| IDENTIFIER_PUNCTUATION.contains(byte))
}

fn char_len_at(text: &str, offset: usize) -> usize {
    text[offset..].chars().next().map_or(1, char::len_utf8)
}

fn visible_letters(text: &str, start: usize, end: usize) -> usize {
    text[start..end]
        .bytes()
        .filter(u8::is_ascii_alphabetic)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, language: TextLanguage) -> HyphenationSpan {
        HyphenationSpan {
            range: 0..text.len(),
            language,
            mode: HyphenationMode::Auto,
            suppress: false,
        }
    }

    #[test]
    fn finds_dictionary_breaks_without_rewriting_source_text() {
        let text = "hyphenation";
        let breaks = break_opportunities(
            text,
            &[span(text, TextLanguage::EnglishUs)],
            &["en-US".into()],
            &[],
        );
        assert!(!breaks.is_empty());
        assert!(breaks.iter().all(|offset| text.is_char_boundary(*offset)));
        assert_eq!(text, "hyphenation");
    }

    #[test]
    fn suppresses_links_blockers_and_non_english_runs() {
        let text = "hyphenation";
        let mut linked = span(text, TextLanguage::EnglishUs);
        linked.suppress = true;
        assert!(break_opportunities(text, &[linked], &["en-US".into()], &[]).is_empty());
        assert!(
            break_opportunities(
                text,
                &[span(text, TextLanguage::EnglishUs)],
                &["en-US".into()],
                &[4],
            )
            .is_empty()
        );
        assert!(
            break_opportunities(
                text,
                &[span(text, TextLanguage::Other)],
                &["en-US".into()],
                &[],
            )
            .is_empty()
        );
    }

    #[test]
    fn manual_mode_uses_only_authored_soft_hyphens() {
        let text = "hy\u{00ad}phenation";
        let mut manual = span(text, TextLanguage::EnglishUs);
        manual.mode = HyphenationMode::Manual;
        let breaks = break_opportunities(text, &[manual], &["en-US".into()], &[]);
        assert_eq!(breaks, HashSet::from(["hy\u{00ad}".len()]));
    }
}
