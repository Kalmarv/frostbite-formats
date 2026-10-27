//! A rigid MeshSet and its geometry chunk built from triangle meshes ([`build`]): one LOD, one section a part, laid out
//! as the game's own one-LOD Quick Drop meshes are (the 73 of `Win32/buildkititems.toc`, each one `default_mat`
//! section; 60 store these three streams, 13 add a sub-material index, usage 51). Rebuilding each of
//! the 60 from its decoded geometry (a check run on the game's files, not part of this repository's tests) gives: the chunk comes out byte-identical but for tangent frames with two encodings
//! (the same frame either way), and every record field but the measured ones below byte-identical.
//!
//! A section's vertices are three streams: half-float positions (w 1), half-float UV0 and the packed tangent frame
//! ([`tangent`]), 8, 4 and 4 bytes. Sections lie back to back in the vertex buffer (16-aligned at its end), their
//! indices (each part's own vertices) back to back after it, 2 bytes each (4 when a part has more than 65,536
//! vertices), padded to 4 (the LOD's index bytes), the chunk then padded to 16.
//!
//! Measured from the parts' own (unquantised) data:
//! - bounds: section +0x150 / +0x160 its vertices' (w 0), header 0x10 / 0x20 every section's;
//! - section +0x14c: the surface area in square metres (measured from the decoded triangles of the game's 1,222
//!   rigid Quick Drop sections with UV0 against the stored value: 1st..99th percentile 0.996..1.003);
//! - section +0x58: the median over the triangles with area of `sqrt(UV area / area)`, UV0 units a metre (the same
//!   sections: 25th..75th percentile 0.996..1.001, 5th..95th 0.849..1.024; it does not fit sections whose UVs fold
//!   many triangles to nothing, as the 60 `default_mat` meshes'); 1.0 when no triangle has area; +0x5c the same for
//!   UV1, which these sections have not: 1.0.
//!
//! Named by [`djb2`]: header 0x78 the MeshSet's name, LOD +0xa4 its path part (`<name>_lod0`), section +0x144 the
//! material's name lowercased. Each section is in the LOD's lists 0, 3 and 4 and has flags 0x0e, as the game's opaque
//! shadow-casting sections (its decal sections are in list 2 alone with flags 0x0a, its shadow proxies in 3 and 4
//! with flags 0x04). Every other byte is the value all 73 store.

use half::f16;

use crate::meshset::{self, Decl, Lod, MeshSet, Section, format, usage};
use crate::{Result, bad, djb2, tangent, u32_len};

const W: &str = "rigid mesh";
/// The section flags of an opaque, shadow-casting section.
pub const OPAQUE: u8 = 0x0e;
/// Bytes a vertex: position, UV0, tangent frame.
const STRIDES: [u8; 3] = [8, 4, 4];

/// One section: a material's triangles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// The material's name (section +0x08), its key (+0x138) and its index in the mesh asset's `Materials` (+0x1c).
    pub material: String,
    /// The material's key (section +0x138).
    pub key: u64,
    /// The material's index in the mesh asset's `Materials` list (section +0x1c).
    pub slot: u8,
    /// Vertex positions in metres; stored as half floats, so they are rounded on writing.
    pub positions: Vec<[f32; 3]>,
    /// Per vertex: the unit normal; the tangent and, in w, the bitangent's sign (the bitangent, `w * (normal x
    /// tangent)`, points where V grows, as in the game's meshes).
    pub normals: Vec<[f32; 3]>,
    /// Per vertex: the unit tangent in x, y, z and the bitangent's sign (+1 or -1) in w.
    pub tangents: Vec<[f32; 4]>,
    /// Per vertex: the UV0 coordinate; stored as half floats.
    pub uv: Vec<[f32; 2]>,
    /// Indices into this part's vertices.
    pub triangles: Vec<[u32; 3]>,
}

/// A built mesh: the MeshSet (write it with [`meshset::write`]) and its LOD's geometry chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    /// The MeshSet to write with [`meshset::write`]: one LOD with one section per part.
    pub meshset: MeshSet,
    /// The LOD's geometry chunk: the vertex streams, then the indices, padded to 16 bytes.
    pub chunk: Vec<u8>,
}

/// The declaration of the three streams.
pub fn declaration() -> Decl {
    let elements = [
        (usage::POSITION, format::HALF4),
        (usage::TEXCOORD0, format::HALF2),
        (usage::TANGENT_SPACE, format::UBYTE4N),
    ];
    let mut b = [0u8; meshset::DECL_SIZE];
    // 16 element slots of 4 bytes (usage, format, offset, stream; an unused slot is 0, 0, 0xff, 0), then 16
    // stream strides of 2 bytes, then the element and stream counts
    for ((slot, element), stream) in b
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .take(16)
        .zip(elements.map(Some).into_iter().chain(std::iter::repeat(None)))
        .zip(0u8..)
    {
        let [usage, format, offset, slot_stream] = slot;
        match element {
            Some((u, f)) => (*usage, *format, *slot_stream) = (u, f, stream),
            None => *offset = 0xff,
        }
    }
    for (slot, &stride) in b.as_chunks_mut::<2>().0.iter_mut().skip(32).zip(&STRIDES) {
        let [first, _] = slot;
        *first = stride;
    }
    if let Some([ne, ns]) = b.get_mut(96..98) {
        (*ne, *ns) = (3, 3);
    }
    Decl {
        elements: elements
            .iter()
            .zip(0u8..)
            .map(|(&(usage, format), stream)| meshset::Element {
                usage,
                format,
                offset: 0,
                stream,
            })
            .collect(),
        strides: STRIDES.to_vec(),
        bytes: b,
    }
}

/// Writes `v` at `at` of a record the builder sized itself.
fn put(b: &mut [u8], at: usize, v: &[u8]) -> Result<()> {
    let total = b.len();
    at.checked_add(v.len())
        .and_then(|end| b.get_mut(at..end))
        .ok_or_else(|| {
            bad(
                W,
                at,
                format!("{} bytes past the end of a {total}-byte record", v.len()),
            )
        })?
        .copy_from_slice(v);
    Ok(())
}

fn f32s(b: &mut [u8], at: usize, v: &[f32]) -> Result<()> {
    for (k, x) in v.iter().enumerate() {
        put(b, crate::at(at, k, 4), &x.to_le_bytes())?;
    }
    Ok(())
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let [a0, a1, a2] = a;
    let [b0, b1, b2] = b;
    [a0 - b0, a1 - b1, a2 - b2]
}

/// The area of the triangle `p`.
fn area(p: [[f64; 3]; 3]) -> f64 {
    let [p0, p1, p2] = p;
    let ([u0, u1, u2], [v0, v1, v2]) = (sub(p1, p0), sub(p2, p0));
    let c = [u1 * v2 - u2 * v1, u2 * v0 - u0 * v2, u0 * v1 - u1 * v0];
    0.5 * c.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// What [`build`] measures of a part (the module documentation's measures).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measures {
    /// Minimum corner of the part's positions, per axis.
    pub min: [f32; 3],
    /// Maximum corner of the part's positions, per axis.
    pub max: [f32; 3],
    /// Surface area, square metres.
    pub area: f32,
    /// UV units a metre: the median over the triangles with area of `sqrt(UV area / area)`; 1.0 when none has area.
    pub uv_density: f32,
}

/// The part's bounds, surface area and UV0 density. Refused: a triangle whose index is past the part's vertices.
pub fn measure(p: &Part) -> Result<Measures> {
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &p.positions {
        for ((lo, hi), c) in min.iter_mut().zip(&mut max).zip(v) {
            *lo = lo.min(*c);
            *hi = hi.max(*c);
        }
    }
    let (mut total, mut density) = (0.0f64, Vec::with_capacity(p.triangles.len()));
    for (k, t) in p.triangles.iter().enumerate() {
        let vertex = |i: u32| -> Result<([f64; 3], [f64; 3])> {
            let (pos, uv) = usize::try_from(i)
                .ok()
                .and_then(|i| p.positions.get(i).zip(p.uv.get(i)))
                .ok_or_else(|| {
                    bad(
                        W,
                        k,
                        format!(
                            "triangle {k}: index {i} past the {} vertices ({} UVs)",
                            p.positions.len(),
                            p.uv.len()
                        ),
                    )
                })?;
            Ok((pos.map(f64::from), [f64::from(uv[0]), f64::from(uv[1]), 0.0]))
        };
        let [(p0, t0), (p1, t1), (p2, t2)] = [vertex(t[0])?, vertex(t[1])?, vertex(t[2])?];
        let a = area([p0, p1, p2]);
        total += a;
        if a > 0.0 {
            density.push((area([t0, t1, t2]) / a).sqrt());
        }
    }
    density.sort_by(f64::total_cmp);
    Ok(Measures {
        min,
        max,
        area: narrow(total),
        uv_density: narrow(density.get(density.len() / 2).copied().unwrap_or(1.0)),
    })
}

/// A measure as the `f32` the record stores.
#[allow(clippy::cast_possible_truncation)] // rounds a computed measure to f32; no stored bytes are narrowed
fn narrow(v: f64) -> f32 {
    v as f32
}

/// The MeshSet named `name` (`<path>/<short name>`, as the game names them: `world/.../<x>_mesh`) of `parts`, its
/// LOD's geometry in the chunk `chunk`. Refused: no part, more than 255 parts, a part without triangles, streams of
/// unequal length, an index past its part's vertices, a value that is not finite, a normal without length.
pub fn build(name: &str, chunk: [u8; 16], parts: &[Part]) -> Result<Built> {
    if parts.is_empty() || parts.len() > 255 {
        return Err(bad(W, 0, format!("{} parts (1..255)", parts.len())));
    }
    for (i, p) in parts.iter().enumerate() {
        let n = p.positions.len();
        let refuse = |what: String| Err(bad(W, i, format!("part {i} ({}): {what}", p.material)));
        if p.normals.len() != n || p.tangents.len() != n || p.uv.len() != n {
            return refuse("streams of unequal length".into());
        }
        if p.triangles.is_empty() || n == 0 {
            return refuse("no triangles".into());
        }
        if let Some(t) = p
            .triangles
            .iter()
            .find(|t| t.iter().any(|&x| usize::try_from(x).map_or(true, |x| x >= n)))
        {
            return refuse(format!("triangle {t:?} past its {n} vertices"));
        }
        let finite = p.positions.iter().flatten().chain(p.normals.iter().flatten());
        if !finite
            .chain(p.tangents.iter().flatten())
            .chain(p.uv.iter().flatten())
            .all(|v| v.is_finite())
        {
            return refuse("a value that is not finite".into());
        }
    }
    let index_size: u8 = if parts.iter().all(|p| p.positions.len() <= 1 << 16) {
        2
    } else {
        4
    };
    let mut data = Vec::new();
    let mut sections = Vec::with_capacity(parts.len());
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    let decl = declaration();
    for (i, p) in parts.iter().enumerate() {
        let vertex_offset = u32::try_from(data.len()).map_err(|_| bad(W, i, "vertex buffer past 4 GiB"))?;
        let (vertices, primitives) = (u32_len(W, p.positions.len())?, u32_len(W, p.triangles.len())?);
        for v in &p.positions {
            for c in [v[0], v[1], v[2], 1.0] {
                data.extend(f16::from_f32(c).to_bits().to_le_bytes());
            }
        }
        for v in &p.uv {
            for c in v {
                data.extend(f16::from_f32(*c).to_bits().to_le_bytes());
            }
        }
        for (k, (n, t)) in p.normals.iter().zip(&p.tangents).enumerate() {
            let b = tangent::encode(*n, [t[0], t[1], t[2]], t[3])
                .ok_or_else(|| bad(W, i, format!("part {i}: vertex {k}'s normal has no length")))?;
            data.extend(b);
        }
        let Measures {
            min: plo,
            max: phi,
            area: surface,
            uv_density: density,
        } = measure(p)?;
        for ((lo, hi), (a, b)) in lo.iter_mut().zip(&mut hi).zip(plo.into_iter().zip(phi)) {
            *lo = lo.min(a);
            *hi = hi.max(b);
        }
        let mut record = vec![0u8; meshset::SECTION_SIZE];
        put(&mut record, 0x1a, &[0x30])?;
        put(&mut record, 0x1c, &[p.slot, 0])?;
        put(&mut record, 0x1f, &[3])?;
        put(&mut record, 0x48, &[0xff; 8])?;
        f32s(&mut record, 0x58, &[density, 1.0, 1.0, 1.0, 1.0, 1.0])?;
        put(
            &mut record,
            0x144,
            &djb2(&p.material.to_lowercase()).to_le_bytes(),
        )?;
        f32s(&mut record, 0x14c, &[surface])?;
        let ([x0, y0, z0], [x1, y1, z1]) = (plo, phi);
        f32s(&mut record, 0x150, &[x0, y0, z0, 0.0, x1, y1, z1, 0.0])?;
        sections.push(Section {
            record,
            name: p.material.clone(),
            key: p.key,
            decl: decl.clone(),
            draw_decl: decl.clone(),
            flags: OPAQUE,
            stride: STRIDES.iter().sum(),
            primitives,
            vertex_offset,
            vertices,
            ..Section::default()
        });
    }
    data.resize(crate::align(data.len(), 16), 0);
    let vertex_bytes = u32::try_from(data.len()).map_err(|_| bad(W, 0, "vertex buffer past 4 GiB"))?;
    let mut first = 0u32;
    for (p, s) in parts.iter().zip(&mut sections) {
        s.start_index = first;
        for &i in p.triangles.iter().flatten() {
            match index_size {
                2 => data.extend(
                    u16::try_from(i)
                        .map_err(|_| bad(W, 0, format!("index {i} does not fit a 2-byte index")))?
                        .to_le_bytes(),
                ),
                _ => data.extend(i.to_le_bytes()),
            }
        }
        let indices = p
            .triangles
            .len()
            .checked_mul(3)
            .and_then(|n| u32::try_from(n).ok());
        first = indices
            .and_then(|n| first.checked_add(n))
            .ok_or_else(|| bad(W, 0, "index count past 32 bits"))?;
    }
    data.resize(crate::align(data.len(), 4), 0);
    let index_bytes = u32_len(W, data.len())?
        .checked_sub(vertex_bytes)
        .ok_or_else(|| bad(W, 0, "index bytes before the vertex bytes"))?;
    // the chunk ends 16-aligned
    data.resize(crate::align(data.len(), 16), 0);
    let short_name = name.rfind('/').map_or(0, |i| i.saturating_add(1));
    let lod_name = format!("Mesh:{name}_lod0");
    let mut record = vec![0u8; meshset::LOD_PLAIN];
    put(&mut record, 0x04, &[0xff, 0xff, 0, 0])?;
    // the last (only) LOD
    put(&mut record, 0x50, &[0x41])?;
    put(&mut record, 0x84, &[0xff; 8])?;
    put(&mut record, 0xa4, &djb2(&format!("{name}_lod0")).to_le_bytes())?;
    let every: Vec<u8> =
        (0..u8::try_from(parts.len()).map_err(|_| bad(W, 0, "more than 255 parts"))?).collect();
    let lod = Lod {
        record,
        name: lod_name,
        name_parts: [
            meshset::NamePart::At("Mesh:".len()),
            meshset::NamePart::At("Mesh:".len().saturating_add(short_name)),
        ],
        lists: [every.clone(), Vec::new(), Vec::new(), every.clone(), every],
        chunk,
        index_size,
        index_bytes,
        vertex_bytes,
        sections,
        ..Lod::default()
    };
    let mut header = vec![0u8; meshset::HEADER];
    for (at, w) in [
        (0, meshset::HEADER - 16),
        (4, meshset::LOD_PLAIN),
        (8, meshset::SECTION_SIZE),
    ] {
        put(&mut header, at, &u32_len(W, w)?.to_le_bytes())?;
    }
    let ([x0, y0, z0], [x1, y1, z1]) = (lo, hi);
    f32s(&mut header, 0x10, &[x0, y0, z0, 0.0, x1, y1, z1, 0.0])?;
    put(&mut header, 0x78, &djb2(name).to_le_bytes())?;
    put(&mut header, 0x7c, &[0x00, 0x03, 0x00, 0xff])?;
    put(&mut header, 0x9c, &[0x00, 0x00, 0x01, 0x00, 0x01])?;
    put(&mut header, 0xa8, &[0x01, 0xff, 0xff, 0x80, 0x1e])?;
    put(&mut header, 0xb0, &[0x1f, 0x7f])?;
    // sections in all, and LOD 0's first (0)
    let section_count = u16::try_from(parts.len()).map_err(|_| bad(W, 0, "more than 65535 sections"))?;
    put(&mut header, 0xb6, &section_count.to_le_bytes())?;
    Ok(Built {
        meshset: MeshSet {
            header,
            name: name.to_owned(),
            short_name: meshset::NamePart::At(short_name),
            lods: vec![lod],
            ..MeshSet::default()
        },
        chunk: data,
    })
}
