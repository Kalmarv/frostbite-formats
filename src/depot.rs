//! ShaderBlockDepot resources (type 0x73312045): material key -> parameter block, and the block's parameters.
//!
//! Layout (relocatable): u64 header, u64 0, then `count` entries (u64 key, u64 descriptor offset); the blocks' data;
//! 16-aligned descriptors (u64 data offset, u64 block hash, u64 size); 16-aligned end; then the relocation list
//! (u32 offsets, zero-terminated). The 16-byte resource metadata holds (size, relocation size, version, count).

use crate::{Rd, Result, bad, capacity, u32_len};

const W: &str = "shader block depot";
/// The resource type id of a ShaderBlockDepot.
pub const RTYPE: u32 = 0x7331_2045;

/// A parameter block: its hash and its bytes (read its parameters with [`params`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub hash: u64,
    pub data: Vec<u8>,
}

/// A depot: the material key to block map, with the blocks it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Depot {
    pub head: u64,
    pub version: u32,
    /// (material key, block index), in stored order.
    pub entries: Vec<(u64, usize)>,
    pub blocks: Vec<Block>,
}

/// Read a depot from its payload `d` and the resource's 16-byte metadata. Refused: sizes that do not match the
/// payload, a descriptor or block outside the payload, blocks that together hold more bytes than the payload (the
/// blocks of a depot lie in the payload one after the other, so a payload cannot hold more bytes of blocks than its
/// own size; this bounds what the entries can make the reader copy).
pub fn read(d: &[u8], meta: &[u8; 16]) -> Result<Depot> {
    let m = Rd::new(meta, W);
    let (size, rsize, version, count) = (m.size32le(0)?, m.size32le(4)?, m.u32le(8)?, m.size32le(12)?);
    if size.checked_add(rsize) != Some(d.len()) {
        return Err(bad(
            W,
            0,
            format!("size {size} + relocations {rsize} != {}", d.len()),
        ));
    }
    let r = Rd::new(d, W);
    let mut dep = Depot {
        head: r.u64le(0)?,
        version,
        entries: Vec::with_capacity(capacity(W, count, 16, d.len().saturating_sub(0x10))?),
        blocks: Vec::new(),
    };
    let mut by_desc = std::collections::BTreeMap::new();
    let mut copied = 0usize;
    for i in 0..count {
        let entry = crate::at(0x10, i, 16);
        let (key, desc) = (r.u64le(entry)?, r.offset64le(entry.saturating_add(8))?);
        let bi = match by_desc.get(&desc) {
            Some(&bi) => bi,
            None => {
                let (off, hash, len) = (
                    r.offset64le(desc)?,
                    r.u64le(desc.saturating_add(8))?,
                    r.offset64le(desc.saturating_add(16))?,
                );
                copied = copied.saturating_add(len);
                if copied > d.len() {
                    return Err(bad(
                        W,
                        desc.saturating_add(16),
                        format!(
                            "the blocks hold {copied} bytes together, more than the payload's {}: expected each block's bytes to lie in the payload once",
                            d.len()
                        ),
                    ));
                }
                let bi = dep.blocks.len();
                dep.blocks.push(Block {
                    hash,
                    data: r.bytes(off, len)?.to_vec(),
                });
                by_desc.insert(desc, bi);
                bi
            }
        };
        dep.entries.push((key, bi));
    }
    Ok(dep)
}

/// The layout Studio writes: header, key table, blocks in first-use order packed, 16-aligned descriptors, 16-aligned
/// end, relocation list (per entry: its block's descriptor data pointer at the block's first use, then the entry's
/// descriptor pointer), zero-terminated. Returns (payload, metadata). An error when an entry names a block the depot
/// does not have, or an offset or count does not fit the 32 bits it is stored in.
pub fn write(dep: &Depot) -> Result<(Vec<u8>, [u8; 16])> {
    let count = dep.entries.len();
    let no_block = |bi: usize| {
        bad(
            W,
            bi,
            format!("an entry names block {bi} of {}", dep.blocks.len()),
        )
    };
    let mut order = Vec::new();
    for &(_, bi) in &dep.entries {
        if !order.contains(&bi) {
            order.push(bi);
        }
    }
    // where the blocks' data and the descriptors lie, before anything is written
    let mut data_at = std::collections::BTreeMap::new();
    let mut pos = crate::at(0x10, count, 16);
    for &bi in &order {
        data_at.insert(bi, pos);
        pos = pos.saturating_add(dep.blocks.get(bi).ok_or_else(|| no_block(bi))?.data.len());
    }
    let descriptors = crate::align(pos, 16);
    let desc_at: std::collections::BTreeMap<usize, usize> = (order.iter().enumerate())
        .map(|(k, &bi)| (bi, crate::at(descriptors, k, 24)))
        .collect();
    let desc_of = |bi: usize| desc_at.get(&bi).copied().ok_or_else(|| no_block(bi));
    let mut out = Vec::new();
    out.extend(dep.head.to_le_bytes());
    out.extend(0u64.to_le_bytes());
    let mut reloc = Vec::new();
    let mut done = std::collections::BTreeSet::new();
    for (i, &(key, bi)) in dep.entries.iter().enumerate() {
        out.extend(key.to_le_bytes());
        out.extend((desc_of(bi)? as u64).to_le_bytes());
        if done.insert(bi) {
            reloc.push(u32_len(W, desc_of(bi)?)?);
        }
        reloc.push(u32_len(W, crate::at(0x10, i, 16).saturating_add(8))?);
    }
    for &bi in &order {
        out.extend(&dep.blocks.get(bi).ok_or_else(|| no_block(bi))?.data);
    }
    out.resize(descriptors, 0);
    for &bi in &order {
        let b = dep.blocks.get(bi).ok_or_else(|| no_block(bi))?;
        out.extend((data_at.get(&bi).copied().ok_or_else(|| no_block(bi))? as u64).to_le_bytes());
        out.extend(b.hash.to_le_bytes());
        out.extend((b.data.len() as u64).to_le_bytes());
    }
    out.resize(crate::align(out.len(), 16), 0);
    let size = u32_len(W, out.len())?;
    reloc.push(0);
    for r in &reloc {
        out.extend(r.to_le_bytes());
    }
    let mut meta = [0u8; 16];
    let words = [
        size,
        u32_len(W, crate::at(0, reloc.len(), 4))?,
        dep.version,
        u32_len(W, count)?,
    ];
    for (slot, v) in meta.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        slot.copy_from_slice(&v.to_le_bytes());
    }
    Ok((out, meta))
}

/// Parameter value types and their element sizes; 0x14c5fc30 is a texture reference.
pub const TEXTURE: u32 = 0x14c5_fc30;
fn elem_size(ty: u32) -> Option<usize> {
    Some(match ty {
        0x2bb6_5c2d | 0x36ed_3444 | 0x8e4a_817e | 0x57f9_70ca | 0x7826_e998 => 4,
        0x885e_ff59 => 12,
        0x7f99_9829 => 16,
        0x2a36_e9d5 => 8,
        _ => return None,
    })
}

/// One parameter of a block: name hash (its top 16 bits name the texture slot), type, and where its value bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: u64,
    pub ty: u32,
    pub count: u16,
    pub extra: u16,
    /// Offset of the parameter record in the block, and its length.
    pub at: usize,
    pub len: usize,
}

impl Param {
    /// The texture slot (top 16 bits of the name hash) of a texture reference.
    pub fn slot(&self) -> u16 {
        u16::try_from(self.name >> 48).unwrap_or(u16::MAX)
    }
    /// A texture reference's 33 value bytes: flag, then two GUIDs (the second the texture asset's file GUID).
    pub fn texture_ref<'a>(&self, block: &'a [u8]) -> Option<&'a [u8]> {
        if self.ty != TEXTURE {
            return None;
        }
        block.get(self.at.saturating_add(16)..self.at.saturating_add(49))
    }
}

/// Every parameter of a block, walking to its exact end.
pub fn params(block: &[u8]) -> Result<Vec<Param>> {
    let r = Rd::new(block, "shader block");
    let n = r.size32le(0)?;
    let mut pos = 4usize;
    let mut out = Vec::with_capacity(capacity("shader block", n, 20, block.len().saturating_sub(4))?);
    for i in 0..n {
        let (name, ty, count, extra) = (
            r.u64le(pos)?,
            r.u32le(pos.saturating_add(8))?,
            r.u16le(pos.saturating_add(12))?,
            r.u16le(pos.saturating_add(14))?,
        );
        let len = if ty == TEXTURE {
            49
        } else {
            let e = elem_size(ty).ok_or_else(|| {
                bad(
                    "shader block",
                    pos,
                    format!("unknown type {ty:#010x} (parameter {i} of {n})"),
                )
            })?;
            let values = r.size32le(pos.saturating_add(16))?;
            crate::at(20, values, e)
        };
        r.bytes(pos, len)?;
        out.push(Param {
            name,
            ty,
            count,
            extra,
            at: pos,
            len,
        });
        pos = pos.saturating_add(len);
    }
    if pos != block.len() {
        return Err(bad(
            "shader block",
            pos,
            format!(
                "{} bytes after the last parameter, expected the block to end there",
                block.len().saturating_sub(pos)
            ),
        ));
    }
    Ok(out)
}
