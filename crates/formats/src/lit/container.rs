//! Microsoft Reader's ITOLITLS directory and data spaces.

use std::collections::HashMap;

use super::{
    invalid,
    lzx::LzxState,
    tokens::{Atoms, Tokens},
};
use crate::FormatError;

const MAX_UNPACKED_BYTES: usize = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const BLOCK_BYTES: usize = 32 * 1024;
const LZX: &[u8; 16] = b"\xc6\x07\x90\x0a\x76\x40\xd3\x11\x87\x89\0\0\xf8\x10\x57\x54";
const LZX_RESET: &str = "Transform/{0A9007C6-4076-11D3-8789-0000F8105754}/InstanceData/ResetTable";

#[derive(Clone, Copy)]
struct Entry {
    section: usize,
    start: usize,
    size: usize,
}

pub(super) struct Container<'a> {
    data: &'a [u8],
    entries: HashMap<&'a str, Entry>,
    data_spaces: Vec<String>,
    expanded: HashMap<usize, Vec<u8>>,
}

pub(super) struct Item {
    pub id: String,
    pub path: String,
    pub media: String,
    pub is_document: bool,
    pub linear: bool,
}

impl<'a> Container<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self, FormatError> {
        if !bytes.starts_with(b"ITOLITLS") || u32_at(bytes, 8)? != 1 {
            return Err(invalid("不是可识别的 Microsoft Reader 文件"));
        }
        let header_size = u32_at(bytes, 12)?;
        let pieces = u32_at(bytes, 16)?;
        let secondary_size = u32_at(bytes, 20)?;
        if header_size < 24 || !(2..=32).contains(&pieces) {
            return Err(invalid("文件头无效"));
        }
        let secondary = slice(bytes, header_size + pieces * 16, secondary_size)?;
        let caol = u32_at(secondary, 4)?;
        if slice(secondary, caol, 4)? != b"CAOL" {
            return Err(invalid("缺少 CAOL 文件头"));
        }
        // CAOL includes the trailing ITSF, so its declared length is
        // larger than the fixed prefix preceding the embedded header.
        let itsf = caol + 48;
        if slice(secondary, itsf, 4)? != b"ITSF" {
            return Err(invalid("缺少 ITSF 文件头"));
        }
        let start = u64_at(secondary, itsf + 16)?;
        let data = bytes
            .get(start..)
            .ok_or_else(|| invalid("数据区偏移无效"))?;
        let directory = slice(
            bytes,
            u64_at(bytes, header_size + 16)?,
            u64_at(bytes, header_size + 24)?,
        )?;
        if !directory.starts_with(b"IFCM") {
            return Err(invalid("缺少 IFCM 文件目录"));
        }
        let chunk_size = u32_at(directory, 8)?;
        let count = u32_at(directory, 24)?;
        if chunk_size < 48 || count > directory.len().saturating_sub(32) / chunk_size {
            return Err(invalid("文件目录长度无效"));
        }
        let mut entries = HashMap::new();
        for chunk in directory[32..].chunks_exact(chunk_size).take(count) {
            if !chunk.starts_with(b"AOLL") {
                continue;
            }
            let end = chunk_size
                .checked_sub(u32_at(chunk, 4)?)
                .ok_or_else(|| invalid("文件目录条目长度无效"))?;
            let mut record = chunk
                .get(48..end)
                .ok_or_else(|| invalid("文件目录条目无效"))?;
            while !record.is_empty() {
                let len = compressed_integer(&mut record)?;
                if len == 0 {
                    if record.iter().all(|&byte| byte == 0) {
                        break;
                    }
                    return Err(invalid("文件目录包含空文件名"));
                }
                let (name, rest) = record
                    .split_at_checked(len)
                    .ok_or_else(|| invalid("文件名长度无效"))?;
                let name = std::str::from_utf8(name).map_err(|_| invalid("文件名编码无效"))?;
                record = rest;
                let section = compressed_integer(&mut record)?;
                let start = compressed_integer(&mut record)?;
                let size = compressed_integer(&mut record)?;
                entries.insert(
                    name,
                    Entry {
                        section,
                        start,
                        size,
                    },
                );
                if entries.len() > MAX_ENTRIES {
                    return Err(invalid("文件目录条目过多"));
                }
            }
        }
        if entries.contains_key("/DRMStorage/Licenses/EUL") {
            return Err(invalid("此电子书需要 Microsoft Reader 的 DRM 授权"));
        }
        let mut container = Self {
            data,
            entries,
            data_spaces: Vec::new(),
            expanded: HashMap::new(),
        };
        container.data_spaces = container.read_data_spaces()?;
        Ok(container)
    }

    fn read_data_spaces(&self) -> Result<Vec<String>, FormatError> {
        let Some(entry) = self.entries.get("::DataSpace/NameList") else {
            return Ok(Vec::new());
        };
        let bytes = slice(self.data, entry.start, entry.size)?;
        let count = u16_at(bytes, 2)?;
        let mut offset = 4;
        let mut names = Vec::new();
        for _ in 0..count {
            let size = u16_at(bytes, offset)? * 2;
            let name = slice(bytes, offset + 2, size)?;
            let (decoded, malformed) = encoding_rs::UTF_16LE.decode_without_bom_handling(name);
            if malformed {
                return Err(invalid("数据区名称编码无效"));
            }
            names.push(decoded.into_owned());
            offset += size + 4;
        }
        Ok(names)
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn read(&mut self, name: &str) -> Result<&[u8], FormatError> {
        let entry = *self
            .entries
            .get(name)
            .ok_or_else(|| invalid(format!("缺少资源 {name}")))?;
        if entry.section == 0 {
            return slice(self.data, entry.start, entry.size);
        }
        if !self.expanded.contains_key(&entry.section) {
            let budget = MAX_UNPACKED_BYTES - self.expanded.values().map(Vec::len).sum::<usize>();
            let expanded = self.expand(entry.section, budget)?;
            self.expanded.insert(entry.section, expanded);
        }
        slice(&self.expanded[&entry.section], entry.start, entry.size)
    }

    fn expand(&self, index: usize, budget: usize) -> Result<Vec<u8>, FormatError> {
        let name = self
            .data_spaces
            .get(index)
            .ok_or_else(|| invalid("数据区编号无效"))?;
        let prefix = format!("::DataSpace/Storage/{name}/");
        let content = self.raw(&prefix, "Content")?;
        if !self.has(&format!("{prefix}Transform/List")) {
            if content.len() > budget {
                return Err(invalid("解压内容超过大小限制"));
            }
            return Ok(content.to_vec());
        }
        if self.raw(&prefix, "Transform/List")? != LZX {
            return Err(invalid("此数据区已加密或使用了不支持的压缩方式"));
        }
        let control = self.raw(&prefix, "ControlData")?;
        let reset = self.raw(&prefix, LZX_RESET)?;
        decompress(content, control, reset, budget)
    }

    fn raw(&self, prefix: &str, name: &str) -> Result<&[u8], FormatError> {
        let key = format!("{prefix}{name}");
        let entry = self
            .entries
            .get(key.as_str())
            .filter(|entry| entry.section == 0)
            .ok_or_else(|| invalid(format!("缺少数据区资源 {name}")))?;
        slice(self.data, entry.start, entry.size)
    }

    pub fn manifest(&mut self) -> Result<Vec<Item>, FormatError> {
        let mut bytes = self.read("/manifest")?;
        let mut items = Vec::new();
        loop {
            let Some((&root_len, rest)) = bytes.split_first() else {
                break;
            };
            if root_len == 0 {
                break;
            }
            bytes = rest
                .get(root_len as usize..)
                .ok_or_else(|| invalid("清单路径无效"))?;
            for group in 0..4 {
                let count = u32_at(bytes, 0)?;
                if count > MAX_ENTRIES - items.len() {
                    return Err(invalid("资源清单条目过多"));
                }
                bytes = &bytes[4..];
                for _ in 0..count {
                    let mut tokens =
                        Tokens::new(bytes.get(4..).ok_or_else(|| invalid("资源清单不完整"))?);
                    let id = tokens.sized()?.to_owned();
                    let path = normalize_path(tokens.sized()?);
                    let media = tokens.sized()?.to_ascii_lowercase();
                    bytes = tokens.rest().strip_prefix(&[0]).unwrap_or(tokens.rest());
                    let is_document = group < 2;
                    items.push(Item {
                        id,
                        path,
                        media,
                        is_document,
                        linear: group == 0,
                    });
                }
            }
        }
        Ok(items)
    }

    pub fn atoms(&mut self, item: &Item) -> Result<Atoms, FormatError> {
        let path = format!("/data/{}/atom", item.id);
        let mut atoms = Atoms::default();
        if !self.has(&path) {
            return Ok(atoms);
        }
        let mut bytes = self.read(&path)?;
        for (wide, table) in [(false, &mut atoms.tags), (true, &mut atoms.attrs)] {
            let count = u32_at(bytes, 0)?;
            if count > MAX_ENTRIES {
                return Err(invalid("文档自定义标签过多"));
            }
            bytes = &bytes[4..];
            for number in 1..=count {
                let (len, prefix) = if wide {
                    (u32_at(bytes, 0)?, 4)
                } else {
                    (slice(bytes, 0, 1)?[0] as usize, 1)
                };
                let name = slice(bytes, prefix, len)?;
                let name = std::str::from_utf8(name).map_err(|_| invalid("文档标签编码无效"))?;
                table.insert(number as u32, name.to_owned());
                bytes = &bytes[prefix + len..];
            }
        }
        Ok(atoms)
    }
}

fn decompress(
    content: &[u8],
    control: &[u8],
    reset: &[u8],
    budget: usize,
) -> Result<Vec<u8>, FormatError> {
    if slice(control, 4, 4)? != b"LZXC" || u64_at(reset, 32)? != BLOCK_BYTES {
        return Err(invalid("LZX 压缩数据头无效"));
    }
    let indicator = u32_at(control, 12)?;
    if indicator == 0 {
        return Err(invalid("LZX 窗口无效"));
    }
    let window_bits = (14 + indicator.ilog2() + 1) as u8;
    let mut decoder = LzxState::new(window_bits).map_err(invalid)?;
    let reset_blocks = (1usize << window_bits) / BLOCK_BYTES;
    let size = u64_at(reset, 16)?;
    let compressed_size = u64_at(reset, 24)?;
    let offsets = u32_at(reset, 12)?;
    let blocks = size.div_ceil(BLOCK_BYTES);
    let declared_blocks = u32_at(reset, 4)?;
    if size > budget || compressed_size > content.len() || blocks > declared_blocks {
        return Err(invalid("LZX 解压数据超出范围"));
    }
    let table = slice(reset, offsets, blocks * 8)?;
    let mut out = vec![0; size];
    for block in 0..blocks {
        if block != 0 && block % reset_blocks == 0 {
            decoder.reset();
        }
        let start = u64_at(table, block * 8)?;
        let end = if block + 1 == blocks {
            compressed_size
        } else {
            u64_at(table, block * 8 + 8)?
        };
        let input = content
            .get(start..end)
            .filter(|block| !block.is_empty())
            .ok_or_else(|| invalid("LZX 压缩块偏移无效"))?;
        let offset = block * BLOCK_BYTES;
        let output = &mut out[offset..(offset + BLOCK_BYTES).min(size)];
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decoder.decompress(input, output)
        }))
        .map_err(|_| invalid("LZX 压缩块已损坏"))?
        .map_err(|error| invalid(format!("LZX 块 {block}：{error}")))?;
    }
    Ok(out)
}

fn normalize_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = if path.as_bytes().get(1) == Some(&b':') {
        &path[2..]
    } else {
        &path
    };
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            name => components.push(name),
        }
    }
    components.join("/")
}

fn compressed_integer(bytes: &mut &[u8]) -> Result<usize, FormatError> {
    let mut value = 0usize;
    while let Some((&byte, rest)) = bytes.split_first() {
        *bytes = rest;
        value = value
            .checked_mul(128)
            .and_then(|value| value.checked_add(usize::from(byte & 127)))
            .ok_or_else(|| invalid("文件目录偏移超出范围"))?;
        if byte < 128 {
            return Ok(value);
        }
    }
    Err(invalid("文件目录条目不完整"))
}

fn slice(bytes: &[u8], start: usize, size: usize) -> Result<&[u8], FormatError> {
    bytes
        .get(start..start.saturating_add(size))
        .ok_or_else(|| invalid("文件内偏移超出范围"))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<usize, FormatError> {
    Ok(u16::from_le_bytes(slice(bytes, offset, 2)?.try_into().unwrap()) as usize)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<usize, FormatError> {
    Ok(u32::from_le_bytes(slice(bytes, offset, 4)?.try_into().unwrap()) as usize)
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<usize, FormatError> {
    usize::try_from(u64::from_le_bytes(
        slice(bytes, offset, 8)?.try_into().unwrap(),
    ))
    .map_err(|_| invalid("文件内偏移超出范围"))
}
