//! Superbundle TOC (`*.toc`): a 0x22C-byte envelope, then a big-endian header, the bundles (name, inline region)
//! and the chunk table, each found through a perfect hash over its keys (lowercase bundle names, chunk GUIDs).
//! Base-game TOCs store names Huffman-coded (flag 4); Patch TOCs (the ones in a folder of added or replaced files,
//! as ReSkate Studio writes it beside the game's `Data`) store them as plain strings, which is what [`write()`] writes.

use crate::{Error, Rd, Result, align, bad, latin1};

/// Length of the envelope that precedes a TOC's (or `layout.toc`'s) own data.
pub const ENVELOPE: usize = 0x22C;
/// The four bytes an envelope starts with.
pub const MAGIC: [u8; 4] = [0x00, 0xd1, 0xce, 0x01];
const FNV_SEED: u32 = 0x811C_9DC5;
const W: &str = "toc";
/// A Huffman table entry with this bit set is a leaf (`!char`); one without it names an internal node by its counter.
const LEAF: u32 = 0x8000_0000;

/// The bits of a TOC's flags word (header word 11).
pub mod flags {
    /// The base game's TOC of the same superbundle has bundles this one does not list: [`merge`](super::merge) takes them.
    /// Set, with [`HAS_BASE_CHUNKS`], in 199 of the 245 TOCs of the installed mods (flags word 3; the other 46 are 0, and
    /// 30 of the 199 have no base TOC in the stock data); the stock TOCs hold 4.
    pub const HAS_BASE_BUNDLES: u32 = 1;
    /// The base game's TOC has chunks this one does not list.
    pub const HAS_BASE_CHUNKS: u32 = 2;
    /// Bundle names are Huffman coded (the base game's TOCs).
    pub const HUFFMAN_NAMES: u32 = 4;
}

/// Where a file lives: (patch folder or base game, install chunk, archive number).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Loc {
    /// Whether the file is in a Patch folder (the added or replaced files) rather than the base game: bits 48..56 of
    /// the stored identifier, 1 for the Patch folder and 0 for the base game.
    pub patch: bool,
    /// Install chunk number: bits 16..48 of the stored identifier.
    pub chunk: u32,
    /// Archive number within the chunk: the low 16 bits of the stored identifier.
    pub archive: u16,
}

impl Loc {
    /// A `Loc` from the 64-bit identifier a TOC or region stores, or `None` when the identifier holds something a
    /// `Loc` can't represent (bits 48..56 other than 0 or 1, or any of bits 56..64 set), so that
    /// [`encode`](Self::encode) gives back exactly the stored identifier.
    pub fn decode(v: u64) -> Option<Self> {
        let [a0, a1, c0, c1, c2, c3, patch, top] = v.to_le_bytes();
        (patch <= 1 && top == 0).then(|| Self {
            patch: patch == 1,
            chunk: u32::from_le_bytes([c0, c1, c2, c3]),
            archive: u16::from_le_bytes([a0, a1]),
        })
    }
    /// The 64-bit identifier a TOC or region stores for this `Loc`.
    pub fn encode(self) -> u64 {
        (u64::from(self.patch) << 48) | (u64::from(self.chunk) << 16) | u64::from(self.archive)
    }
}

/// The refusal text for an archive identifier [`Loc::decode`] can't represent.
pub(crate) fn unrepresentable(id: u64) -> String {
    format!(
        "archive identifier {id:#018x}: expected bits 48..56 (the patch flag) to be 0 or 1 and bits 56..64 to be 0"
    )
}

/// A chunk row of a TOC: GUID (as stored in bundles: little endian) and its file, or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocChunk {
    /// The chunk's GUID in the byte order bundle manifests use (the TOC stores it reversed).
    pub guid: [u8; 16],
    /// `None` for a removed chunk.
    pub file: Option<(Loc, u32, u32)>,
}

/// A superbundle TOC as stored: its flags, its bundles (name and inline region bytes), its chunk table and the
/// perfect-hash maps over the bundle names and chunk GUIDs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toc {
    /// The TOC's flags word (header word 11). Bit 2 (value 4) means names are Huffman-coded, as in base-game TOCs; the rest are kept as stored.
    pub flags: u32,
    /// (name, inline bundle region bytes), in file order.
    pub bundles: Vec<(String, Vec<u8>)>,
    /// Where each chunk lives, in the order the TOC lists them; `None` for a removed chunk (stored as `-1`).
    pub chunks: Vec<TocChunk>,
    /// The perfect-hash table over the lowercased bundle names: one `i32` per bundle, a positive seed or `!slot` (see [`perfect_hash`]).
    pub bundle_hash: Vec<i32>,
    /// The perfect-hash table over the chunk GUIDs, in the same form; empty when the TOC lists no chunks.
    pub chunk_hash: Vec<i32>,
}

impl Toc {
    /// Whether the flags word says the base game's TOC lists bundles this one lacks ([`flags::HAS_BASE_BUNDLES`]).
    pub fn has_base_bundles(&self) -> bool {
        self.flags & flags::HAS_BASE_BUNDLES != 0
    }
    /// Whether the flags word says the base game's TOC lists chunks this one lacks ([`flags::HAS_BASE_CHUNKS`]).
    pub fn has_base_chunks(&self) -> bool {
        self.flags & flags::HAS_BASE_CHUNKS != 0
    }
}

/// The TOC a patch TOC describes together with the base TOC it builds on: what the game sees of the superbundle.
///
/// - With [`flags::HAS_BASE_BUNDLES`] the base's bundles stay, except those the patch lists again (matched by lowercase
///   name: the patch's replaces the base's, wherever it stood); the patch's follow. Without it only the patch's bundles
///   are kept.
/// - With [`flags::HAS_BASE_CHUNKS`] the base's chunks stay, except those the patch lists again (matched by GUID); a
///   patch chunk stored as removed (`None`) drops the base's and is not listed; the patch's others follow. Without it
///   the patch's chunks are kept as they are.
///
/// The merged TOC keeps the patch's flags word minus the base bits and the Huffman bit, so it reads as a TOC that
/// stands alone. Its entries are in the order of the perfect hash [`write`] lays out ([`perfect_hash`] over the merged
/// keys), so [`verify`] holds and the TOC writes and reads back as it is. An error when the
/// merged keys are not unique (a duplicate name that differs only in case within the patch or the base).
pub fn merge(base: &Toc, patch: &Toc) -> Result<Toc> {
    let key = |name: &str| name.to_lowercase();
    let mut bundles: Vec<(String, Vec<u8>)> = Vec::new();
    if patch.has_base_bundles() {
        let replaced: std::collections::BTreeSet<String> =
            patch.bundles.iter().map(|(n, _)| key(n)).collect();
        bundles.extend(
            base.bundles
                .iter()
                .filter(|(n, _)| !replaced.contains(&key(n)))
                .cloned(),
        );
    }
    bundles.extend(patch.bundles.iter().cloned());
    let mut chunks: Vec<TocChunk> = Vec::new();
    if patch.has_base_chunks() {
        let overridden: std::collections::BTreeSet<[u8; 16]> = patch.chunks.iter().map(|c| c.guid).collect();
        chunks.extend(
            base.chunks
                .iter()
                .filter(|c| !overridden.contains(&c.guid))
                .cloned(),
        );
    }
    chunks.extend(
        patch
            .chunks
            .iter()
            .filter(|c| c.file.is_some() || !patch.has_base_chunks())
            .cloned(),
    );
    let bkeys = bundles
        .iter()
        .map(|(n, _)| bundle_key(n))
        .collect::<Result<Vec<_>>>()?;
    let ckeys: Vec<Vec<u8>> = chunks.iter().map(|c| c.guid.to_vec()).collect();
    let (bundle_hash, border) = perfect_hash(&bkeys)?;
    let (chunk_hash, corder) = perfect_hash(&ckeys)?;
    // a TOC lists its entries in the order of the perfect hash's slots: the stored tables index that order
    let in_order = |order: &[usize], n: usize| -> Result<Vec<usize>> {
        (order.len() == n && order.iter().all(|&i| i < n))
            .then(|| order.to_vec())
            .ok_or_else(|| bad(W, 0, "perfect-hash order does not cover the merged entries"))
    };
    let (border, corder) = (
        in_order(&border, bundles.len())?,
        in_order(&corder, chunks.len())?,
    );
    let merged = Toc {
        flags: patch.flags & !(flags::HAS_BASE_BUNDLES | flags::HAS_BASE_CHUNKS | flags::HUFFMAN_NAMES),
        bundle_hash,
        chunk_hash,
        bundles: border.iter().filter_map(|&i| bundles.get(i).cloned()).collect(),
        chunks: corder.iter().filter_map(|&i| chunks.get(i).cloned()).collect(),
    };
    Ok(merged)
}

/// Huffman-coded names: leaves are `!char` values (top bit set), internal nodes counters, read in pairs (left, right).
struct Huffman {
    words: Vec<u32>,
    nodes: Vec<(u32, Option<(usize, usize)>)>,
    /// `None` for an empty table (a TOC without names).
    root: Option<usize>,
}

impl Huffman {
    fn read(r: &Rd, names_at: usize, names_n: usize, table_at: usize, table_n: usize) -> Result<Self> {
        let words = (0..names_n)
            .map(|i| r.u32be(crate::at(names_at, i, 4)))
            .collect::<Result<Vec<_>>>()?;
        let mut nodes: Vec<(u32, Option<(usize, usize)>)> = Vec::new();
        // A table entry is a leaf (`!char`: the top bit set) or the counter of an internal node built before it, so the two
        // kinds of value are disjoint and each has its own map: a stray small value cannot become a leaf that shadows an
        // internal node of the same number.
        let mut leaves = std::collections::BTreeMap::new();
        let mut internal = std::collections::BTreeMap::new();
        let (mut left, mut counter, mut root) = (None, 0u32, None);
        for i in 0..table_n {
            let at = crate::at(table_at, i, 4);
            let v = r.u32be(at)?;
            let node = if v & LEAF != 0 {
                *leaves.entry(v).or_insert_with(|| {
                    nodes.push((v, None));
                    nodes.len().saturating_sub(1)
                })
            } else {
                *internal.get(&v).ok_or_else(|| {
                    bad(
                        W,
                        at,
                        format!(
                            "Huffman entry {v:#x} is neither a character nor an internal node built before it"
                        ),
                    )
                })?
            };
            let Some(l) = left.take() else {
                left = Some(node);
                continue;
            };
            nodes.push((counter, Some((l, node))));
            let parent = nodes.len().saturating_sub(1);
            internal.insert(counter, parent);
            root = Some(parent);
            counter = counter.saturating_add(1);
        }
        Ok(Self { words, nodes, root })
    }

    /// The name whose code starts at bit `bit` of the name words.
    fn decode(&self, mut bit: usize) -> Result<String> {
        let nbits = self.words.len().saturating_mul(32);
        let mut out = String::new();
        let root = self
            .root
            .ok_or_else(|| bad(W, bit / 8, "a name in a TOC with an empty Huffman table"))?;
        let node = |n: usize, bit: usize| {
            self.nodes
                .get(n)
                .ok_or_else(|| bad(W, bit / 8, format!("Huffman node {n} of {}", self.nodes.len())))
        };
        while bit < nbits {
            let mut n = root;
            while let (_, Some((l, r))) = *node(n, bit)? {
                let word = self
                    .words
                    .get(bit >> 5)
                    .ok_or_else(|| bad(W, bit / 8, "Huffman name runs past the data"))?;
                let one = (word >> (bit & 31)) & 1 != 0;
                n = if one { r } else { l };
                bit = bit.saturating_add(1);
                if bit > nbits {
                    return Err(bad(W, bit / 8, "Huffman name runs past the data"));
                }
            }
            let ch = !node(n, bit)?.0 & 0xFFFF;
            if ch == 0 {
                return Ok(out);
            }
            out.push(char::from_u32(ch).ok_or_else(|| bad(W, bit / 8, "Huffman character is not a char"))?);
        }
        Err(bad(W, bit / 8, "Huffman name is unterminated"))
    }
}

/// Read a TOC (envelope included). Refused: a wrong envelope, a negative count, a table or string outside the
/// file, a bundle region that is not inline, a Huffman table or name that does not decode, a chunk reference that
/// does not resolve. An error's offset is into `data`, the envelope included.
pub fn read(data: &[u8]) -> Result<Toc> {
    match data.split_at_checked(ENVELOPE) {
        Some((envelope, body)) if body.len() >= 48 && envelope.starts_with(&MAGIC) => read_body(body)
            .map_err(|e| match e {
                Error::Format { what, offset, detail } => Error::Format {
                    what,
                    offset: offset.saturating_add(ENVELOPE),
                    detail,
                },
                other => other,
            }),
        _ => Err(bad(
            W,
            0,
            format!(
                "envelope is invalid: expected the magic {MAGIC:02x?} and {} bytes at least, got {} bytes",
                ENVELOPE + 48,
                data.len()
            ),
        )),
    }
}

/// The TOC after its envelope; error offsets are into `data`.
fn read_body(data: &[u8]) -> Result<Toc> {
    let r = Rd::new(data, W);
    let u = |i: usize| r.u32be(crate::at(0, i, 4));
    let (bho, bdo, cho, cgo, nameso, cdo, flags) = (
        u(0)? as usize,
        u(1)? as usize,
        u(3)? as usize,
        u(4)? as usize,
        u(8)? as usize,
        u(9)? as usize,
        u(11)?,
    );
    let count = |i: usize| -> Result<usize> {
        let at = crate::at(0, i, 4);
        usize::try_from(r.i32be(at)?).map_err(|_| bad(W, at, "count is negative, expected 0 or more"))
    };
    let (bcount, ccount, dcount) = (count(2)?, count(5)?, count(10)?);
    let huffman = if flags & 4 != 0 {
        let (n, t, at) = (
            r.u32be(48)? as usize,
            r.u32be(52)? as usize,
            r.u32be(56)? as usize,
        );
        Some(Huffman::read(&r, nameso, n, at, t)?)
    } else {
        None
    };
    let mut toc = Toc {
        flags,
        bundles: Vec::new(),
        chunks: Vec::new(),
        bundle_hash: Vec::new(),
        chunk_hash: Vec::new(),
    };
    toc.bundle_hash = (0..bcount)
        .map(|i| r.i32be(crate::at(bho, i, 4)))
        .collect::<Result<_>>()?;
    for i in 0..bcount {
        let at = crate::at(bdo, i, 16);
        let (no, sf, ro) = (
            r.u32be(at)? as usize,
            r.u32be(at.saturating_add(4))?,
            r.u64be(at.saturating_add(8))?,
        );
        if sf >> 30 != 1 {
            return Err(bad(
                W,
                at.saturating_add(4),
                format!("bundle region flags {sf:#x}: expected the inline form (bits 31..30 equal to 0b01)"),
            ));
        }
        let name = match &huffman {
            Some(h) => h.decode(no)?,
            None => r.cstr(nameso.saturating_add(no))?,
        };
        let ro = usize::try_from(ro).map_err(|_| {
            bad(
                W,
                at.saturating_add(8),
                format!("region offset {ro:#x} does not fit the address space"),
            )
        })?;
        toc.bundles
            .push((name, r.bytes(ro, (sf & 0x3FFF_FFFF) as usize)?.to_vec()));
    }
    if ccount != 0 {
        toc.chunk_hash = (0..ccount)
            .map(|i| r.i32be(crate::at(cho, i, 4)))
            .collect::<Result<_>>()?;
        let words = (0..dcount)
            .map(|i| r.u32be(crate::at(cdo, i, 4)))
            .collect::<Result<Vec<_>>>()?;
        for i in 0..ccount {
            let at = crate::at(cgo, i, 20);
            let mut guid: [u8; 16] = r.arr(at)?;
            guid.reverse();
            let ref_at = at.saturating_add(16);
            let enc = r.i32be(ref_at)?.cast_unsigned();
            if enc == u32::MAX {
                toc.chunks.push(TocChunk { guid, file: None });
                continue;
            }
            let (flag, w) = (enc >> 24, (enc & 0xFF_FFFF) as usize);
            let record = (flag == 0x80)
                .then(|| words.get(w..).and_then(|rest| rest.first_chunk::<4>()))
                .flatten();
            let Some(&[loc_high, loc_low, offset, size]) = record else {
                return Err(bad(
                    W,
                    ref_at,
                    format!(
                        "chunk data reference {enc:#010x}: expected -1 or 0x80 and the index of a 4-word record among the {} data words",
                        words.len()
                    ),
                ));
            };
            let id = (u64::from(loc_high) << 32) | u64::from(loc_low);
            let loc = Loc::decode(id).ok_or_else(|| bad(W, crate::at(cdo, w, 4), unrepresentable(id)))?;
            toc.chunks.push(TocChunk {
                guid,
                file: Some((loc, offset, size)),
            });
        }
    }
    Ok(toc)
}

/// The TOC's FNV-1: multiply by 0x01000193, xor the sign-extended byte, wrapping; reduced modulo 0x01000193 once at
/// the end. The tables' default seed is 0x811C9DC5.
fn hash_with(key: &[u8], seed: u32) -> u32 {
    key.iter().fold(seed, |h, &b| {
        h.wrapping_mul(0x01000193) ^ (b as i8 as i32).cast_unsigned()
    }) % 0x01000193
}

/// The slot of `key` among `size` (not 0) under `seed`: its hash modulo the size.
fn slot(key: &[u8], seed: u32, size: usize) -> usize {
    usize::try_from(hash_with(key, seed))
        .ok()
        .and_then(|h| h.checked_rem(size))
        .unwrap_or(0)
}

/// The perfect hash the TOC stores over `keys`: (hash map, key index per slot). Mirrors the game tools: buckets by
/// first hash, the multi-key buckets largest first (stable) get a seed that separates them, single keys fill free
/// slots in order, encoded as `-slot - 1`. An error when the keys are not unique, or when the table is too large for
/// the 32-bit entries it is stored in.
pub fn perfect_hash(keys: &[Vec<u8>]) -> Result<(Vec<i32>, Vec<usize>)> {
    let size = keys.len();
    let mut map = vec![-1i32; size];
    let mut order = vec![usize::MAX; size];
    if size == 0 {
        return Ok((map, order));
    }
    let mut seen = keys.to_vec();
    seen.sort();
    seen.dedup();
    if seen.len() != size {
        return Err(bad(W, 0, "perfect-hash keys are not unique"));
    }
    let too_large = || {
        bad(
            W,
            0,
            format!("{size} keys do not fit the 32-bit entries of a perfect hash"),
        )
    };
    let first: Vec<usize> = keys.iter().map(|k| slot(k, FNV_SEED, size)).collect();
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); size];
    for (i, &s) in first.iter().enumerate() {
        if let Some(bucket) = buckets.get_mut(s) {
            bucket.push(i);
        }
    }
    buckets.sort_by_key(|b| std::cmp::Reverse(b.len()));
    let mut used = vec![false; size];
    let multi = buckets.iter().take_while(|b| b.len() > 1).count();
    for bucket in buckets.iter().take(multi) {
        let mut seed = 1u32;
        let slots = loop {
            let mut slots = Vec::with_capacity(bucket.len());
            let ok = bucket.iter().all(|&m| {
                let s = keys.get(m).map_or(0, |k| slot(k, seed, size));
                let free = used.get(s) == Some(&false) && !slots.contains(&s);
                slots.push(s);
                free
            });
            if ok {
                break slots;
            }
            seed = seed
                .checked_add(1)
                .ok_or_else(|| bad(W, 0, "no seed separates a bucket of keys"))?;
        };
        let head = bucket.first().and_then(|&m| first.get(m)).copied();
        if let Some(entry) = head.and_then(|h| map.get_mut(h)) {
            *entry = i32::try_from(seed).map_err(|_| too_large())?;
        }
        for (&s, &m) in slots.iter().zip(bucket) {
            if let Some(o) = order.get_mut(s) {
                *o = m;
            }
            if let Some(u) = used.get_mut(s) {
                *u = true;
            }
        }
    }
    let mut free = 0usize;
    for bucket in buckets.iter().skip(multi).take_while(|b| !b.is_empty()) {
        while used.get(free) == Some(&true) {
            free = free.saturating_add(1).checked_rem(size).unwrap_or(0);
        }
        let Some(&m) = bucket.first() else { break };
        let entry = first.get(m).and_then(|&h| map.get_mut(h));
        if let Some(entry) = entry {
            *entry = !i32::try_from(free).map_err(|_| too_large())?;
        }
        if let Some(o) = order.get_mut(free) {
            *o = m;
        }
        if let Some(u) = used.get_mut(free) {
            *u = true;
        }
        free = free.saturating_add(1).checked_rem(size).unwrap_or(0);
    }
    Ok((map, order))
}

/// Index of `key` in `ordered` through the stored hash map, as the game looks it up.
pub fn lookup(map: &[i32], ordered: &[Vec<u8>], key: &[u8]) -> Option<usize> {
    if map.is_empty() || map.len() != ordered.len() {
        return None;
    }
    let v = *map.get(slot(key, FNV_SEED, map.len()))?;
    let i = if v < 0 {
        // the stored slot is `-slot - 1`, which is `!v`; no negation, so `i32::MIN` names the absent slot 2^31 - 1
        usize::try_from(!v).ok()?
    } else {
        slot(key, v.cast_unsigned(), map.len())
    };
    (ordered.get(i)? == key).then_some(i)
}

fn bundle_key(name: &str) -> Result<Vec<u8>> {
    latin1(&name.to_lowercase(), W)
}

/// Every bundle and chunk resolves through the stored perfect hashes. The offset of an error is the index of the
/// bundle or chunk that does not.
pub fn verify(toc: &Toc) -> Result<()> {
    let keys = toc
        .bundles
        .iter()
        .map(|(n, _)| bundle_key(n))
        .collect::<Result<Vec<_>>>()?;
    for (i, k) in keys.iter().enumerate() {
        if lookup(&toc.bundle_hash, &keys, k) != Some(i) {
            return Err(bad(
                W,
                i,
                "bundle perfect hash does not resolve: expected the name to find its own index",
            ));
        }
    }
    let keys: Vec<Vec<u8>> = toc.chunks.iter().map(|c| c.guid.to_vec()).collect();
    for (i, k) in keys.iter().enumerate() {
        if lookup(&toc.chunk_hash, &keys, k) != Some(i) {
            return Err(bad(
                W,
                i,
                "chunk perfect hash does not resolve: expected the GUID to find its own index",
            ));
        }
    }
    Ok(())
}

/// A Patch TOC (plain names): bundles and chunks are laid out in perfect-hash order. `flags` must not ask for
/// Huffman names. An error when a size or offset does not fit the 32 bits it is stored in.
pub fn write(bundles: &[(String, Vec<u8>)], chunks: &[TocChunk], flags: u32) -> Result<Vec<u8>> {
    if flags & 4 != 0 {
        return Err(bad(W, 0, "the Patch TOC writer stores plain names"));
    }
    let w32 = |n: usize| crate::u32_len(W, n);
    let bkeys = bundles
        .iter()
        .map(|(n, _)| bundle_key(n))
        .collect::<Result<Vec<_>>>()?;
    let (bmap, border) = perfect_hash(&bkeys)?;
    let ckeys: Vec<Vec<u8>> = chunks.iter().map(|c| c.guid.to_vec()).collect();
    let (cmap, corder) = perfect_hash(&ckeys)?;
    let bundle_at = |src: usize| {
        bundles
            .get(src)
            .ok_or_else(|| bad(W, src, format!("bundle {src} of {}", bundles.len())))
    };
    let mut names = Vec::new();
    let mut name_at = Vec::with_capacity(border.len());
    for &src in &border {
        name_at.push(names.len());
        names.extend(latin1(&bundle_at(src)?.0, W)?);
        names.push(0);
    }
    let mut words: Vec<u32> = Vec::new();
    let mut table = Vec::with_capacity(crate::at(0, chunks.len(), 20));
    for &src in &corder {
        let c = chunks
            .get(src)
            .ok_or_else(|| bad(W, src, format!("chunk {src} of {}", chunks.len())))?;
        table.extend(c.guid.iter().rev());
        match c.file {
            None => table.extend((-1i32).to_be_bytes()),
            Some((loc, off, size)) => {
                table.extend(((0x80u32 << 24) | w32(words.len())?).to_be_bytes());
                let [a, b, c, d, e, f, g, h] = loc.encode().to_be_bytes();
                words.extend([
                    u32::from_be_bytes([a, b, c, d]),
                    u32::from_be_bytes([e, f, g, h]),
                    off,
                    size,
                ]);
            }
        }
    }
    let bho = 48usize;
    let bdo = align(crate::at(bho, bundles.len(), 4), 8);
    let cho = align(crate::at(bdo, bundles.len(), 16), 4);
    let cgo = align(crate::at(cho, chunks.len(), 4), 4);
    let cdo = align(cgo.saturating_add(table.len()), 4);
    let nameso = crate::at(cdo, words.len(), 4);
    let mut pos = align(nameso.saturating_add(names.len()), 4);
    let mut region_at = Vec::with_capacity(border.len());
    for &src in &border {
        region_at.push(pos);
        pos = align(pos.saturating_add(bundle_at(src)?.1.len()), 4);
    }
    let mut out = vec![0u8; ENVELOPE.saturating_add(pos)];
    let (envelope, d) = out
        .split_at_mut_checked(ENVELOPE)
        .ok_or_else(|| bad(W, 0, "TOC larger than the address space"))?;
    if let Some(magic) = envelope.get_mut(..4) {
        magic.copy_from_slice(&MAGIC);
    }
    // every write lies inside `d`, which is sized from the same offsets
    let put = |d: &mut [u8], at: usize, v: &[u8]| -> Result<()> {
        d.get_mut(at..at.saturating_add(v.len()))
            .ok_or_else(|| bad(W, at, "internal layout error: write past the TOC"))?
            .copy_from_slice(v);
        Ok(())
    };
    let has_chunks = !chunks.is_empty();
    let head = [
        w32(bho)?,
        w32(bdo)?,
        w32(bundles.len())?,
        if has_chunks { w32(cho)? } else { 0 },
        if has_chunks { w32(cgo)? } else { 0 },
        w32(chunks.len())?,
        w32(cdo)?,
        w32(cdo)?,
        w32(nameso)?,
        w32(cdo)?,
        w32(words.len())?,
        flags,
    ];
    for (i, v) in head.iter().enumerate() {
        put(d, crate::at(0, i, 4), &v.to_be_bytes())?;
    }
    for (i, v) in bmap.iter().enumerate() {
        put(d, crate::at(bho, i, 4), &v.to_be_bytes())?;
    }
    for (i, &src) in border.iter().enumerate() {
        let region = &bundle_at(src)?.1;
        if region.len() > 0x3FFF_FFFF {
            return Err(bad(W, i, "bundle region exceeds 30 bits"));
        }
        let (name, at) = (
            name_at.get(i).copied().unwrap_or(0),
            region_at.get(i).copied().unwrap_or(0),
        );
        let entry = crate::at(bdo, i, 16);
        put(d, entry, &w32(name)?.to_be_bytes())?;
        put(
            d,
            entry.saturating_add(4),
            &((1u32 << 30) | w32(region.len())?).to_be_bytes(),
        )?;
        put(d, entry.saturating_add(8), &(at as u64).to_be_bytes())?;
        put(d, at, region)?;
    }
    if has_chunks {
        for (i, v) in cmap.iter().enumerate() {
            put(d, crate::at(cho, i, 4), &v.to_be_bytes())?;
        }
        put(d, cgo, &table)?;
        for (i, w) in words.iter().enumerate() {
            put(d, crate::at(cdo, i, 4), &w.to_be_bytes())?;
        }
    }
    put(d, nameso, &names)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// Reference values of the TOC hash (independent Python TOC hashing).
    #[test]
    fn toc_hash_reference_values() {
        assert_eq!(super::hash_with(&[0, 127, 128, 255], 0x811c9dc5), 0x529916);
        assert_eq!(super::hash_with(b"levels/test", 0x811c9dc5), 0xb97702);
    }
}

#[cfg(test)]
mod huffman_tests {
    use super::*;

    /// The table `entries` (big-endian words) and the code `bits` (bit 0 first, in one word) as a Huffman reader's input.
    fn huffman(entries: &[u32], bits: u32) -> Result<Huffman> {
        let mut data = bits.to_be_bytes().to_vec();
        for e in entries {
            data.extend(e.to_be_bytes());
        }
        let r = Rd::new(&data, W);
        Huffman::read(&r, 0, 1, 4, entries.len())
    }

    /// One internal node: bit 0 is the character 'a', bit 1 the terminator; "a" is the bits 0 then 1.
    #[test]
    fn a_name_decodes_through_its_table() {
        let h = huffman(&[!u32::from(b'a'), !0], 0b10).unwrap();
        assert_eq!(h.decode(0).unwrap(), "a");
    }

    /// Two levels: the first pair is internal node 0, the second pair (node 0, 'b') is node 1, the root.
    #[test]
    fn an_internal_node_is_named_by_its_counter() {
        let table = [!u32::from(b'a'), !0, 0, !u32::from(b'b')];
        // root: bit 0 -> node 0 ('a' on 0, end on 1), bit 1 -> 'b'. "b" then the end: 1, then 0, 1.
        let h = huffman(&table, 0b101).unwrap();
        assert_eq!(h.decode(0).unwrap(), "b");
    }

    /// A small value is no character: it used to become a leaf of that number, shadowing the internal node the same number
    /// would name once built (one map held both). It is refused where it stands, and so is a counter not built yet.
    #[test]
    fn a_value_that_is_neither_a_character_nor_a_built_node_is_refused() {
        let e = huffman(&[0, !0], 0).err().unwrap().to_string();
        assert!(e.contains("neither a character nor an internal node"), "{e}");
        let e = huffman(&[!u32::from(b'a'), !0, 1, !0], 0)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("0x1"), "{e}");
    }
}
