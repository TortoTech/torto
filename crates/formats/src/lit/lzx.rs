// SPDX-License-Identifier: MIT
// LZX decoder from libchm 0.2.0 (Quin Gillespie). See LICENSE-libchm.
// Adapted for direct use with Microsoft Reader data spaces.

#[derive(Debug, thiserror::Error)]
pub(super) enum LzxError {
    #[error("illegal data in LZX stream")]
    IllegalData,
    #[error("data format error in LZX stream")]
    DataFormat,
    #[error("invalid LZX window bits: must be 15-21, got {0}")]
    InvalidWindow(u8),
}

const LZX_MIN_MATCH: usize = 2;
const LZX_NUM_CHARS: usize = 256;
const LZX_PRETREE_NUM_ELEMENTS: usize = 20;
const LZX_ALIGNED_NUM_ELEMENTS: usize = 8;
const LZX_NUM_PRIMARY_LENGTHS: usize = 7;
const LZX_NUM_SECONDARY_LENGTHS: usize = 249;
const LZX_PRETREE_MAXSYMBOLS: usize = LZX_PRETREE_NUM_ELEMENTS; // 20
const LZX_PRETREE_TABLEBITS: usize = 6;
const LZX_MAINTREE_MAXSYMBOLS: usize = LZX_NUM_CHARS + 50 * 8; // 656
const LZX_MAINTREE_TABLEBITS: usize = 12;
const LZX_LENGTH_MAXSYMBOLS: usize = LZX_NUM_SECONDARY_LENGTHS + 1; // 250
const LZX_LENGTH_TABLEBITS: usize = 12;
const LZX_ALIGNED_MAXSYMBOLS: usize = LZX_ALIGNED_NUM_ELEMENTS; // 8
const LZX_ALIGNED_TABLEBITS: usize = 7;
const LZX_LENTABLE_SAFETY: usize = 64;
const BLOCKTYPE_VERBATIM: u8 = 1;
const BLOCKTYPE_ALIGNED: u8 = 2;
const BLOCKTYPE_UNCOMPRESSED: u8 = 3;

static EXTRA_BITS: [u8; 51] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
];

static POSITION_BASE: [u32; 51] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1_024, 1_536,
    2_048, 3_072, 4_096, 6_144, 8_192, 12_288, 16_384, 24_576, 32_768, 49_152, 65_536, 98_304,
    131_072, 196_608, 262_144, 393_216, 524_288, 655_360, 786_432, 917_504, 1_048_576, 1_179_648,
    1_310_720, 1_441_792, 1_572_864, 1_703_936, 1_835_008, 1_966_080, 2_097_152,
];

const PRETREE_TABLE_SIZE: usize = (1 << LZX_PRETREE_TABLEBITS) + LZX_PRETREE_MAXSYMBOLS * 2;
const MAINTREE_TABLE_SIZE: usize = (1 << LZX_MAINTREE_TABLEBITS) + LZX_MAINTREE_MAXSYMBOLS * 2;
const LENGTH_TABLE_SIZE: usize = (1 << LZX_LENGTH_TABLEBITS) + LZX_LENGTH_MAXSYMBOLS * 2;
const ALIGNED_TABLE_SIZE: usize = (1 << LZX_ALIGNED_TABLEBITS) + LZX_ALIGNED_MAXSYMBOLS * 2;

pub(super) struct LzxState {
    window: Vec<u8>,
    window_size: u32,
    window_posn: u32,
    r0: u32,
    r1: u32,
    r2: u32,
    main_elements: u16,
    header_read: bool,
    block_type: u8,
    block_length: u32,
    block_remaining: u32,
    frames_read: u32,
    intel_filesize: i32,
    /// i64 to avoid wrapping for large (>2 GB) CHM decompression sequences.
    intel_curpos: i64,
    intel_started: bool,
    // Code-length tables (persist across decompress calls; deltas applied each block)
    pretree_len: [u8; LZX_PRETREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
    maintree_len: [u8; LZX_MAINTREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
    length_len: [u8; LZX_LENGTH_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
    aligned_len: [u8; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
    // Fast-lookup decode tables (rebuilt at the start of each block)
    pretree_table: [u16; PRETREE_TABLE_SIZE],
    maintree_table: [u16; MAINTREE_TABLE_SIZE],
    length_table: [u16; LENGTH_TABLE_SIZE],
    aligned_table: [u16; ALIGNED_TABLE_SIZE],
}

impl LzxState {
    pub fn new(window_bits: u8) -> Result<Box<Self>, LzxError> {
        if !(15..=21).contains(&window_bits) {
            return Err(LzxError::InvalidWindow(window_bits));
        }
        let window_size = 1u32 << window_bits;
        let posn_slots: u16 = match window_bits {
            20 => 42,
            21 => 50,
            n => u16::from(n) * 2,
        };
        let main_elements = 256u16 + posn_slots * 8;
        let state = Self {
            window: vec![0u8; window_size as usize],
            window_size,
            window_posn: 0,
            r0: 1,
            r1: 1,
            r2: 1,
            main_elements,
            header_read: false,
            block_type: 0,
            block_length: 0,
            block_remaining: 0,
            frames_read: 0,
            intel_filesize: 0,
            intel_curpos: 0,
            intel_started: false,
            pretree_len: [0; LZX_PRETREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
            maintree_len: [0; LZX_MAINTREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
            length_len: [0; LZX_LENGTH_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
            aligned_len: [0; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
            pretree_table: [0; PRETREE_TABLE_SIZE],
            maintree_table: [0; MAINTREE_TABLE_SIZE],
            length_table: [0; LENGTH_TABLE_SIZE],
            aligned_table: [0; ALIGNED_TABLE_SIZE],
        };
        Ok(Box::new(state))
    }

    pub fn reset(&mut self) {
        self.r0 = 1;
        self.r1 = 1;
        self.r2 = 1;
        self.header_read = false;
        self.frames_read = 0;
        self.block_remaining = 0;
        self.block_type = 0;
        self.intel_curpos = 0;
        self.intel_started = false;
        self.window_posn = 0;
        self.maintree_len[..LZX_MAINTREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY].fill(0);
        self.length_len[..LZX_LENGTH_MAXSYMBOLS + LZX_LENTABLE_SAFETY].fill(0);
    }

    // Remaining casts are bounded by LZX spec invariants:
    //   - block sizes fit in i32 (LZX blocks ≤ 65535 bytes)
    //   - window_posn/window_size are u32 → usize (lossless on all ≥32-bit targets)
    //   - bits.read(N) as u8 with N ≤ 4 (max value 15, fits in u8)
    //   - this_run bounded by block_remaining (u32) and togo (i32 ≤ 65535)
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    #[allow(clippy::too_many_lines)]
    pub fn decompress(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), LzxError> {
        let out_len = output.len();
        let out_len_i32 = i32::try_from(out_len).map_err(|_| LzxError::DataFormat)?;
        let mut bits = Bits::new(input);
        let mut togo = out_len_i32;
        if !self.header_read {
            let k = bits.read(1);
            let filesize = if k != 0 {
                let hi = bits.read(16);
                let lo = bits.read(16);
                (hi << 16) | lo
            } else {
                0
            };
            self.intel_filesize = filesize.cast_signed();
            self.header_read = true;
        }
        while togo > 0 {
            if self.block_remaining == 0 {
                if self.block_type == BLOCKTYPE_UNCOMPRESSED {
                    // Re-align to word boundary after an uncompressed block
                    if self.block_length & 1 != 0 {
                        bits.skip_byte();
                    }
                    bits.reinit();
                }
                let bt = bits.read(3) as u8;
                let blen_hi = bits.read(16);
                let blen_lo = bits.read(8);
                let blen = (blen_hi << 8) | blen_lo;
                self.block_type = bt;
                self.block_length = blen;
                self.block_remaining = blen;
                match bt {
                    BLOCKTYPE_ALIGNED => {
                        for slot in &mut self.aligned_len[..LZX_ALIGNED_NUM_ELEMENTS] {
                            *slot = bits.read(3) as u8;
                        }
                        make_decode_table(
                            LZX_ALIGNED_MAXSYMBOLS,
                            LZX_ALIGNED_TABLEBITS,
                            &self.aligned_len,
                            &mut self.aligned_table,
                        )?;
                        // Fall through: aligned blocks also have MAINTREE and LENGTH
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.maintree_len,
                            0,
                            256,
                        )?;
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.maintree_len,
                            256,
                            self.main_elements as usize,
                        )?;
                        make_decode_table(
                            LZX_MAINTREE_MAXSYMBOLS,
                            LZX_MAINTREE_TABLEBITS,
                            &self.maintree_len,
                            &mut self.maintree_table,
                        )?;
                        if self.maintree_len[0xE8] != 0 {
                            self.intel_started = true;
                        }
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.length_len,
                            0,
                            LZX_NUM_SECONDARY_LENGTHS,
                        )?;
                        make_decode_table(
                            LZX_LENGTH_MAXSYMBOLS,
                            LZX_LENGTH_TABLEBITS,
                            &self.length_len,
                            &mut self.length_table,
                        )?;
                    }
                    BLOCKTYPE_VERBATIM => {
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.maintree_len,
                            0,
                            256,
                        )?;
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.maintree_len,
                            256,
                            self.main_elements as usize,
                        )?;
                        make_decode_table(
                            LZX_MAINTREE_MAXSYMBOLS,
                            LZX_MAINTREE_TABLEBITS,
                            &self.maintree_len,
                            &mut self.maintree_table,
                        )?;
                        if self.maintree_len[0xE8] != 0 {
                            self.intel_started = true;
                        }
                        read_lens(
                            &mut bits,
                            &mut self.pretree_len,
                            &mut self.pretree_table,
                            &mut self.length_len,
                            0,
                            LZX_NUM_SECONDARY_LENGTHS,
                        )?;
                        make_decode_table(
                            LZX_LENGTH_MAXSYMBOLS,
                            LZX_LENGTH_TABLEBITS,
                            &self.length_len,
                            &mut self.length_table,
                        )?;
                    }
                    BLOCKTYPE_UNCOMPRESSED => {
                        self.intel_started = true;
                        // Align bitstream to 16-bit boundary
                        bits.ensure(16);
                        if bits.left > 16 {
                            bits.pos -= 2;
                        }
                        if bits.pos + 12 > input.len() {
                            return Err(LzxError::IllegalData);
                        }
                        self.r0 =
                            u32::from_le_bytes(input[bits.pos..bits.pos + 4].try_into().unwrap());
                        self.r1 = u32::from_le_bytes(
                            input[bits.pos + 4..bits.pos + 8].try_into().unwrap(),
                        );
                        self.r2 = u32::from_le_bytes(
                            input[bits.pos + 8..bits.pos + 12].try_into().unwrap(),
                        );
                        bits.pos += 12;
                    }
                    _ => return Err(LzxError::IllegalData),
                }
            }
            if bits.pos > input.len() + 2 || (bits.pos > input.len() && bits.left < 16) {
                return Err(LzxError::IllegalData);
            }
            // togo > 0 in this branch, so cast_unsigned is safe; min produces u32 ≤ block_remaining
            let this_run = self.block_remaining.min(togo.cast_unsigned()) as usize;
            togo -= this_run as i32;
            self.block_remaining -= this_run as u32;
            let wp = self.window_posn as usize;
            let ws = self.window_size as usize;
            if (wp & (ws - 1)) + this_run > ws {
                return Err(LzxError::DataFormat);
            }
            match self.block_type {
                BLOCKTYPE_VERBATIM => {
                    decode_matches(
                        &mut bits,
                        &mut self.window,
                        wp,
                        this_run,
                        self.window_size,
                        &mut self.r0,
                        &mut self.r1,
                        &mut self.r2,
                        &self.maintree_table,
                        &self.maintree_len,
                        &self.length_table,
                        &self.length_len,
                        None,
                        None,
                    )?;
                    self.window_posn += this_run as u32;
                }
                BLOCKTYPE_ALIGNED => {
                    decode_matches(
                        &mut bits,
                        &mut self.window,
                        wp,
                        this_run,
                        self.window_size,
                        &mut self.r0,
                        &mut self.r1,
                        &mut self.r2,
                        &self.maintree_table,
                        &self.maintree_len,
                        &self.length_table,
                        &self.length_len,
                        Some(&self.aligned_table),
                        Some(&self.aligned_len),
                    )?;
                    self.window_posn += this_run as u32;
                }
                BLOCKTYPE_UNCOMPRESSED => {
                    if bits.pos + this_run > input.len() {
                        return Err(LzxError::IllegalData);
                    }
                    self.window[wp..wp + this_run]
                        .copy_from_slice(&input[bits.pos..bits.pos + this_run]);
                    bits.pos += this_run;
                    self.window_posn += this_run as u32;
                }
                _ => return Err(LzxError::IllegalData),
            }
        }
        if togo != 0 {
            return Err(LzxError::IllegalData);
        }
        let final_pos = if self.window_posn == 0 {
            self.window_size as usize
        } else {
            self.window_posn as usize
        };
        let src_start = final_pos.checked_sub(out_len).ok_or(LzxError::DataFormat)?;
        output.copy_from_slice(&self.window[src_start..final_pos]);
        if self.frames_read < 32768 && self.intel_filesize != 0 {
            if out_len > 6 && self.intel_started {
                let curpos_start = self.intel_curpos;
                let mut curpos = curpos_start;
                let filesize = self.intel_filesize;
                let end = out_len - 10;
                let mut i = 0usize;
                while i < end {
                    if output[i] != 0xE8 {
                        i += 1;
                        curpos += 1;
                        continue;
                    }
                    let abs_off = i32::from_le_bytes(output[i + 1..i + 5].try_into().unwrap());
                    if i64::from(abs_off) >= -curpos && abs_off < filesize {
                        let rel_off = if abs_off >= 0 {
                            (i64::from(abs_off) - curpos) as i32
                        } else {
                            abs_off + filesize
                        };
                        output[i + 1..i + 5].copy_from_slice(&rel_off.to_le_bytes());
                    }
                    i += 5;
                    curpos += 5;
                }
                // out_len_i32 checked to fit in i32 at entry; i64::from is lossless
                self.intel_curpos = curpos_start + i64::from(out_len_i32);
            } else {
                self.intel_curpos += i64::from(out_len_i32);
            }
        }
        self.frames_read += 1;
        Ok(())
    }
}

struct Bits<'a> {
    buf: u32,
    left: i32,
    src: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    const fn new(src: &'a [u8]) -> Self {
        Self {
            buf: 0,
            left: 0,
            src,
            pos: 0,
        }
    }

    const fn reinit(&mut self) {
        self.buf = 0;
        self.left = 0;
    }

    const fn skip_byte(&mut self) {
        self.pos += 1;
    }

    /// Fill the bit buffer until it holds at least `n` bits. Pads zeros at end-of-input (matches C reference).
    #[inline]
    fn ensure(&mut self, n: i32) {
        while self.left < n {
            if self.pos + 1 >= self.src.len() {
                if self.pos >= self.src.len() {
                    self.left += 16;
                    continue;
                }
                let w = u32::from(self.src[self.pos]);
                // left is always ≥ 0 here (we only reach this after left < n with n > 0)
                let shift = 16u32.saturating_sub(self.left.cast_unsigned());
                self.buf |= w << shift;
                self.left += 8;
                self.pos += 1;
                continue;
            }
            let w = u32::from(self.src[self.pos]) | (u32::from(self.src[self.pos + 1]) << 8);
            let shift = 16u32.saturating_sub(self.left.cast_unsigned());
            self.buf |= w << shift;
            self.left += 16;
            self.pos += 2;
        }
    }

    #[inline]
    const fn peek(&self, n: i32) -> u32 {
        // n is always a small positive count (1–16); cast_unsigned is safe
        self.buf >> (32 - n.cast_unsigned())
    }

    #[inline]
    const fn remove(&mut self, n: i32) {
        // n is always a small positive count (1–16); cast_unsigned is safe
        self.buf = self.buf.wrapping_shl(n.cast_unsigned());
        self.left -= n;
    }

    #[inline]
    fn read(&mut self, n: i32) -> u32 {
        self.ensure(n);
        let v = self.peek(n);
        self.remove(n);
        v
    }

    // tablebits is always ≤ LZX_MAINTREE_TABLEBITS (12): fits in both i32 and u32
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn read_huffsym(
        &mut self,
        table: &[u16],
        lens: &[u8],
        tablebits: usize,
        maxsymbols: usize,
    ) -> Result<usize, LzxError> {
        self.ensure(16);
        let mut i = table[self.peek(tablebits as i32) as usize] as usize;
        if i >= maxsymbols {
            let mut j = 1u32 << (32 - tablebits as u32 - 1);
            loop {
                i <<= 1;
                if self.buf & j != 0 {
                    i |= 1;
                }
                j >>= 1;
                if j == 0 {
                    return Err(LzxError::IllegalData);
                }
                i = table[i] as usize;
                if i < maxsymbols {
                    break;
                }
            }
        }
        let sym = i;
        let code_len = i32::from(lens[sym]);
        self.remove(code_len);
        Ok(sym)
    }
}

// Remaining casts are bounded by construction:
//   - sym < nsyms ≤ LZX_MAINTREE_MAXSYMBOLS (656) — fits in u16
//   - next_symbol < table.len() / 2 (guarded by the ns2+1 check) — fits in u16
//   - pos/table_mask ≤ 1<<nbits ≤ 1<<16 — fits in u32
//   - bit_mask ≤ 1<<15 — fits in u32
#[allow(clippy::cast_possible_truncation)]
fn make_decode_table(
    nsyms: usize,
    nbits: usize,
    lengths: &[u8],
    table: &mut [u16],
) -> Result<(), LzxError> {
    let table_mask = 1usize << nbits;
    let mut bit_mask = table_mask >> 1;
    let mut next_symbol = bit_mask; // base for long-code tree allocation
    let mut pos: usize = 0;
    let mut bit_num = 1usize;
    while bit_num <= nbits {
        for (sym, &len) in lengths.iter().enumerate().take(nsyms) {
            if len as usize == bit_num {
                let leaf = pos;
                pos += bit_mask;
                if pos > table_mask {
                    return Err(LzxError::IllegalData);
                }
                for entry in &mut table[leaf..leaf + bit_mask] {
                    *entry = sym as u16;
                }
            }
        }
        bit_mask >>= 1;
        bit_num += 1;
    }
    if pos != table_mask {
        for entry in &mut table[pos..table_mask] {
            *entry = 0;
        }
        let mut pos32 = (pos as u32) << 16;
        let table_mask32 = (table_mask as u32) << 16;
        bit_mask = 1 << 15;
        while bit_num <= 16 {
            for (sym, &len) in lengths.iter().enumerate().take(nsyms) {
                if len as usize == bit_num {
                    let mut leaf = (pos32 >> 16) as usize;
                    for fill in 0..(bit_num - nbits) {
                        if table[leaf] == 0 {
                            let ns2 = next_symbol << 1;
                            if ns2 + 1 >= table.len() {
                                return Err(LzxError::IllegalData);
                            }
                            table[ns2] = 0;
                            table[ns2 + 1] = 0;
                            table[leaf] = next_symbol as u16;
                            next_symbol += 1;
                        }
                        leaf = (table[leaf] as usize) << 1;
                        if (pos32 >> (15 - fill as u32)) & 1 != 0 {
                            leaf += 1;
                        }
                    }
                    table[leaf] = sym as u16;
                    pos32 += bit_mask as u32;
                    if pos32 > table_mask32 {
                        return Err(LzxError::IllegalData);
                    }
                }
            }
            bit_mask >>= 1;
            bit_num += 1;
        }
        if pos32 != table_mask32 {
            for &len in lengths.iter().take(nsyms) {
                if len != 0 {
                    return Err(LzxError::IllegalData);
                }
            }
        }
    }
    Ok(())
}

// Remaining casts are bounded by construction:
//   - bits.read(N) as u8 with N ≤ 4: max value 15, fits in u8
//   - bits.read(N) as usize: u32 → usize (lossless on all ≥32-bit platforms)
//   - z/sym as i32: values ≤ LZX_PRETREE_MAXSYMBOLS (20), fit in i32
//   - .rem_euclid(17) as u8: result in [0, 16], fits in u8
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn read_lens(
    bits: &mut Bits,
    pretree_len: &mut [u8],
    pretree_table: &mut [u16],
    lens: &mut [u8],
    first: usize,
    last: usize,
) -> Result<(), LzxError> {
    for slot in &mut pretree_len[..LZX_PRETREE_NUM_ELEMENTS] {
        *slot = bits.read(4) as u8;
    }
    make_decode_table(
        LZX_PRETREE_MAXSYMBOLS,
        LZX_PRETREE_TABLEBITS,
        pretree_len,
        pretree_table,
    )?;
    let mut x = first;
    while x < last {
        let z = bits.read_huffsym(
            pretree_table,
            pretree_len,
            LZX_PRETREE_TABLEBITS,
            LZX_PRETREE_MAXSYMBOLS,
        )?;
        if z == 17 {
            let y = bits.read(4) as usize + 4;
            let end = (x + y).min(last);
            lens[x..end].fill(0);
            x = end;
        } else if z == 18 {
            let y = bits.read(5) as usize + 20;
            let end = (x + y).min(last);
            lens[x..end].fill(0);
            x = end;
        } else if z == 19 {
            let y = bits.read(1) as usize + 4;
            let sym = bits.read_huffsym(
                pretree_table,
                pretree_len,
                LZX_PRETREE_TABLEBITS,
                LZX_PRETREE_MAXSYMBOLS,
            )?;
            let val = (i32::from(lens[x]) - sym as i32).rem_euclid(17) as u8;
            let end = (x + y).min(last);
            lens[x..end].fill(val);
            x = end;
        } else {
            lens[x] = (i32::from(lens[x]) - z as i32).rem_euclid(17) as u8;
            x += 1;
        }
    }
    Ok(())
}

// Remaining casts are bounded by construction:
//   - main_element < LZX_NUM_CHARS (256) when cast to u8
//   - aligned_bits < LZX_ALIGNED_MAXSYMBOLS (8) when cast to u32
//   - wp/ws < window_size ≤ 2^21 when cast to i64 for back-reference arithmetic
//   - .rem_euclid(ws) result < ws ≤ 2^21, fits in usize
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::too_many_arguments
)]
fn decode_matches(
    bits: &mut Bits,
    window: &mut [u8],
    mut wp: usize,
    mut this_run: usize,
    window_size: u32,
    r0: &mut u32,
    r1: &mut u32,
    r2: &mut u32,
    maintree_table: &[u16],
    maintree_len: &[u8],
    length_table: &[u16],
    length_len: &[u8],
    aligned_table: Option<&[u16]>,
    aligned_len: Option<&[u8]>,
) -> Result<(), LzxError> {
    let ws = window_size as usize;
    while this_run > 0 {
        let main_element = bits.read_huffsym(
            maintree_table,
            maintree_len,
            LZX_MAINTREE_TABLEBITS,
            LZX_MAINTREE_MAXSYMBOLS,
        )?;
        if main_element < LZX_NUM_CHARS {
            window[wp] = main_element as u8;
            wp += 1;
            this_run -= 1;
        } else {
            let me = main_element - LZX_NUM_CHARS;
            let mut match_length = me & LZX_NUM_PRIMARY_LENGTHS;
            if match_length == LZX_NUM_PRIMARY_LENGTHS {
                let length_footer = bits.read_huffsym(
                    length_table,
                    length_len,
                    LZX_LENGTH_TABLEBITS,
                    LZX_LENGTH_MAXSYMBOLS,
                )?;
                match_length += length_footer;
            }
            match_length += LZX_MIN_MATCH;
            let match_offset_slot = me >> 3;
            let match_offset;
            if match_offset_slot > 2 {
                let extra = i32::from(EXTRA_BITS[match_offset_slot]);
                match_offset = if let (Some(at), Some(al)) = (aligned_table, aligned_len) {
                    let base = POSITION_BASE[match_offset_slot] - 2;
                    if extra > 3 {
                        let verbatim_bits = bits.read(extra - 3);
                        let aligned_bits = bits.read_huffsym(
                            at,
                            al,
                            LZX_ALIGNED_TABLEBITS,
                            LZX_ALIGNED_MAXSYMBOLS,
                        )?;
                        base + (verbatim_bits << 3) + aligned_bits as u32
                    } else if extra == 3 {
                        let aligned_bits = bits.read_huffsym(
                            at,
                            al,
                            LZX_ALIGNED_TABLEBITS,
                            LZX_ALIGNED_MAXSYMBOLS,
                        )?;
                        base + aligned_bits as u32
                    } else if extra > 0 {
                        let verbatim_bits = bits.read(extra);
                        base + verbatim_bits
                    } else {
                        1u32
                    }
                } else if match_offset_slot == 3 {
                    // Slot 3: EXTRA_BITS[3] == 0, POSITION_BASE[3] - 2 == 1
                    1u32
                } else {
                    let verbatim_bits = bits.read(extra);
                    POSITION_BASE[match_offset_slot] - 2 + verbatim_bits
                };
                *r2 = *r1;
                *r1 = *r0;
                *r0 = match_offset;
            } else if match_offset_slot == 0 {
                match_offset = *r0;
            } else if match_offset_slot == 1 {
                match_offset = *r1;
                *r1 = *r0;
                *r0 = match_offset;
            } else {
                match_offset = *r2;
                *r2 = *r0;
                *r0 = match_offset;
            }
            if match_offset == 0 {
                return Err(LzxError::IllegalData);
            }
            // src_i == wp - match_offset at every iteration, so no separate counter is needed
            for _ in 0..match_length {
                let s = (wp as i64 - i64::from(match_offset)).rem_euclid(ws as i64) as usize;
                window[wp] = window[s];
                wp += 1;
            }
            this_run = this_run.saturating_sub(match_length);
        }
    }
    Ok(())
}
