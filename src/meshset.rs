//! MeshSet resources (type 0x49B156D4): the LODs, their sections and vertex declarations, and where each section's
//! vertices and indices lie in the LOD's geometry chunk. Read ([`read`], [`read_resource`]) and written back
//! ([`write()`]): every MeshSet of the game reads and writes back byte-identical, its resource metadata too
//! (`tests/meshset.rs`).
//!
//! A MeshSet is one relocatable block: fixed-size records whose pointers (u64, stored relative to byte 16) name a pool
//! after them, then a relocation table listing every pointer field, then (one development mesh only) data the
//! metadata sizes. The bundle's resource metadata is `{table offset, table bytes, trailing bytes, 0}`. That trailing
//! data is geometry: in skate.'s stock bundles the one MeshSet that has any (a development mesh) also has the only LOD
//! whose chunk GUID is all zero, and the data is that LOD's vertex bytes, its index bytes and four zero bytes
//! (`tail.len() == vertex_bytes + index_bytes + 4`) instead of a geometry chunk ([`MeshSet::tail`]).
//!
//! ```text
//! header (0xd0 bytes, 0xe0 for the meshes with bone bounds):
//!   0x00 u32 header bytes - 16   0x04 u32 LOD record bytes (0xb0; 0xb8 or 0xbc, below)   0x08 u32 section bytes (384)
//!   0x10 f32x4 bounds minimum   0x20 f32x4 maximum   0x30 u64[7] LOD records   0x68 u64 name   0x70 u64 its last part
//!   0xb4 u16 LOD count; header 0xe0: 0xc4 u16 bones, 0xc6 u16 bounded bones, 0xc8 u64 their ids (u16 each),
//!   0xd0 u64 bone bounds (f32x4 minimum, maximum; one a bounded bone, else one a bone)
//! LOD record (each 16-aligned): +0x08 u32 sections, +0x0c u64 the LOD's section records, five lists (+0x14 + 12k
//!   u32 count, u64 pointer: section indices, a byte each), +0x54 u32 index format (0x21: 16-bit, 0x2e: 32-bit
//!   indices), +0x58 u32 index-buffer bytes, +0x5c u32 vertex-buffer bytes (a multiple of 16), +0x74 geometry chunk
//!   GUID (the vertex buffer, then the index buffer), +0x8c u64 name, +0x94 u64 a part of it, +0x9c u64 its last part;
//!   record 0xb8: +0xb0 u64 each section's bone mask (24 bytes); record 0xbc: +0xb0 u32 count, +0xb4 u64 a bone map
//!   (u32 each)
//! section (384 bytes): +0x08 u64 material name, +0x10 u64 bone list (u16 each; when +0x18 u16 the bone count is not
//!   zero: the skeleton bone the section's vertices name by that index), +0x1b flags, +0x1e total vertex stride,
//!   +0x20 primitive count, +0x24 start index (again at +0x140), +0x28 vertex offset in bytes (again at +0x54), +0x2c
//!   vertex count, two vertex declarations (100 bytes each: 16 elements of usage, format, offset, stream; 16 stream
//!   strides of two bytes; element count; stream count): the stored one at +0x70 and the draw one at +0xd4 (usually
//!   equal; see `check_draw` for how they may differ: unbound elements, aliases into stored streams, streams filled
//!   at run time); +0x138 u64 material key
//! ```
//!
//! Order of the block: header, LOD records, every LOD's section records, then the pool: the bone lists (16-aligned
//! after), the strings (per LOD its section names then its name, then the MeshSet's name; each name's parts point into
//! it, or, in ReSkate Studio's meshes, at copies of their own right after it; 16-aligned after), the
//! five lists of each LOD (16-aligned after), each LOD's bone masks (16-aligned) or bone map (4-aligned), the bone ids
//! and the bone bounds (each 16-aligned), 16-aligned; the relocation table (one u32 a pointer, its field's offset - 16:
//! per LOD its section pointer, each section's name and bone list, the five lists, the three names, the masks or map;
//! then the header's LOD records, names, bone ids and bounds), 16-aligned; the trailing data.
//!
//! A section's stored streams lie one after another from its vertex offset, each `stride * vertex count` bytes.

use crate::{Rd, Result, bad, capacity};

const W: &str = "meshset";
/// Bytes in a section record.
pub const SECTION_SIZE: usize = 384;
/// Bytes in a vertex declaration.
pub const DECL_SIZE: usize = 100;
/// Where a section record's stored declaration starts.
pub const DECL_AT: usize = 0x70;
/// Where a section record's draw declaration starts.
pub const DECL2_AT: usize = 0xd4;
const KEY_AT: usize = 0x138;
/// The LOD record pointers (u64) lie from 0x30 up to the name at 0x68 (Studio refuses more: `mesh LOD count must be
/// 1..7`).
pub const MAX_LODS: usize = (0x68 - 0x30) / 8;

/// Element usages seen in skate.'s declarations (Frostbite's VertexElementUsage).
pub mod usage {
    /// Vertex position.
    pub const POSITION: u8 = 1;
    /// Blend (skinning) bone indices.
    pub const BLEND_INDICES: u8 = 2;
    /// Blend (skinning) bone weights.
    pub const BLEND_WEIGHTS: u8 = 4;
    /// First vertex colour.
    pub const COLOR0: u8 = 30;
    /// Second vertex colour.
    pub const COLOR1: u8 = 31;
    /// First texture coordinate set (UV0).
    pub const TEXCOORD0: u8 = 33;
    /// Second texture coordinate set (UV1).
    pub const TEXCOORD1: u8 = 34;
    /// Sub-material index.
    pub const SUBMATERIAL: u8 = 51;
    /// Packed tangent frame, four bytes a vertex (see [`crate::tangent`]).
    pub const TANGENT_SPACE: u8 = 52;
}

/// Element formats (Frostbite's VertexElementFormat, with the numbering Frosty's SDK uses) and their sizes in bytes.
/// `tests/meshset.rs` checks every element of the base game against its stream stride with these sizes.
pub mod format {
    /// Two 32-bit floats, 8 bytes.
    pub const FLOAT2: u8 = 0x02;
    /// Three 32-bit floats, 12 bytes.
    pub const FLOAT3: u8 = 0x03;
    /// Four 32-bit floats, 16 bytes.
    pub const FLOAT4: u8 = 0x04;
    /// Two half floats, 4 bytes.
    pub const HALF2: u8 = 0x06;
    /// Four half floats, 8 bytes.
    pub const HALF4: u8 = 0x08;
    /// Four unsigned bytes, 4 bytes.
    pub const UBYTE4: u8 = 0x0c;
    /// Four normalised unsigned bytes, 4 bytes.
    pub const UBYTE4N: u8 = 0x0d;
    /// Four unsigned 16-bit integers, 8 bytes.
    pub const USHORT4: u8 = 0x17;
    /// The size in bytes of an element of format `f`, `None` for a format not listed.
    pub fn size(f: u8) -> Option<usize> {
        Some(match f {
            0x32 => 1,               // UByteN
            0x05 | 0x0e | 0x12 => 2, // Half, Short, ShortN
            0x07 | 0x10 | 0x14 => 6, // Half3, Short3, Short3N
            0x01
            | 0x06
            | 0x0a..=0x0d
            | 0x0f
            | 0x13
            | 0x16
            | 0x18
            | 0x1a
            | 0x1d
            | 0x20
            | 0x23
            | 0x26..=0x31 => 4,
            0x02 | 0x08 | 0x11 | 0x15 | 0x17 | 0x19 | 0x1b | 0x1e | 0x21 | 0x24 => 8,
            0x03 | 0x33 | 0x34 => 12,               // Float3, Int3, UInt3
            0x04 | 0x1c | 0x1f | 0x22 | 0x25 => 16, // Float4, Int4, Int4N, UInt4, UInt4N
            _ => return None,
        })
    }
}

/// One element of a vertex declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Element {
    /// The element's usage ([`usage`]).
    pub usage: u8,
    /// The element's format ([`mod@format`]); [`format::size`] gives its size.
    pub format: u8,
    /// Byte offset of the element within its stream's vertex.
    pub offset: u8,
    /// Index of the vertex stream the element is read from; 255 marks an element bound to no stream (the shader reads a default).
    pub stream: u8,
}

/// A vertex declaration, decoded, and its stored bytes ([`write()`] writes the bytes; the elements and strides are
/// their reading).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decl {
    /// The declared elements, in stored order (at most 16).
    pub elements: Vec<Element>,
    /// Bytes per vertex of each stream (at most 16 streams).
    pub strides: Vec<u8>,
    /// The declaration exactly as stored (100 bytes: 16 element slots of 4 bytes, 16 stream strides of 2 bytes, then the element and stream counts).
    pub bytes: [u8; DECL_SIZE],
}

impl Default for Decl {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            strides: Vec::new(),
            bytes: [0; DECL_SIZE],
        }
    }
}

impl Decl {
    /// Decode a stored declaration. Refused: more than 16 elements or streams.
    pub fn read(b: &[u8; DECL_SIZE]) -> Result<Self> {
        let [.., ne, ns, _, _] = *b;
        let (ne, ns) = (usize::from(ne), usize::from(ns));
        if ne > 16 || ns > 16 {
            return Err(bad(W, 96, format!("declaration of {ne} elements, {ns} streams")));
        }
        let elements = b
            .as_chunks::<4>()
            .0
            .iter()
            .take(ne)
            .map(|&[usage, format, offset, stream]| Element {
                usage,
                format,
                offset,
                stream,
            })
            .collect();
        // the stream strides lie from byte 64, two bytes each, the stride in the first
        let strides = b
            .get(64..96)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .take(ns)
            .map(|&[stride, _]| stride)
            .collect();
        Ok(Self {
            elements,
            strides,
            bytes: *b,
        })
    }
    /// The sum of the stream strides: the bytes of one vertex.
    pub fn stride(&self) -> usize {
        self.strides.iter().map(|&s| usize::from(s)).sum()
    }
    /// The first element of `usage`.
    pub fn element(&self, usage: u8) -> Option<Element> {
        self.elements.iter().copied().find(|e| e.usage == usage)
    }
}

/// One section of a LOD: a material's run of vertices and indices, with its vertex layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Section {
    /// Offset of the section record in the payload it was read from.
    pub at: usize,
    /// The record as stored ([`SECTION_SIZE`] bytes), its pointers zero: [`write()`] writes its pointers and the fields
    /// below over it.
    pub record: Vec<u8>,
    /// The material name (+0x08).
    pub name: String,
    /// The material key (u64 at +0x138 of the section record).
    pub key: u64,
    /// The stored vertex layout (what the chunk holds).
    pub decl: Decl,
    /// The draw layout: `decl` plus any streams filled at run time.
    pub draw_decl: Decl,
    /// The section flags (u8 at +0x1b). Stock MeshSets hold 15 values, all below 0x20 (300,875 sections of skate.'s
    /// build 20260929): `0x0a` on 170,233 sections, `0x04` on 116,897, `0x0e` on 10,935, the other twelve on fewer
    /// than 600 each. The byte does not name the LOD's section list (a category) a section is in: `0x0a` is the
    /// flag of sections in list 0 alone (156,048) as well as of sections in list 2 alone (9,943). What the bits mean
    /// is not established.
    pub flags: u8,
    /// Total vertex stride in bytes (u8 at +0x1e).
    pub stride: u8,
    /// Number of triangles (u32 at +0x20).
    pub primitives: u32,
    /// Index of the section's first index in the LOD's index buffer (u32 at +0x24).
    pub start_index: u32,
    /// Byte offset of the section's vertices in the LOD's vertex buffer (u32 at +0x28).
    pub vertex_offset: u32,
    /// Vertex count (u32 at +0x2c).
    pub vertices: u32,
    /// The bone list (the module documentation's section +0x10): empty for a section with no bones.
    pub bones: Vec<u16>,
}

impl Section {
    /// The primitive type of the section's indices ([`SECTION_PRIMITIVE`]); `None` for a value that is none of [`PrimitiveType`].
    pub fn primitive_type(&self) -> Option<PrimitiveType> {
        PrimitiveType::from_u8(self.record.get(SECTION_PRIMITIVE).copied()?)
    }
    /// Where stream `j` starts in the chunk; `None` when the section has no stream `j` (`j` may be the stream count,
    /// the end of the last) or the offset does not fit the address space.
    pub fn stream_offset(&self, j: usize) -> Option<usize> {
        let vertices = usize::try_from(self.vertices).ok()?;
        self.decl
            .strides
            .get(..j)?
            .iter()
            .try_fold(usize::try_from(self.vertex_offset).ok()?, |at, &s| {
                at.checked_add(usize::from(s).checked_mul(vertices)?)
            })
    }
    /// One past the section's last vertex byte (saturating: an end past the address space is past any buffer).
    fn vertex_end(&self) -> usize {
        wide(self.vertex_offset).saturating_add(self.decl.stride().saturating_mul(wide(self.vertices)))
    }
}

/// One LOD: its sections, section lists, geometry chunk and buffer sizes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lod {
    /// Offset of the LOD record in the payload it was read from.
    pub at: usize,
    /// The record as stored (the header's LOD record size, 16-aligned), its pointers zero: [`write()`] writes its
    /// pointers and the fields below over it.
    pub record: Vec<u8>,
    /// The LOD's name (+0x8c), and its parts +0x94 and +0x9c.
    pub name: String,
    /// The LOD name's two parts at +0x94 and +0x9c.
    pub name_parts: [NamePart; 2],
    /// The five lists of section indices (+0x14 + 12k).
    pub lists: [Vec<u8>; LISTS],
    /// GUID of the LOD's geometry chunk, which holds the vertex buffer and then the index buffer (16 bytes at +0x74).
    /// All zero for the one stock LOD whose geometry is the MeshSet's [`tail`](MeshSet::tail) instead.
    pub chunk: [u8; 16],
    /// Bytes per index: 2 (format 0x21) or 4 (format 0x2e), from the LOD's index format at +0x54.
    pub index_size: u8,
    /// Size in bytes of the LOD's index buffer (u32 at +0x58).
    pub index_bytes: u32,
    /// Size in bytes of the LOD's vertex buffer (u32 at +0x5c); a multiple of 16.
    pub vertex_bytes: u32,
    /// The LOD's sections, in stored order.
    pub sections: Vec<Section>,
    /// LOD record 0xb8: each section's bone mask (+0xb0).
    pub bone_masks: Option<Vec<[u8; MASK]>>,
    /// LOD record 0xbc: the bone map (+0xb0 count, +0xb4).
    pub bone_map: Option<Vec<u32>>,
}

impl Lod {
    /// The LOD's layout flags word ([`LOD_FLAGS`]).
    pub fn flags(&self) -> u32 {
        word(&self.record, LOD_FLAGS)
    }
}

/// A MeshSet as read: the records as stored, with the typed fields, names and tables decoded beside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshSet {
    /// The header as stored ([`HEADER`] or [`HEADER_BONES`] bytes), its pointers zero: [`write()`] writes its counts
    /// and pointers over it.
    pub header: Vec<u8>,
    /// The MeshSet's name (0x68), and its last part (0x70).
    pub name: String,
    /// The MeshSet name's last part (pointer at +0x70).
    pub short_name: NamePart,
    /// The LODs, at most [`MAX_LODS`].
    pub lods: Vec<Lod>,
    /// Header 0xe0: the bounded bones (0xc8, as many as 0xc6 counts) and the bone bounds (0xd0, as stored: f32x4
    /// minimum, f32x4 maximum; [`bounds`]): one a bounded bone, else one a bone (0xc4).
    pub bone_ids: Option<Vec<u16>>,
    /// The bone bounds as stored, 32 bytes each ([`bounds`] decodes one); present only in headers of [`HEADER_BONES`] bytes.
    pub bone_bounds: Option<Vec<[u8; BOUNDS]>>,
    /// The data after the relocation table (the metadata's third word), which only [`read_resource`] reads. Empty in
    /// every stock MeshSet but one development mesh, where it is the geometry of the LOD whose [`Lod::chunk`] is all
    /// zero: that LOD's vertex bytes and index bytes ([`Lod::vertex_bytes`], [`Lod::index_bytes`]) and four more
    /// bytes, 2,001,568 in all: the vertex buffer, then the index buffer (every index names a vertex of the LOD), then
    /// four zero bytes (observed on skate.'s build 20260929).
    pub tail: Vec<u8>,
}

/// A MeshSet's kind, the byte at header 0x7c. In all 47,758 stock MeshSets: `Rigid` (46,977), `Skinned` (296) and
/// `Composite` (485). The kind decides the form of the LOD records (plain [`LOD_PLAIN`], a bone map [`LOD_MAP`] for
/// `Skinned`, per-section bone masks [`LOD_MASKS`] for `Composite`) and whether the header has bone bounds
/// ([`HEADER_BONES`]); every section of the 781 meshes of the last two declares blend indices and none of the rigid ones
/// does. The names are those of Frostbite's mesh types; which of the two non-rigid kinds is `Skinned` and which
/// `Composite` is the names' order, which the data does not contradict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MeshType {
    /// A mesh drawn as it is.
    Rigid = 0,
    /// A mesh deformed by a skeleton.
    Skinned = 1,
    /// A mesh made of parts that move with a skeleton.
    Composite = 2,
}

impl MeshType {
    /// The type the byte `v` names.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Rigid,
            1 => Self::Skinned,
            2 => Self::Composite,
            _ => return None,
        })
    }
}

/// Where a MeshSet header keeps its kind ([`MeshType`], a byte).
pub const HEADER_TYPE: usize = 0x7c;
/// Where a MeshSet header keeps its layout flags (a `u32`).
pub const HEADER_FLAGS: usize = 0xa8;
/// Where a LOD record keeps its layout flags (a `u32`).
pub const LOD_FLAGS: usize = 0x50;
/// Where a section record keeps its primitive type ([`PrimitiveType`], a byte).
pub const SECTION_PRIMITIVE: usize = 0x1f;

/// Primitive types of a section's indices. :observed: byte 0x1f of a section record is 3 in all 300,875 stock sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PrimitiveType {
    /// Separate points.
    PointList = 0,
    /// Separate lines.
    LineList = 1,
    /// A strip of lines.
    LineStrip = 2,
    /// Separate triangles, three indices each: the only type in the stock data.
    TriangleList = 3,
    /// A strip of triangles.
    TriangleStrip = 5,
    /// Separate quads.
    QuadList = 7,
    /// Rectangles (a console primitive).
    XenonRectList = 8,
    /// Tessellation patches of triangles.
    TrianglePatch = 9,
}

impl PrimitiveType {
    /// The type the byte `v` names.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::PointList,
            1 => Self::LineList,
            2 => Self::LineStrip,
            3 => Self::TriangleList,
            5 => Self::TriangleStrip,
            7 => Self::QuadList,
            8 => Self::XenonRectList,
            9 => Self::TrianglePatch,
            _ => return None,
        })
    }
}

/// The section lists of a LOD ([`Lod::lists`]) by what each draws: the order of the subset categories Frostbite meshes
/// use. :inferred from that order and from the use of the lists (the decal sections are in list 2); no field of the
/// data names them.
pub mod category {
    /// The main pass.
    pub const OPAQUE: usize = 0;
    /// Transparent sections (non-empty in 228 of 126,014 stock LODs).
    pub const TRANSPARENT: usize = 1;
    /// Decals.
    pub const TRANSPARENT_DECAL: usize = 2;
    /// The depth-only pass.
    pub const Z_ONLY: usize = 3;
    /// The shadow pass.
    pub const SHADOW: usize = 4;
}

/// The bits of a MeshSet's layout flags word ([`HEADER_FLAGS`]) that the data names. Bits 0 to 31 take other values in the
/// stock data (bits 1, 4 and 7 on 25, 3 and 557 MeshSets, a block of four tier bits at each of 8 to 11, 12 to 15, 16 to 19
/// and 20 to 23, bits 29, 30 and 31 on 470, 134 and 38,345) that nothing here names.
pub mod set_flags {
    /// The geometry lies in chunks. Set on 47,757 of the 47,758 stock MeshSets; clear on the one whose LOD holds its
    /// geometry inline ([`MeshSet::tail`](super::MeshSet::tail)).
    pub const STREAMING_ENABLE: u32 = 1 << 0;
    /// Set on a MeshSet exactly when some LOD sets [`lod_flags::STREAM_INSTANCING_ENABLE`](super::lod_flags) (321
    /// MeshSets).
    pub const STREAM_INSTANCING_ENABLE: u32 = 1 << 2;
}

/// The bits of a LOD's layout flags word ([`LOD_FLAGS`]) that the data names. Bits 0, 9, 17 and 18 take other values (50,130,
/// 738, 2,364 and 1,200 of the 126,014 stock LODs) that nothing here names; bit 0 is not set exactly on a MeshSet's first
/// LOD.
pub mod lod_flags {
    /// The LOD's geometry is a chunk: set on 126,013 stock LODs, clear on the one whose geometry is inline.
    pub const STREAMING_ENABLE: u32 = 1 << 6;
    /// Set on 402 stock LODs, all in the MeshSets that set [`set_flags::STREAM_INSTANCING_ENABLE`](super::set_flags).
    pub const STREAM_INSTANCING_ENABLE: u32 = 1 << 4;
}

impl MeshSet {
    /// The kind of mesh the header byte at [`HEADER_TYPE`] names; `None` for a value that is none of [`MeshType`].
    pub fn mesh_type(&self) -> Option<MeshType> {
        MeshType::from_u8(self.header.get(HEADER_TYPE).copied()?)
    }
    /// The header's layout flags word ([`HEADER_FLAGS`]); 0 for a header too short to hold it.
    pub fn layout_flags(&self) -> u32 {
        word(&self.header, HEADER_FLAGS)
    }
    /// The header's LOD record size (0x04).
    pub fn lod_size(&self) -> usize {
        wide(word(&self.header, 4))
    }
}

/// Header sizes: without and with bone bounds.
pub const HEADER: usize = 0xd0;
/// Header size with bone bounds.
pub const HEADER_BONES: usize = 0xe0;
/// LOD record sizes: plain, with each section's bone mask, with a bone map.
pub const LOD_PLAIN: usize = 0xb0;
/// LOD record size with a bone mask per section.
pub const LOD_MASKS: usize = 0xb8;
/// LOD record size with a bone map.
pub const LOD_MAP: usize = 0xbc;
/// Section index lists per LOD.
pub const LISTS: usize = 5;
/// Bytes in a section's bone mask.
pub const MASK: usize = 24;
/// Bytes in a stored bone bound.
pub const BOUNDS: usize = 32;

/// A stored bone bound as its minimum and maximum.
pub fn bounds(b: &[u8; BOUNDS]) -> [[f32; 4]; 2] {
    let quad = |half: &[u8]| {
        let mut q = [0f32; 4];
        for (v, c) in q.iter_mut().zip(half.as_chunks::<4>().0) {
            *v = f32::from_le_bytes(*c);
        }
        q
    };
    let (min, max) = b.split_at(BOUNDS / 2);
    [quad(min), quad(max)]
}

/// A stored 32-bit size, count or offset as a `usize` (the address space holds it).
fn wide(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// `at` moved on by `by`; past the address space it saturates, and the read or write that uses it is refused.
fn plus(at: usize, by: usize) -> usize {
    at.saturating_add(by)
}

/// The little-endian `u32` at `at` of a record held in memory; 0 past its end.
fn word(b: &[u8], at: usize) -> u32 {
    b.get(at..)
        .and_then(|b| b.first_chunk::<4>())
        .map_or(0, |w| u32::from_le_bytes(*w))
}

/// The draw declaration against the stored one: the same streams first (equal strides), every stored element
/// present, and every other element either unbound (stream 255: the shader reads a default), reading inside a stored
/// stream (an alias, e.g. a second texture coordinate on the first one's stream) or in a stream after the stored ones
/// (filled at run time). Element order may differ.
fn check_draw(stored: &Decl, draw: &Decl) -> std::result::Result<(), String> {
    if !draw.strides.starts_with(&stored.strides) {
        return Err("the draw declaration's streams do not start with the stored ones".into());
    }
    if let Some(e) = stored.elements.iter().find(|e| !draw.elements.contains(e)) {
        return Err(format!("stored element {e:?} missing from the draw declaration"));
    }
    for e in draw.elements.iter().filter(|e| !stored.elements.contains(e)) {
        if e.stream == 255 {
            continue;
        }
        let size = format::size(e.format).ok_or_else(|| format!("unknown element format {}", e.format))?;
        let stride = *draw
            .strides
            .get(usize::from(e.stream))
            .ok_or("draw element stream out of range")?;
        if usize::from(e.offset).saturating_add(size) > usize::from(stride) {
            return Err(format!("draw element {e:?} runs past its stream"));
        }
    }
    Ok(())
}

fn ptr(r: &Rd, at: usize) -> Result<usize> {
    let p = r.u64le(at)?;
    usize::try_from(p)
        .ok()
        .and_then(|p| p.checked_add(16))
        .ok_or_else(|| bad(W, at, "pointer out of range"))
}

/// A record's bytes with its pointer fields (u64 at `fields`) zero: where they point is the layout's, which
/// [`write()`] lays out anew.
fn without_pointers(b: &[u8], fields: &[usize]) -> Result<Vec<u8>> {
    let mut v = b.to_vec();
    for &f in fields {
        put(&mut v, f, &[0; 8])?;
    }
    Ok(v)
}

/// A pointer field that may be null.
fn opt_ptr(r: &Rd, at: usize) -> Result<Option<usize>> {
    Ok(if r.u64le(at)? == 0 {
        None
    } else {
        Some(ptr(r, at)?)
    })
}

/// What a name's part pointer names: a place in the name (the game's own meshes: the part's first byte) or a string
/// of its own (ReSkate Studio's meshes, the part's text again right after the name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamePart {
    /// The part starts `usize` bytes into the name.
    At(usize),
    /// The part is a string of its own, stored after the name.
    Own(String),
}

impl Default for NamePart {
    fn default() -> Self {
        Self::At(0)
    }
}

/// The name the pointer at `at` names, and what the pointers at `parts` name.
fn name<const N: usize>(r: &Rd, at: usize, parts: [usize; N]) -> Result<(String, [NamePart; N])> {
    let a = ptr(r, at)?;
    let s = r.cstr(a)?;
    let mut out: [NamePart; N] = std::array::from_fn(|_| NamePart::default());
    for (o, &p) in out.iter_mut().zip(&parts) {
        let b = ptr(r, p)?;
        *o = match b.checked_sub(a).filter(|&o| o <= s.len()) {
            Some(o) => NamePart::At(o),
            None => NamePart::Own(r.cstr(b)?),
        };
    }
    Ok((s, out))
}

/// `n` items of `size` bytes from `at`, read by `item`.
fn items<T>(r: &Rd, at: usize, n: usize, size: usize, item: impl Fn(usize) -> Result<T>) -> Result<Vec<T>> {
    (0..capacity(W, n, size, r.data.len())?)
        .map(|i| item(crate::at(at, i, size)))
        .collect()
}

/// The MeshSet's records, names, lists and bone tables (everything but the data after the relocation table, which
/// [`read_resource`] adds). Checks each layout relation the module doc names and fails on the first broken one
/// (strict: a MeshSet this reader accepts can be rewritten section by section).
pub fn read(d: &[u8]) -> Result<MeshSet> {
    let r = Rd::new(d, W);
    let header_size = plus(r.size32le(0)?, 16);
    if header_size != HEADER && header_size != HEADER_BONES {
        return Err(bad(W, 0, format!("header of {header_size:#x} bytes")));
    }
    let lod_size = r.size32le(4)?;
    if ![LOD_PLAIN, LOD_MASKS, LOD_MAP].contains(&lod_size) {
        return Err(bad(W, 4, format!("LOD records of {lod_size:#x} bytes")));
    }
    let section_size = r.size32le(8)?;
    if section_size != SECTION_SIZE {
        return Err(bad(W, 8, format!("section records of {section_size:#x} bytes")));
    }
    let n = usize::from(r.u16le(0xb4)?);
    if n > MAX_LODS {
        return Err(bad(
            W,
            0xb4,
            format!("{n} LODs, but only {MAX_LODS} record pointers fit between 0x30 and the name"),
        ));
    }
    let (name_, [short_name]) = name(&r, 0x68, [0x70])?;
    let mut lods = Vec::with_capacity(n);
    for l in 0..n {
        let at = ptr(&r, crate::at(0x30, l, 8))?;
        let field = |k: usize| plus(at, k);
        let count = r.size32le(field(8))?;
        let first = ptr(&r, field(0x0c))?;
        let (lod_name, name_parts) = name(&r, field(0x8c), [field(0x94), field(0x9c)])?;
        let mut lists: [Vec<u8>; LISTS] = Default::default();
        for (k, list) in lists.iter_mut().enumerate() {
            let c = r.size32le(crate::at(field(0x14), k, 12))?;
            *list = r.bytes(ptr(&r, crate::at(field(0x18), k, 12))?, c)?.to_vec();
        }
        let bone_masks = match lod_size {
            LOD_MASKS => opt_ptr(&r, field(0xb0))?
                .map(|p| items(&r, p, count, MASK, |a| r.arr(a)))
                .transpose()?,
            _ => None,
        };
        let bone_map = match lod_size {
            LOD_MAP => opt_ptr(&r, field(0xb4))?
                .map(|p| items(&r, p, r.size32le(field(0xb0))?, 4, |a| r.u32le(a)))
                .transpose()?,
            _ => None,
        };
        let mut lod = Lod {
            at,
            record: without_pointers(
                r.bytes(at, crate::align(lod_size, 16))?,
                &[0x0c, 0x18, 0x24, 0x30, 0x3c, 0x48, 0x8c, 0x94, 0x9c]
                    .into_iter()
                    .chain(match lod_size {
                        LOD_MASKS => Some(0xb0),
                        LOD_MAP => Some(0xb4),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            )?,
            name: lod_name,
            name_parts,
            lists,
            chunk: r.arr(field(0x74))?,
            index_size: match r.u32le(field(0x54))? {
                0x21 => 2,
                0x2e => 4,
                f => return Err(bad(W, field(0x54), format!("unknown index format {f:#x}"))),
            },
            index_bytes: r.u32le(field(0x58))?,
            vertex_bytes: r.u32le(field(0x5c))?,
            sections: Vec::with_capacity(capacity(W, count, SECTION_SIZE, d.len())?),
            bone_masks,
            bone_map,
        };
        for s in 0..count {
            let o = crate::at(first, s, SECTION_SIZE);
            let field = |k: usize| plus(o, k);
            let decl = Decl::read(&r.arr(field(DECL_AT))?)?;
            let draw_decl = Decl::read(&r.arr(field(DECL2_AT))?)?;
            check_draw(&decl, &draw_decl).map_err(|d| bad(W, o, d))?;
            let sec = Section {
                at: o,
                record: without_pointers(r.bytes(o, SECTION_SIZE)?, &[0x08, 0x10])?,
                name: r.cstr(ptr(&r, field(8))?)?,
                key: r.u64le(field(KEY_AT))?,
                decl,
                draw_decl,
                flags: r.u8(field(0x1b))?,
                stride: r.u8(field(0x1e))?,
                primitives: r.u32le(field(0x20))?,
                start_index: r.u32le(field(0x24))?,
                vertex_offset: r.u32le(field(0x28))?,
                vertices: r.u32le(field(0x2c))?,
                bones: match usize::from(r.u16le(field(0x18))?) {
                    0 => Vec::new(),
                    c => items(&r, ptr(&r, field(0x10))?, c, 2, |a| r.u16le(a))?,
                },
            };
            if usize::from(sec.stride) != sec.decl.stride() {
                return Err(bad(
                    W,
                    o,
                    format!(
                        "stride byte {} != declared streams {}",
                        sec.stride,
                        sec.decl.stride()
                    ),
                ));
            }
            if r.u32le(field(0x54))? != sec.vertex_offset || r.u32le(field(0x140))? != sec.start_index {
                return Err(bad(W, o, "repeated vertex offset / start index differ"));
            }
            for e in &sec.decl.elements {
                let size = format::size(e.format)
                    .ok_or_else(|| bad(W, o, format!("unknown element format {}", e.format)))?;
                let stride = *sec
                    .decl
                    .strides
                    .get(usize::from(e.stream))
                    .ok_or_else(|| bad(W, o, "element stream out of range"))?;
                if usize::from(e.offset).saturating_add(size) > usize::from(stride) {
                    return Err(bad(W, o, "element runs past its stream"));
                }
            }
            lod.sections.push(sec);
        }
        let vend = lod.sections.iter().map(Section::vertex_end).max().unwrap_or(0);
        if !lod.vertex_bytes.is_multiple_of(16)
            || wide(lod.vertex_bytes)
                .checked_sub(vend)
                .is_none_or(|slack| slack >= 16)
        {
            return Err(bad(
                W,
                at,
                format!(
                    "vertex buffer {} for vertex data ending at {vend}",
                    lod.vertex_bytes
                ),
            ));
        }
        let iend = lod
            .sections
            .iter()
            .map(|s| {
                usize::from(lod.index_size)
                    .saturating_mul(wide(s.start_index).saturating_add(wide(s.primitives).saturating_mul(3)))
            })
            .max()
            .unwrap_or(0);
        if wide(lod.index_bytes)
            .checked_sub(iend)
            .is_none_or(|slack| slack >= 4)
        {
            return Err(bad(
                W,
                at,
                format!(
                    "index buffer {} for {}-byte indices ending at {iend}",
                    lod.index_bytes, lod.index_size
                ),
            ));
        }
        lods.push(lod);
    }
    let (mut bone_ids, mut bone_bounds) = (None, None);
    if header_size == HEADER_BONES {
        let (bones, bounded) = (usize::from(r.u16le(0xc4)?), usize::from(r.u16le(0xc6)?));
        if let Some(p) = opt_ptr(&r, 0xc8)? {
            bone_ids = Some(items(&r, p, bounded, 2, |a| r.u16le(a))?);
        }
        if let Some(p) = opt_ptr(&r, 0xd0)? {
            let c = if bone_ids.is_some() { bounded } else { bones };
            bone_bounds = Some(items(&r, p, c, BOUNDS, |a| r.arr(a))?);
        }
    }
    Ok(MeshSet {
        header: without_pointers(
            r.bytes(0, header_size)?,
            &(0..MAX_LODS)
                .map(|l| crate::at(0x30, l, 8))
                .chain([0x68, 0x70])
                .chain(if header_size == HEADER_BONES {
                    vec![0xc8, 0xd0]
                } else {
                    vec![]
                })
                .collect::<Vec<_>>(),
        )?,
        name: name_,
        short_name,
        lods,
        bone_ids,
        bone_bounds,
        tail: Vec::new(),
    })
}

/// Every pointer field of the records placed at `lod_at` and `sec_at`, in the relocation table's order (the module
/// documentation's).
fn pointer_fields(m: &MeshSet, lod_at: &[usize], sec_at: &[Vec<usize>]) -> Vec<usize> {
    let mut f = Vec::new();
    for ((lod, &at), secs) in m.lods.iter().zip(lod_at).zip(sec_at) {
        f.push(plus(at, 0x0c));
        for (sec, &o) in lod.sections.iter().zip(secs) {
            f.push(plus(o, 8));
            if !sec.bones.is_empty() {
                f.push(plus(o, 0x10));
            }
        }
        f.extend((0..LISTS).map(|k| crate::at(plus(at, 0x18), k, 12)));
        f.extend([plus(at, 0x8c), plus(at, 0x94), plus(at, 0x9c)]);
        if lod.bone_masks.is_some() {
            f.push(plus(at, 0xb0));
        }
        if lod.bone_map.is_some() {
            f.push(plus(at, 0xb4));
        }
    }
    f.extend((0..m.lods.len()).map(|l| crate::at(0x30, l, 8)));
    f.extend([0x68, 0x70]);
    if m.bone_ids.is_some() {
        f.push(0xc8);
    }
    if m.bone_bounds.is_some() {
        f.push(0xd0);
    }
    f
}

/// [`read`], and the data after the relocation table that the resource metadata `meta` sizes. Refused when the
/// metadata does not place the relocation table where it lies, or the table does not list exactly the records'
/// pointers (in any order: the game's own files list them in the module documentation's order, which [`write()`]
/// keeps; ReSkate Studio's in its template's).
pub fn read_resource(d: &[u8], meta: &[u8; 16]) -> Result<MeshSet> {
    let mut m = read(d)?;
    let w = |i: usize| wide(word(meta, crate::at(0, i, 4)));
    let (table, size, trailing) = (w(0), w(1), w(2));
    let end = crate::align(plus(table, size), 16);
    if w(3) != 0 || !size.is_multiple_of(4) || end.checked_add(trailing) != Some(d.len()) {
        return Err(bad(
            W,
            table,
            format!("metadata {} for {} bytes", crate::hex(meta), d.len()),
        ));
    }
    let r = Rd::new(d, W);
    let lod_at: Vec<usize> = m.lods.iter().map(|l| l.at).collect();
    let sec_at: Vec<Vec<usize>> = m
        .lods
        .iter()
        .map(|l| l.sections.iter().map(|s| s.at).collect())
        .collect();
    let fields = pointer_fields(&m, &lod_at, &sec_at);
    let mut stored = items(&r, table, size / 4, 4, |a| r.u32le(a).map(|v| plus(wide(v), 16)))?;
    let mut fields = fields;
    stored.sort_unstable();
    fields.sort_unstable();
    if stored != fields {
        return Err(bad(
            W,
            table,
            format!(
                "relocation table of {} entries, not the records' {} pointers",
                stored.len(),
                fields.len()
            ),
        ));
    }
    m.tail = d
        .get(end..)
        .ok_or_else(|| bad(W, end, "trailing data past the end"))?
        .to_vec();
    Ok(m)
}

/// Writes `v` at `at` of a record or block being built.
fn put(out: &mut [u8], at: usize, v: &[u8]) -> Result<()> {
    let total = out.len();
    at.checked_add(v.len())
        .and_then(|end| out.get_mut(at..end))
        .ok_or_else(|| {
            bad(
                W,
                at,
                format!("{} bytes past the end of the {total}-byte block", v.len()),
            )
        })?
        .copy_from_slice(v);
    Ok(())
}

fn pad(out: &mut Vec<u8>, a: usize) {
    out.resize(crate::align(out.len(), a), 0);
}

/// A name and its parts, appended (each a zero-terminated string): where the name and each part start. Refused: a
/// part [`NamePart::At`] past the name's end.
fn push_named<const N: usize>(
    out: &mut Vec<u8>,
    s: &str,
    parts: &[NamePart; N],
) -> Result<(usize, [usize; N])> {
    let at = push_name(out, s)?;
    let mut places = [0; N];
    for (place, part) in places.iter_mut().zip(parts) {
        *place = match part {
            NamePart::At(o) if *o <= s.len() => plus(at, *o),
            NamePart::At(o) => return Err(bad(W, at, format!("part at {o} of {s:?}, past its end"))),
            NamePart::Own(p) => push_name(out, p)?,
        };
    }
    Ok((at, places))
}

/// A name and its terminating zero, appended; where it starts.
fn push_name(out: &mut Vec<u8>, s: &str) -> Result<usize> {
    let at = out.len();
    let b = crate::latin1(s, W)?;
    if b.contains(&0) {
        return Err(bad(W, 0, format!("name {s:?} holds a zero")));
    }
    out.extend(b);
    out.push(0);
    Ok(at)
}

fn u32_of(n: usize, what: &str) -> Result<[u8; 4]> {
    u32::try_from(n)
        .map(u32::to_le_bytes)
        .map_err(|_| bad(W, 0, format!("{n} {what}")))
}

/// Entry `i` of a table the writer built beside the records; an error when the layout has none (the tables are built
/// from the same MeshSet, so this does not happen).
fn nth<T: Copy>(table: &[T], i: usize, what: &str) -> Result<T> {
    table
        .get(i)
        .copied()
        .ok_or_else(|| bad(W, i, format!("no {what} {i} in the layout")))
}

fn u16_of(n: usize, at: usize, what: &str) -> Result<[u8; 2]> {
    u16::try_from(n)
        .map(u16::to_le_bytes)
        .map_err(|_| bad(W, at, format!("{n} {what}")))
}

/// Write the pointer field `at` naming `target`.
fn pointer(out: &mut [u8], at: usize, target: usize) -> Result<()> {
    let relative = u64::try_from(target)
        .ok()
        .and_then(|t| t.checked_sub(16))
        .ok_or_else(|| bad(W, at, format!("a pointer to {target:#x}")))?;
    put(out, at, &relative.to_le_bytes())
}

/// The MeshSet written in the layout of the module documentation (the game's own order), and its resource
/// metadata: the records as [`MeshSet`] holds them, with their counts, pointers and typed fields written over them.
/// Refused: records of the wrong size, bone tables the header or LOD record has no room for or that do not match
/// their counts, counts past their fields, name parts outside their names.
pub fn write(m: &MeshSet) -> Result<(Vec<u8>, [u8; 16])> {
    let hs = m.header.len();
    if (hs != HEADER && hs != HEADER_BONES) || wide(word(&m.header, 0)).saturating_add(16) != hs {
        return Err(bad(W, 0, format!("a header of {hs:#x} bytes")));
    }
    let lod_size = m.lod_size();
    if ![LOD_PLAIN, LOD_MASKS, LOD_MAP].contains(&lod_size) || wide(word(&m.header, 8)) != SECTION_SIZE {
        return Err(bad(W, 4, "LOD or section records of an unknown size"));
    }
    if m.lods.len() > MAX_LODS {
        return Err(bad(W, 0xb4, format!("{} LODs", m.lods.len())));
    }
    if hs == HEADER && (m.bone_ids.is_some() || m.bone_bounds.is_some()) {
        return Err(bad(W, 0xc8, "bone bounds need the 0xe0 header"));
    }
    let lod_stride = crate::align(lod_size, 16);
    let mut out = m.header.clone();
    let mut lod_at = Vec::with_capacity(m.lods.len());
    for (l, lod) in m.lods.iter().enumerate() {
        let refuse = |what: &str| Err(bad(W, l, format!("LOD {l}: {what}")));
        if lod.record.len() != lod_stride {
            return refuse("a record of another size than the header's");
        }
        if (lod.bone_masks.is_some() && lod_size != LOD_MASKS)
            || (lod.bone_map.is_some() && lod_size != LOD_MAP)
        {
            return refuse("a bone table its record has no room for");
        }
        if lod
            .bone_masks
            .as_ref()
            .is_some_and(|b| b.len() != lod.sections.len())
        {
            return refuse("bone masks for other than its sections");
        }
        lod_at.push(out.len());
        out.extend(&lod.record);
    }
    let mut sec_at = Vec::with_capacity(m.lods.len());
    for (l, lod) in m.lods.iter().enumerate() {
        let mut row = Vec::with_capacity(lod.sections.len());
        for (s, sec) in lod.sections.iter().enumerate() {
            if sec.record.len() != SECTION_SIZE {
                return Err(bad(
                    W,
                    l,
                    format!("LOD {l} section {s}: a record of {} bytes", sec.record.len()),
                ));
            }
            row.push(out.len());
            out.extend(&sec.record);
        }
        sec_at.push(row);
    }
    // the pool
    let mut bones_at = Vec::new();
    for sec in m.lods.iter().flat_map(|l| &l.sections) {
        bones_at.push(out.len());
        sec.bones.iter().for_each(|b| out.extend(b.to_le_bytes()));
    }
    pad(&mut out, 16);
    let (mut names_at, mut lod_name_at) = (Vec::new(), Vec::new());
    for lod in &m.lods {
        for sec in &lod.sections {
            names_at.push(push_name(&mut out, &sec.name)?);
        }
        lod_name_at.push(push_named(&mut out, &lod.name, &lod.name_parts)?);
    }
    let (name_at, [short_at]) = push_named(&mut out, &m.name, std::array::from_ref(&m.short_name))?;
    pad(&mut out, 16);
    let mut lists_at = Vec::new();
    for list in m.lods.iter().flat_map(|l| &l.lists) {
        lists_at.push(out.len());
        out.extend(list);
    }
    pad(&mut out, 16);
    let mut tables_at = Vec::new();
    for lod in &m.lods {
        if let Some(masks) = &lod.bone_masks {
            pad(&mut out, 16);
            tables_at.push(out.len());
            masks.iter().for_each(|b| out.extend(b));
        } else if let Some(map) = &lod.bone_map {
            pad(&mut out, 4);
            tables_at.push(out.len());
            map.iter().for_each(|b| out.extend(b.to_le_bytes()));
        } else {
            tables_at.push(0);
        }
    }
    let ids_at = m.bone_ids.as_ref().map(|ids| {
        pad(&mut out, 16);
        let at = out.len();
        ids.iter().for_each(|b| out.extend(b.to_le_bytes()));
        at
    });
    let bounds_at = m.bone_bounds.as_ref().map(|bounds| {
        pad(&mut out, 16);
        let at = out.len();
        bounds.iter().for_each(|b| out.extend(b));
        at
    });
    pad(&mut out, 16);
    // the records' fields
    let mut s_i = 0usize;
    for (l, lod) in m.lods.iter().enumerate() {
        let at = nth(&lod_at, l, "LOD record")?;
        let field = |k: usize| plus(at, k);
        put(&mut out, field(8), &u32_of(lod.sections.len(), "sections")?)?;
        let first = nth_row(&sec_at, l)?.first().copied().unwrap_or(16);
        pointer(&mut out, field(0x0c), first)?;
        for (k, list) in lod.lists.iter().enumerate() {
            put(
                &mut out,
                crate::at(field(0x14), k, 12),
                &u32_of(list.len(), "list entries")?,
            )?;
            let list_at = nth(&lists_at, crate::at(k, l, LISTS), "list")?;
            pointer(&mut out, crate::at(field(0x18), k, 12), list_at)?;
        }
        let format: u32 = match lod.index_size {
            2 => 0x21,
            4 => 0x2e,
            n => return Err(bad(W, field(0x54), format!("LOD {l}: {n}-byte indices"))),
        };
        put(&mut out, field(0x54), &format.to_le_bytes())?;
        put(&mut out, field(0x58), &lod.index_bytes.to_le_bytes())?;
        put(&mut out, field(0x5c), &lod.vertex_bytes.to_le_bytes())?;
        put(&mut out, field(0x74), &lod.chunk)?;
        let (name_at, [path_at, last_at]) = nth(&lod_name_at, l, "LOD name")?;
        pointer(&mut out, field(0x8c), name_at)?;
        pointer(&mut out, field(0x94), path_at)?;
        pointer(&mut out, field(0x9c), last_at)?;
        match (lod_size, &lod.bone_masks, &lod.bone_map) {
            (LOD_MASKS, Some(_), _) => pointer(&mut out, field(0xb0), nth(&tables_at, l, "bone table")?)?,
            (LOD_MASKS, None, _) => put(&mut out, field(0xb0), &[0; 8])?,
            (LOD_MAP, _, Some(map)) => {
                put(&mut out, field(0xb0), &u32_of(map.len(), "mapped bones")?)?;
                pointer(&mut out, field(0xb4), nth(&tables_at, l, "bone table")?)?;
            }
            (LOD_MAP, _, None) => put(&mut out, field(0xb0), &[0; 12])?,
            _ => {}
        }
        for (s, sec) in lod.sections.iter().enumerate() {
            let o = nth(nth_row(&sec_at, l)?, s, "section record")?;
            let field = |k: usize| plus(o, k);
            pointer(&mut out, field(8), nth(&names_at, s_i, "section name")?)?;
            if sec.bones.is_empty() {
                put(&mut out, field(0x10), &[0; 8])?;
            } else {
                pointer(&mut out, field(0x10), nth(&bones_at, s_i, "bone list")?)?;
            }
            put(&mut out, field(0x18), &u16_of(sec.bones.len(), o, "bones")?)?;
            put(&mut out, field(0x1b), &[sec.flags])?;
            put(&mut out, field(0x1e), &[sec.stride])?;
            put(&mut out, field(0x20), &sec.primitives.to_le_bytes())?;
            put(&mut out, field(0x24), &sec.start_index.to_le_bytes())?;
            put(&mut out, field(0x140), &sec.start_index.to_le_bytes())?;
            put(&mut out, field(0x28), &sec.vertex_offset.to_le_bytes())?;
            put(&mut out, field(0x54), &sec.vertex_offset.to_le_bytes())?;
            put(&mut out, field(0x2c), &sec.vertices.to_le_bytes())?;
            put(&mut out, field(DECL_AT), &sec.decl.bytes)?;
            put(&mut out, field(DECL2_AT), &sec.draw_decl.bytes)?;
            put(&mut out, field(KEY_AT), &sec.key.to_le_bytes())?;
            s_i = s_i.saturating_add(1);
        }
    }
    for l in 0..MAX_LODS {
        let field = crate::at(0x30, l, 8);
        match lod_at.get(l) {
            Some(&at) => pointer(&mut out, field, at)?,
            None => put(&mut out, field, &[0; 8])?,
        }
    }
    put(&mut out, 0xb4, &u16_of(m.lods.len(), 0xb4, "LODs")?)?;
    pointer(&mut out, 0x68, name_at)?;
    pointer(&mut out, 0x70, short_at)?;
    if hs == HEADER_BONES {
        let bones = usize::from(Rd::new(&out, W).u16le(0xc4)?);
        let bounded = m.bone_ids.as_ref().map_or(0, Vec::len);
        let bounds_for = if m.bone_ids.is_some() { bounded } else { bones };
        if m.bone_bounds.as_ref().is_some_and(|b| b.len() != bounds_for) {
            return Err(bad(
                W,
                0xd0,
                format!("bone bounds for other than {bounds_for} bones"),
            ));
        }
        put(&mut out, 0xc6, &u16_of(bounded, 0xc6, "bounded bones")?)?;
        match ids_at {
            Some(at) => pointer(&mut out, 0xc8, at)?,
            None => put(&mut out, 0xc8, &[0; 8])?,
        }
        match bounds_at {
            Some(at) => pointer(&mut out, 0xd0, at)?,
            None => put(&mut out, 0xd0, &[0; 8])?,
        }
    }
    // the relocation table
    let table = out.len();
    for f in pointer_fields(m, &lod_at, &sec_at) {
        out.extend(u32_of(f.saturating_sub(16), "bytes")?);
    }
    let size = out.len().saturating_sub(table);
    pad(&mut out, 16);
    out.extend(&m.tail);
    let mut meta = [0u8; 16];
    put(&mut meta, 0, &u32_of(table, "bytes")?)?;
    put(&mut meta, 4, &u32_of(size, "bytes")?)?;
    put(&mut meta, 8, &u32_of(m.tail.len(), "bytes")?)?;
    Ok((out, meta))
}

/// Row `l` of the section offsets the writer built.
fn nth_row(rows: &[Vec<usize>], l: usize) -> Result<&[usize]> {
    rows.get(l)
        .map(Vec::as_slice)
        .ok_or_else(|| bad(W, l, format!("no section row {l} in the layout")))
}
