//! A bundle: its region in the TOC (the file table: where each of its files lives) and its binary manifest (the
//! first file: names, SHA-1s and sizes of the bundle's EBX, resources and chunks, plus the chunk metadata DB).

use crate::toc::Loc;
use crate::{Rd, Result, bad, capacity, latin1, u32_len};

const R: &str = "bundle region";
const M: &str = "bundle manifest";
/// The salt a manifest's magic word is xored with.
pub const BUNDLE_SALT: u32 = 0x7065_636E;
/// The manifest magic (before the salt is applied).
pub const BUNDLE_MAGIC: u32 = 0xED1C_EDB8;

/// One file of a bundle: archive, offset, stored size.
pub type FileRef = (Loc, u32, u32);

/// The region's file table (archive, offset and stored size of each file, the manifest first when it is not inline) and
/// its inline manifest bytes (empty when the region has none).
///
/// Refused: a header whose counts are negative or disagree, a table or inline manifest outside the bytes, and a first
/// file with no archive identifier.
pub fn read_region(r: &[u8]) -> Result<(Vec<FileRef>, Vec<u8>)> {
    let rd = Rd::new(r, R);
    let int = |i: usize| rd.i32be(crate::at(0, i, 4));
    let (mo, ms, lo, total, dato, ro, co, total2) = (
        int(0)?,
        int(1)?,
        rd.u32be(8)?,
        int(3)?,
        rd.u32be(16)?,
        rd.u32be(20)?,
        rd.u32be(24)?,
        int(8)?,
    );
    let (Ok(mo), Ok(ms), Ok(total)) = (usize::try_from(mo), usize::try_from(ms), usize::try_from(total))
    else {
        return Err(bad(
            R,
            0,
            "header is invalid: expected a metadata offset, metadata size and file count of 0 or more",
        ));
    };
    if total2 < 0 || usize::try_from(total2) != Ok(total) || ro != dato || co != dato {
        return Err(bad(
            R,
            0,
            "header is invalid: expected the two file counts equal and the three data offsets equal",
        ));
    }
    let inline = if ms > 0 {
        rd.bytes(mo, ms)?.to_vec()
    } else {
        Vec::new()
    };
    // each file has a flag byte and an 8-byte offset and size
    let mut files = Vec::with_capacity(capacity(R, total, 9, r.len())?);
    let mut pos = dato as usize;
    let mut cur: Option<Loc> = None;
    for i in 0..total {
        let flag = rd.u8(crate::at(lo as usize, i, 1))?;
        if flag == 1 {
            let enc = rd.u32be(pos)?;
            pos = pos.saturating_add(4);
            cur = Some(Loc {
                patch: enc & 0x00FF_0000 != 0,
                chunk: (enc >> 8) & 0xFF,
                archive: u16::from(enc.to_le_bytes()[0]),
            });
        } else if flag != 0 {
            let id = rd.u64be(pos)?;
            cur = Some(Loc::decode(id).ok_or_else(|| bad(R, pos, crate::toc::unrepresentable(id)))?);
            pos = pos.saturating_add(8);
        }
        let loc = cur.ok_or_else(|| bad(R, pos, "first file has no archive identifier"))?;
        files.push((loc, rd.u32be(pos)?, rd.u32be(pos.saturating_add(4))?));
        pos = pos.saturating_add(8);
    }
    Ok((files, inline))
}

/// The counterpart of [`read_region`] for the layout Studio and ReSkate write (8-byte identifiers).
///
/// An error when the file table or the inline manifest is larger than the 32-bit offsets and counts it is stored with.
pub fn write_region(files: &[FileRef], inline: &[u8]) -> Result<Vec<u8>> {
    let mut table = Vec::new();
    let mut flags = Vec::with_capacity(files.len());
    let mut prev = None;
    for &(loc, off, size) in files {
        if prev == Some(loc) {
            flags.push(0u8);
        } else {
            flags.push(0x80);
            table.extend(loc.encode().to_be_bytes());
            prev = Some(loc);
        }
        table.extend(off.to_be_bytes());
        table.extend(size.to_be_bytes());
    }
    if let Some(f) = flags.first_mut() {
        *f |= 0x04;
    }
    let data_at = 0x24u32;
    let too_large = || {
        bad(
            R,
            0,
            "the file table and inline manifest do not fit 32-bit offsets",
        )
    };
    let table_end = u32_len(R, table.len())?
        .checked_add(data_at)
        .ok_or_else(too_large)?;
    let inline_len = u32_len(R, inline.len())?;
    let meta_at = if inline.is_empty() { 0 } else { table_end };
    let loc_at = if inline.is_empty() {
        table_end
    } else {
        table_end.checked_add(inline_len).ok_or_else(too_large)?
    };
    let n = u32_len(R, files.len())?;
    let mut out = Vec::with_capacity(
        table
            .len()
            .saturating_add(inline.len())
            .saturating_add(flags.len())
            .saturating_add(0x24),
    );
    for v in [meta_at, inline_len, loc_at, n, data_at, data_at, data_at, 0, n] {
        out.extend(v.to_be_bytes());
    }
    out.extend(table);
    out.extend(inline);
    out.extend(flags);
    Ok(out)
}

/// What a manifest row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An EBX document.
    Ebx,
    /// A resource (a MeshSet, a texture header, a shader resource...), identified by a type id and a 64-bit resource id.
    Res,
    /// A chunk, identified by a GUID (texture pixels, geometry, ...).
    Chunk,
}

/// One manifest row. EBX and resources have a name and original size; resources a type, 16 metadata bytes and an id;
/// chunks a GUID and a logical offset / size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// Which table the row came from.
    pub kind: Kind,
    /// Asset path (EBX and resources); empty for chunks, which have a GUID instead.
    pub name: String,
    /// SHA-1 of the stored bytes, from the manifest's hash table (one per asset, EBX then resources then chunks).
    pub sha1: [u8; 20],
    /// Uncompressed size in bytes. For a chunk it is [`Asset::lsize`], the size of the stored range, as the manifest
    /// has no separate field for it; the whole chunk's size is [`Asset::chunk_size`].
    pub size: u32,
    /// Resource type id (resources only; 0 otherwise).
    pub rtype: u32,
    /// The 16 metadata bytes stored with a resource (resources only), kept as stored.
    pub rmeta: [u8; 16],
    /// 64-bit resource id (resources only).
    pub rid: u64,
    /// Chunk GUID (chunks only), with the byte order of the TOC's chunk table.
    pub guid: [u8; 16],
    /// Chunk logical offset (chunks only): the 32-bit value stored in the chunk record. It is where the stored range
    /// begins inside the whole chunk, a multiple of 65,536 (it is not zero for a texture whose first mips are left
    /// out of the bundle).
    pub loff: u32,
    /// Chunk logical size (chunks only): the 32-bit value stored in the chunk record, the decoded size of the range
    /// the bundle stores.
    pub lsize: u32,
}

impl Asset {
    /// The size of the whole chunk (chunks only): the stored range's [`Asset::loff`] plus its [`Asset::lsize`].
    pub fn chunk_size(&self) -> u64 {
        u64::from(self.loff).saturating_add(u64::from(self.lsize))
    }

    fn new(kind: Kind) -> Self {
        Self {
            kind,
            name: String::new(),
            sha1: [0; 20],
            size: 0,
            rtype: 0,
            rmeta: [0; 16],
            rid: 0,
            guid: [0; 16],
            loff: 0,
            lsize: 0,
        }
    }
}

/// A bundle's binary manifest: its EBX documents, resources and chunks, and the chunk metadata DB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The bundle's EBX documents, in manifest order.
    pub ebx: Vec<Asset>,
    /// The bundle's resources, in manifest order.
    pub res: Vec<Asset>,
    /// The bundle's chunks, in manifest order.
    pub chunks: Vec<Asset>,
    /// The chunk metadata DB, as stored.
    pub meta: Vec<u8>,
    /// Whether the manifest was stored big-endian (its magic word read that way). [`write_manifest`] writes only little-endian.
    pub big_endian: bool,
}

impl Manifest {
    /// The number of assets (EBX documents, resources and chunks).
    pub fn count(&self) -> usize {
        self.ebx
            .len()
            .saturating_add(self.res.len())
            .saturating_add(self.chunks.len())
    }
    /// Index in the region's file list (manifest first) of resource `i`.
    pub fn res_file(&self, i: usize) -> usize {
        self.ebx.len().saturating_add(i).saturating_add(1)
    }
    /// Index in the region's file list of chunk `i`.
    pub fn chunk_file(&self, i: usize) -> usize {
        self.ebx
            .len()
            .saturating_add(self.res.len())
            .saturating_add(i)
            .saturating_add(1)
    }
}

/// Whether `m` has a manifest's header (declared size and salted magic, either endianness).
pub fn is_manifest(m: &[u8]) -> bool {
    let rd = Rd::new(m, M);
    let (Ok(declared), Ok(be), Ok(le)) = (rd.u32be(0), rd.u32be(4), rd.u32le(4)) else {
        return false;
    };
    m.len() >= 36
        && usize::try_from(declared).ok().and_then(|d| d.checked_add(4)) == Some(m.len())
        && (be ^ BUNDLE_SALT == BUNDLE_MAGIC || le ^ BUNDLE_SALT == BUNDLE_MAGIC)
}

/// Read a bundle manifest to its exact end. Refused: a truncated header, a declared size that is not the data's,
/// an unknown magic, counts that do not add up, a string or table outside the data.
pub fn read_manifest(m: &[u8]) -> Result<Manifest> {
    let rd = Rd::new(m, M);
    if m.len() < 36 {
        return Err(bad(M, 0, "header is truncated"));
    }
    let declared = rd.u32be(0)?;
    if usize::try_from(declared).ok().and_then(|d| d.checked_add(4)) != Some(m.len()) {
        return Err(bad(
            M,
            0,
            format!(
                "declared size {declared} does not match: expected {}, the bytes after the size word",
                m.len().saturating_sub(4)
            ),
        ));
    }
    let big_endian = rd.u32be(4)? ^ BUNDLE_SALT == BUNDLE_MAGIC;
    if !big_endian && rd.u32le(4)? ^ BUNDLE_SALT != BUNDLE_MAGIC {
        return Err(bad(M, 4, "magic is unsupported"));
    }
    let u32e = |at: usize| if big_endian { rd.u32be(at) } else { rd.u32le(at) };
    let u64e = |at: usize| if big_endian { rd.u64be(at) } else { rd.u64le(at) };
    let size = |at: usize| u32e(at).map(|v| v as usize);
    let (total, ne, nr, nc) = (size(8)?, size(12)?, size(16)?, size(20)?);
    let (strings, mo, ms) = (size(24)?.saturating_add(4), size(28)?, size(32)?);
    if ne.checked_add(nr).and_then(|n| n.checked_add(nc)) != Some(total) {
        return Err(bad(
            M,
            8,
            format!(
                "asset counts do not add up: {ne} EBX + {nr} resources + {nc} chunks, expected the total {total}"
            ),
        ));
    }
    let mut pos = 36usize;
    let mut hashes = Vec::with_capacity(capacity(M, total, 20, m.len().saturating_sub(36))?);
    for _ in 0..total {
        hashes.push(rd.arr::<20>(pos)?);
        pos = pos.saturating_add(20);
    }
    let mut b = Manifest {
        ebx: Vec::new(),
        res: Vec::new(),
        chunks: Vec::new(),
        meta: Vec::new(),
        big_endian,
    };
    for (n, kind) in [(ne, Kind::Ebx), (nr, Kind::Res)] {
        for _ in 0..n {
            let mut a = Asset::new(kind);
            a.name = rd.cstr(strings.saturating_add(size(pos)?))?;
            a.size = u32e(pos.saturating_add(4))?;
            pos = pos.saturating_add(8);
            if kind == Kind::Ebx {
                b.ebx.push(a)
            } else {
                b.res.push(a)
            }
        }
    }
    // `hashes` holds one per asset, EBX documents then resources then chunks
    for (a, hash) in b.ebx.iter_mut().chain(b.res.iter_mut()).zip(&hashes) {
        a.sha1 = *hash;
    }
    for a in &mut b.res {
        a.rtype = u32e(pos)?;
        pos = pos.saturating_add(4);
    }
    for a in &mut b.res {
        a.rmeta = rd.arr(pos)?;
        pos = pos.saturating_add(16);
    }
    for a in &mut b.res {
        a.rid = u64e(pos)?;
        pos = pos.saturating_add(8);
    }
    let chunk_hashes = hashes.iter().skip(ne.saturating_add(nr));
    for hash in chunk_hashes.take(nc) {
        let mut a = Asset::new(Kind::Chunk);
        a.guid = rd.arr(pos)?;
        if big_endian {
            a.guid.reverse();
        }
        a.loff = u32e(pos.saturating_add(16))?;
        a.lsize = u32e(pos.saturating_add(20))?;
        pos = pos.saturating_add(24);
        a.sha1 = *hash;
        a.size = a.lsize;
        b.chunks.push(a);
    }
    if ms > 0 {
        b.meta = rd.bytes(mo.saturating_add(4), ms)?.to_vec();
    }
    Ok(b)
}

/// The manifest as little-endian bytes, the form Studio and ReSkate write; [`read_manifest`] reads it back.
///
/// An error when `b.big_endian` is set, a name is not Latin-1, or a count or offset exceeds 32 bits.
pub fn write_manifest(b: &Manifest) -> Result<Vec<u8>> {
    if b.big_endian {
        return Err(bad(M, 0, "only little-endian manifests are written"));
    }
    let total = b.count();
    let mut strings = Vec::new();
    let mut offsets = Vec::with_capacity(b.ebx.len().saturating_add(b.res.len()));
    for a in b.ebx.iter().chain(&b.res) {
        offsets.push(u32_len(M, strings.len())?);
        strings.extend(latin1(&a.name, M)?);
        strings.push(0);
    }
    // the tables before the strings: header, SHA-1s, (name offset, size) pairs, resource records, chunk records
    let assets_with_names = b.ebx.len().saturating_add(b.res.len());
    let string_at = 36usize
        .saturating_add(crate::at(0, total, 20))
        .saturating_add(crate::at(0, assets_with_names, 8))
        .saturating_add(crate::at(0, b.res.len(), 28))
        .saturating_add(crate::at(0, b.chunks.len(), 24));
    let meta_at = string_at.saturating_add(strings.len());
    let len = meta_at.saturating_add(b.meta.len());
    // the stored offsets count from the byte after the size word
    let after_size = |at: usize| u32_len(M, at.saturating_sub(4));
    let mut out = Vec::with_capacity(len);
    out.extend(after_size(len)?.to_be_bytes());
    out.extend((BUNDLE_MAGIC ^ BUNDLE_SALT).to_le_bytes());
    let meta_field = if b.meta.is_empty() {
        0
    } else {
        after_size(meta_at)?
    };
    for v in [
        u32_len(M, total)?,
        u32_len(M, b.ebx.len())?,
        u32_len(M, b.res.len())?,
        u32_len(M, b.chunks.len())?,
        after_size(string_at)?,
        meta_field,
        u32_len(M, b.meta.len())?,
    ] {
        out.extend(v.to_le_bytes());
    }
    for a in b.ebx.iter().chain(&b.res).chain(&b.chunks) {
        out.extend(a.sha1);
    }
    for (a, off) in b.ebx.iter().chain(&b.res).zip(&offsets) {
        out.extend(off.to_le_bytes());
        out.extend(a.size.to_le_bytes());
    }
    for a in &b.res {
        out.extend(a.rtype.to_le_bytes());
    }
    for a in &b.res {
        out.extend(a.rmeta);
    }
    for a in &b.res {
        out.extend(a.rid.to_le_bytes());
    }
    for a in &b.chunks {
        out.extend(a.guid);
        out.extend(a.loff.to_le_bytes());
        out.extend(a.lsize.to_le_bytes());
    }
    out.extend(strings);
    out.extend(&b.meta);
    debug_assert_eq!(out.len(), len);
    Ok(out)
}
