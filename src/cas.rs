//! Frostbite CAS: a sequence of blocks, each an eight-byte header and an exactly sized payload ([`blocks`]), and the
//! codec that decodes a stream of them to its bytes ([`Cas`]), the Oodle blocks through the caller's handle to the
//! game's library.
use primase_foundations::compress::{Compressor, Decompressor, Error, Result, Zlib, oodle::Oodle};

pub(crate) fn invalid(offset: usize, detail: impl ToString) -> Error {
    Error::Invalid {
        codec: "cas",
        offset,
        detail: detail.to_string(),
    }
}
pub(crate) fn buffer(size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(size).map_err(|_| Error::Limit)?;
    out.resize(size, 0);
    Ok(out)
}
pub(crate) fn check_size(actual: usize, expected: usize) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Size { expected, actual })
    }
}
/// A borrowed block, including its physical header offset.
#[derive(Debug, Clone, Copy)]
pub struct Block<'a> {
    /// Where the block's header starts in the stream.
    pub offset: usize,
    /// The codec byte: 0 raw, 2 zlib, 0x11, 0x15 and 0x19 Oodle.
    pub kind: u8,
    /// The size of the block once decoded.
    pub decoded_size: usize,
    /// The stored bytes after the header.
    pub payload: &'a [u8],
}
/// Parse one complete CAS stream. Header: decoded size is BE u32 low 24 bits;
/// high byte is dictionary metadata (unsupported, rejected). Byte 4 low 7 bits
/// is codec; its high bit is reserved. Byte 5 low nibble supplies payload bits
/// 16..19; bytes 6..7 are BE payload low16. Byte 5 high nibble is a flags field,
/// not part of either length, and is 7 in every block of the stock install
/// (1,001,300) and of the 82 mod folders installed with it (1,535,187 blocks,
/// maps compiled by ReSkate Studio among them); any other value is refused.
pub fn blocks(input: &[u8]) -> Result<Vec<Block<'_>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < input.len() {
        let start = pos;
        let &[flag, d1, d2, d3, kind, s1, s2, s3] = input
            .get(pos..)
            .and_then(|rest| rest.first_chunk::<8>())
            .ok_or_else(|| invalid(pos, "short header: expected 8 bytes"))?;
        if flag != 0 || kind & 0x80 != 0 {
            return Err(invalid(
                pos,
                "dictionary/reserved flag unsupported: expected byte 0 and the top bit of byte 4 clear",
            ));
        }
        if s1 >> 4 != 7 {
            return Err(invalid(
                pos,
                "block flags unsupported: expected the high nibble of byte 5 to be 7",
            ));
        }
        let decoded_size = (usize::from(d1) << 16) | (usize::from(d2) << 8) | usize::from(d3);
        let size = (usize::from(s1 & 15) << 16) | (usize::from(s2) << 8) | usize::from(s3);
        pos = pos.saturating_add(8);
        let payload = input
            .get(pos..)
            .and_then(|rest| rest.get(..size))
            .ok_or_else(|| invalid(pos, format!("short payload: expected {size} bytes")))?;
        out.push(Block {
            offset: start,
            kind,
            decoded_size,
            payload,
        });
        pos = pos.saturating_add(size);
    }
    Ok(out)
}
pub(crate) fn append(out: &mut Vec<u8>, decoded: usize, kind: u8, payload: &[u8]) {
    // a block holds at most 64 KiB decoded and the stored size takes 20 bits: both fit the fields (the callers cut the
    // input into 64 KiB parts), and a larger value keeps its low bits as the format does
    out.extend_from_slice(&u32::try_from(decoded).unwrap_or(u32::MAX).to_be_bytes());
    let [.., high, mid, low] = u64::try_from(payload.len()).unwrap_or(u64::MAX).to_be_bytes();
    out.extend_from_slice(&[kind, 0x70 | (high & 0x0F)]);
    out.extend_from_slice(&[mid, low]);
    out.extend_from_slice(payload);
}
/// Encode raw 64 KiB blocks, including one empty raw block for empty input.
#[derive(Debug, Clone, Copy)]
pub struct CasRaw;
impl Compressor for CasRaw {
    fn compress(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if input.is_empty() {
            append(&mut out, 0, 0, &[]);
        }
        for part in input.chunks(65536) {
            append(&mut out, part.len(), 0, part);
        }
        Ok(out)
    }
}

/// CAS codec, optional explicit Oodle implementation. Decode supports raw, zlib,
/// and kinds 0x11/0x15/0x19. Unknown kinds are errors with physical offsets.
#[derive(Debug, Default)]
pub struct Cas<'a> {
    /// The Oodle library to decode (and encode) Oodle blocks with; blocks of the other kinds need none.
    pub oodle: Option<&'a Oodle>,
}
impl Decompressor for Cas<'_> {
    fn decompress(&self, input: &[u8], expected: usize) -> Result<Vec<u8>> {
        let blocks = blocks(input)?;
        let total = blocks
            .iter()
            .try_fold(0usize, |s, b| s.checked_add(b.decoded_size).ok_or(Error::Limit))?;
        check_size(total, expected)?;
        // a raw block is stored as it decodes: a header that claims more than its payload holds is refused before
        // anything is reserved
        if let Some(b) = blocks
            .iter()
            .find(|b| b.kind == 0 && b.payload.len() != b.decoded_size)
        {
            return Err(Error::Size {
                expected: b.decoded_size,
                actual: b.payload.len(),
            });
        }
        let mut out = buffer(expected)?;
        let mut rest = out.as_mut_slice();
        for b in blocks {
            let (dst, after) = std::mem::take(&mut rest)
                .split_at_mut_checked(b.decoded_size)
                .ok_or_else(|| invalid(b.offset, "block decodes past the stream's decoded size"))?;
            rest = after;
            match b.kind {
                0 => {
                    check_size(b.payload.len(), b.decoded_size)?;
                    dst.copy_from_slice(b.payload);
                }
                2 => dst.copy_from_slice(
                    &Zlib
                        .decompress(b.payload, b.decoded_size)
                        .map_err(|e| invalid(b.offset, e))?,
                ),
                0x11 | 0x15 | 0x19 => {
                    let oodle = self
                        .oodle
                        .ok_or_else(|| invalid(b.offset, "Oodle library required"))?;
                    dst.copy_from_slice(
                        &oodle
                            .decompress(b.payload, b.decoded_size)
                            .map_err(|e| invalid(b.offset, e))?,
                    );
                }
                _ => {
                    return Err(invalid(b.offset, format!("unsupported kind {:#x}", b.kind)));
                }
            }
        }
        Ok(out)
    }
}
impl Compressor for Cas<'_> {
    /// Kraken level 4 in independent 64 KiB blocks; uses stored blocks when
    /// compression is no smaller (the reference CAS writer policy).
    /// Requires an Oodle handle explicitly; absent DLL is not a silent fallback.
    fn compress(&self, input: &[u8]) -> Result<Vec<u8>> {
        let oodle = self
            .oodle
            .ok_or_else(|| invalid(0, "Oodle library required for encoding"))?;
        let mut out = Vec::new();
        if input.is_empty() {
            append(&mut out, 0, 0, &[]);
        }
        for part in input.chunks(65536) {
            let payload = oodle.compress(part)?;
            if payload.len() < part.len() {
                append(&mut out, part.len(), 0x11, &payload);
            } else {
                append(&mut out, part.len(), 0, part);
            }
        }
        Ok(out)
    }
}
