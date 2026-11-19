//! Binary OEB markup used by Microsoft Reader.

use std::{collections::HashMap, fmt::Write};

use super::{invalid, maps::LitMap};
use crate::FormatError;

const CUSTOM_NAME: u32 = 0x8000;
const MAX_MARKUP: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 256;

#[derive(Default)]
pub(super) struct Atoms {
    pub tags: HashMap<u32, String>,
    pub attrs: HashMap<u32, String>,
}

pub(super) struct Tokens<'a> {
    rest: &'a [u8],
}

impl<'a> Tokens<'a> {
    pub fn new(rest: &'a [u8]) -> Self {
        Self { rest }
    }

    pub fn next(&mut self) -> Result<char, FormatError> {
        let Some(&byte) = self.rest.first() else {
            return Err(invalid("二进制文档意外结束"));
        };
        let len = if byte.is_ascii() {
            1
        } else {
            byte.leading_ones() as usize
        };
        let token = self
            .rest
            .get(..len)
            .ok_or_else(|| invalid("文档字符不完整"))?;
        let ch = std::str::from_utf8(token)
            .ok()
            .and_then(|s| s.chars().next())
            .ok_or_else(|| invalid("文档字符编码无效"))?;
        self.rest = &self.rest[len..];
        Ok(ch)
    }

    pub fn take(&mut self, count: usize) -> Result<&'a str, FormatError> {
        if count > self.rest.len() {
            return Err(invalid("二进制字符串长度超出文件范围"));
        }
        let start = self.rest;
        for _ in 0..count {
            self.next()?;
        }
        std::str::from_utf8(&start[..start.len() - self.rest.len()])
            .map_err(|_| invalid("文档字符编码无效"))
    }

    pub fn sized(&mut self) -> Result<&'a str, FormatError> {
        let count = self.next()? as usize;
        self.take(count)
    }

    pub fn rest(&self) -> &'a [u8] {
        self.rest
    }
}

pub(super) fn decode(
    bytes: &[u8],
    path: &str,
    paths: &HashMap<&str, &str>,
    map: &LitMap,
    atoms: &Atoms,
) -> Result<String, FormatError> {
    if bytes.len() > MAX_MARKUP {
        return Err(invalid("二进制文档超过大小限制"));
    }
    let mut tokens = Tokens::new(bytes);
    let mut html = String::new();
    let mut elements = Vec::<String>::new();
    while !tokens.rest().is_empty() {
        let plain_len = tokens
            .rest()
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(tokens.rest().len());
        let plain = std::str::from_utf8(&tokens.rest()[..plain_len])
            .map_err(|_| invalid("正文字符编码无效"))?;
        html.push_str(&quick_xml::escape::partial_escape(
            plain.replace('\u{000b}', "\n"),
        ));
        tokens = Tokens::new(&tokens.rest()[plain_len..]);
        if html.len() > MAX_MARKUP {
            return Err(invalid("解码文档超过大小限制"));
        }
        if tokens.rest().is_empty() {
            break;
        }
        tokens.next()?; // The zero byte marks the start of a markup operation.
        let flags = tokens.next()? as u32;
        if flags == 0 {
            continue;
        }
        let code = tokens.next()? as u32;
        if flags & 1 == 0 {
            if flags & 2 != 0
                && let Some(name) = elements.pop()
            {
                write!(html, "</{name}>").unwrap();
            }
            continue;
        }

        let atom = flags & 0x10 != 0;
        let name = if code == CUSTOM_NAME {
            name_token(&mut tokens)?
        } else if atom {
            atoms
                .tags
                .get(&code)
                .map(String::as_str)
                .unwrap_or("unknown")
        } else {
            map.tags
                .get(code as usize)
                .and_then(|&tag| tag)
                .unwrap_or("unknown")
        };
        let name = if valid_name(name) { name } else { "unknown" };
        write!(html, "<{name}").unwrap();

        loop {
            let attr = tokens.next()? as u32;
            if attr == 0 {
                break;
            }
            let key = if attr == CUSTOM_NAME {
                Some(name_token(&mut tokens)?)
            } else {
                let local = if atom {
                    atoms.attrs.get(&attr).map(String::as_str)
                } else {
                    u16::try_from(attr)
                        .ok()
                        .and_then(|attr| (map.tag_attr)(code as usize, attr))
                };
                local.or_else(|| u16::try_from(attr).ok().and_then(map.global_attr))
            };
            let length = tokens.next()? as u32;
            let value = if length == 0xffff {
                (tokens.next()? as u32).saturating_sub(1).to_string()
            } else {
                tokens.take(length.saturating_sub(1) as usize)?.to_owned()
            };
            if let Some(key) = key.filter(|key| valid_name(key)) {
                let value = if matches!(key, "href" | "src") {
                    resolve_link(&value, path, paths)
                } else {
                    value
                };
                write!(html, " {key}=\"{}\"", quick_xml::escape::escape(&value)).unwrap();
            }
            if html.len() > MAX_MARKUP {
                return Err(invalid("解码文档超过大小限制"));
            }
        }
        if flags & 2 != 0 {
            html.push_str(" />");
        } else {
            if elements.len() >= MAX_DEPTH {
                return Err(invalid("文档标签嵌套过深"));
            }
            html.push('>');
            elements.push(name.to_owned());
        }
    }
    for name in elements.into_iter().rev() {
        write!(html, "</{name}>").unwrap();
    }
    Ok(html)
}

fn name_token<'a>(tokens: &mut Tokens<'a>) -> Result<&'a str, FormatError> {
    let len = (tokens.next()? as usize).saturating_sub(1);
    tokens.take(len)
}

fn valid_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn resolve_link(value: &str, source: &str, paths: &HashMap<&str, &str>) -> String {
    // The first character is the link kind. The remaining text is the
    // manifest identifier (or a literal external URL), followed by a fragment.
    let target = value.get(1..).unwrap_or(value);
    let (id, fragment) = target
        .split_once('#')
        .map_or((target, None), |(id, fragment)| (id, Some(fragment)));
    let Some(&target_path) = paths.get(id) else {
        return target.to_owned();
    };
    let source_dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
    let base: Vec<_> = source_dir.split('/').filter(|s| !s.is_empty()).collect();
    let dest: Vec<_> = target_path.split('/').collect();
    let same = base.iter().zip(&dest).take_while(|(a, b)| a == b).count();
    let mut result = "../".repeat(base.len() - same);
    let relative = dest[same..].join("/");
    // Archive paths are decoded strings. Escape delimiter characters before
    // passing the relative reference to the EPUB and HTML URL resolvers.
    result.push_str(
        &relative
            .replace('%', "%25")
            .replace('#', "%23")
            .replace('?', "%3F"),
    );
    if let Some(fragment) = fragment {
        result.push('#');
        result.push_str(fragment);
    }
    result
}
