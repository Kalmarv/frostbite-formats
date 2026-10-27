//! skate.'s PhysicsResource (resource type 0x41759364): the collision a PhysicsAsset or EcsPhysicsAsset names. Read
//! and written faithfully ([`read`], [`write()`]), and a triangle soup encoded as the triangle-mesh shapes ReSkate Studio
//! writes ([`weld`], [`mesh_shape`]).
//!
//! Layout (little endian; every offset is self-relative, the field's own position plus its value):
//!
//! ```text
//! 0x00 u32 version (2)   0x04 u32 magic 0x69AF7015   0x08 u32 (Studio 1, the game's own 0)   0x0C u32 parts
//! 0x10 u32 B (24-byte entries after the parts)   0x14 u32 0   0x18 u32 shapes   0x1C u32 D (44-byte entries)   0x20 u32 0
//! 0x24..0x38 five offsets: body (always 0x38), shape table, D table, part table, end of the part table
//! body: 0x4C bytes in Studio's files (copied from its template), none in the game's own
//! shape (84 bytes): f32x3 min, f32x3 max, u16 vertices, u16 corners, u16 faces, u16 tree nodes, u32 0,
//!   12 offsets: [0] vertices f32x3, [1] vertex -> corner offsets (vertices + 1 u16), [2] corner ids by vertex,
//!   [3] corner vertices u16, [4] corner edge value f32, [5] concave bits, [6] flat bits, [7] face offsets
//!   (faces + 1 u16), [8], [9] unused (0), [10] face planes (8 bytes per face, below), [11] the bounding volume tree
//!   (faces - 1 u64 nodes)
//! part (72 bytes): f32x4 rotation, f32x3 position, f32 1.0, u32, u32 shape kind, u32x3 ids, u32, u32 shape index,
//!   u32, u32 packed material, u32 flags (0x800)
//! then the arrays, by kind across all shapes in shape order: vertices, vertex offsets, corner ids, corner vertices;
//! 4-aligned before the first f32 array, then per shape its edge values, concave bits and flat bits; face offsets; and
//! 4-aligned face planes; where some shape has a tree, 8-aligned trees and 8 zero bytes; and where some shape has planes,
//! zero bytes to at least 56 past the last plane.
//! ```
//!
//! Face planes: the game's convex shapes (kind 3, and the boxes of kind 2) may carry them, one per face: its normal
//! octahedrally encoded as two snorm16 values, and its offset as f32. The zeros after them look like padding for a
//! reader that loads past the last plane; zero in every resource seen.
//!
//! Shapes of kind 5 are triangle meshes with edge data and a tree; the game's kind 3 shapes are convex polyhedra
//! (polygon faces, no edge data, no tree, zero bounds). The kind lives in the part, not the shape. Every fact here is
//! read from ReSkate Studio 20261012's writer (`reskate_cli`, unpacked: FUN_140160250, FUN_14015c790 and the functions
//! named below) and checked against Studio's output and the game's own resources (`tests/physics.rs`).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{Rd, Result, bad, capacity, u32_len};

const W: &str = "physics resource";
/// The resource type id of a PhysicsResource.
pub const RTYPE: u32 = 0x4175_9364;
/// The magic word at 0x04.
pub const MAGIC: u32 = 0x69AF_7015;
/// The shape table's offset field and the first byte of the body.
const BODY: usize = 0x38;
const SHAPE: usize = 0x54;
const PART: usize = 0x48;
/// The shape kind of a triangle mesh with edge data and a tree (Studio's only kind).
pub const KIND_MESH: u32 = 5;
/// The shape kind of a convex polyhedron (the game's own small parts).
pub const KIND_CONVEX: u32 = 3;
/// The shape kind of a box with its face planes (the game's own; [`Shape::planes`]).
pub const KIND_BOX: u32 = 2;
/// The most a shape's u16 fields hold: its vertices, and its corners (three per triangle).
pub const MAX_U16: usize = 0xFFFF;

/// A PhysicsResource: its header words, the body block, its shapes and its parts.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicsResource {
    pub version: u32,
    /// The word at 0x08: 1 in Studio's files, 0 in the game's own; meaning unknown.
    pub word08: u32,
    /// The block at 0x38: 0x4C bytes in some (ReSkate Studio writes its template's), empty in others.
    pub body: Vec<u8>,
    pub shapes: Vec<Shape>,
    pub parts: Vec<Part>,
}

/// A part's shape index when it names no shape of the resource.
pub const NO_SHAPE: u32 = u32::MAX;
/// The flags Studio and the game write into every part.
pub const PART_FLAGS: u32 = 0x800;

/// One part: a shape placed with its rotation and position, with its kind, ids and packed physics material.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub rotation: [f32; 4],
    pub position: [f32; 3],
    /// The f32 after the position (1.0 in every file seen).
    pub w: f32,
    pub word20: u32,
    pub kind: u32,
    pub ids: [u32; 3],
    pub word34: u32,
    pub shape: u32,
    pub word3c: u32,
    /// The packed physics material: `(material slot << 6) | flags` (ReSkate's `material_grid.h`).
    pub material: u32,
    pub flags: u32,
}

impl Part {
    /// A triangle-mesh part of `shape` at `position`, unrotated, with `ids` and the usual flags.
    pub fn mesh(shape: u32, position: [f32; 3], ids: [u32; 3], material: u32) -> Self {
        Self {
            rotation: [0.0, 0.0, 0.0, 1.0],
            position,
            w: 1.0,
            word20: 0,
            kind: KIND_MESH,
            ids,
            word34: 0,
            shape,
            word3c: 0,
            material,
            flags: PART_FLAGS,
        }
    }
}

/// What a corner's edge (from its vertex to the next corner's of its face) says about the face across it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// The cosine of half the angle between the two faces' normals on a convex edge, 1.0 otherwise.
    pub value: f32,
    pub concave: bool,
    pub flat: bool,
}

impl Edge {
    /// An edge that says nothing about its neighbour: no face across it, or none that pairs.
    pub const OPEN: Edge = Edge {
        value: 1.0,
        concave: false,
        flat: false,
    };
}

/// A shape: its local bounds, vertices and polygon faces, the edge data of every corner and its tree. The vertex ->
/// corner index is derived from the faces (read checks it, write builds it).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub vertices: Vec<[f32; 3]>,
    /// Face `f`'s corners are `corners[faces[f] as usize..faces[f + 1] as usize]`; `faces` starts at 0.
    pub faces: Vec<u16>,
    pub corners: Vec<u16>,
    /// One per corner, or none (a convex shape).
    pub edges: Vec<Edge>,
    /// The tree's nodes in heap order (faces - 1 of them), or none.
    pub tree: Vec<u64>,
    /// One plane per face, or none.
    pub planes: Vec<Plane>,
}

/// A face's plane as stored: its normal octahedrally encoded (two snorm16 values) and its offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub normal: [i16; 2],
    pub offset: f32,
}

impl Shape {
    /// The number of faces.
    pub fn face_count(&self) -> usize {
        self.faces.len().saturating_sub(1)
    }

    /// The vertex -> corner index: `offsets` (one per vertex and one more) into `ids`, the corners of each vertex in
    /// corner order. For a shape within the u16 limits ([`write()`] refuses the others); a corner that names no vertex
    /// is left out.
    pub fn corner_index(&self) -> (Vec<u16>, Vec<u16>) {
        let mut offsets = vec![0u16; self.vertices.len().saturating_add(1)];
        for &v in &self.corners {
            if let Some(o) = offsets.get_mut(usize::from(v).saturating_add(1)) {
                *o = o.saturating_add(1);
            }
        }
        let mut sum = 0u16;
        for o in &mut offsets {
            sum = sum.saturating_add(*o);
            *o = sum;
        }
        let mut next: Vec<u16> = offsets.iter().copied().take(self.vertices.len()).collect();
        let mut ids = vec![0u16; self.corners.len()];
        for (c, &v) in self.corners.iter().enumerate() {
            let (Ok(c), Some(n)) = (u16::try_from(c), next.get_mut(usize::from(v))) else {
                continue;
            };
            if let Some(id) = ids.get_mut(usize::from(*n)) {
                *id = c;
            }
            *n = n.saturating_add(1);
        }
        (offsets, ids)
    }
}

/// The absolute offset a self-relative field at `at` names; 0 (no array) stays 0.
fn rel(r: &Rd, at: usize) -> Result<usize> {
    let v = r.size32le(at)?;
    if v == 0 {
        return Ok(0);
    }
    at.checked_add(v).ok_or_else(|| {
        bad(
            W,
            at,
            format!("relative offset {v:#x} runs past the address space"),
        )
    })
}

fn f32s3(r: &Rd, at: usize) -> Result<[f32; 3]> {
    let word = |k: usize| -> Result<f32> { Ok(f32::from_bits(r.u32le(crate::at(at, k, 4))?)) };
    Ok([word(0)?, word(1)?, word(2)?])
}

fn u16s(r: &Rd, at: usize, n: usize) -> Result<Vec<u16>> {
    let b = r.bytes(at, n.saturating_mul(2))?;
    Ok(b.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect())
}

/// Bit `i` of the bit array at `at`.
fn bit(r: &Rd, at: usize, i: usize) -> Result<bool> {
    let word = r.u32le(crate::at(at, i / 32, 4))?;
    Ok(u32::try_from(i % 32).is_ok_and(|k| word.wrapping_shr(k) & 1 == 1))
}

/// The resource read: its header, body, shapes and parts. Refused: another magic or version, B or D entries, any of a
/// shape's unused offsets set, and a vertex -> corner index that disagrees with the faces (the writer derives it).
pub fn read(d: &[u8]) -> Result<PhysicsResource> {
    let r = Rd::new(d, W);
    let version = r.u32le(0)?;
    if r.u32le(4)? != MAGIC || version & !2 != 0 {
        return Err(bad(W, 0, "not a version 0 or 2 PhysicsResource"));
    }
    let (word08, nparts, nb, nshapes, nd) = (
        r.u32le(8)?,
        r.size32le(0xC)?,
        r.u32le(0x10)?,
        r.size32le(0x18)?,
        r.u32le(0x1C)?,
    );
    if nb != 0 || nd != 0 || r.u32le(0x14)? != 0 || r.u32le(0x20)? != 0 {
        return Err(bad(W, 0x10, format!("{nb} B and {nd} D entries (none read yet)")));
    }
    let mut tables = [0usize; 5];
    for (k, t) in tables.iter_mut().enumerate() {
        *t = rel(&r, crate::at(0x24, k, 4))?;
    }
    let [body_at, shape_at, d_at, part_at, end] = tables;
    if body_at != BODY
        || shape_at < BODY
        || d_at != crate::at(shape_at, nshapes, SHAPE)
        || part_at != d_at
        || end != crate::at(part_at, nparts, PART)
        || end > d.len()
    {
        return Err(bad(W, 0x24, "table offsets do not match the counts"));
    }
    let body = r.bytes(BODY, shape_at.saturating_sub(BODY))?.to_vec();
    let mut shapes = Vec::with_capacity(capacity(W, nshapes, SHAPE, d.len())?);
    for i in 0..nshapes {
        let s = crate::at(shape_at, i, SHAPE);
        let at_s = |k: usize| s.saturating_add(k);
        let nv = usize::from(r.u16le(at_s(0x18))?);
        let nc = usize::from(r.u16le(at_s(0x1A))?);
        let mut slots = [0usize; 12];
        for (k, t) in slots.iter_mut().enumerate() {
            *t = rel(&r, at_s(crate::at(0x24, k, 4)))?;
        }
        let [
            vertices_at,
            vertex_offsets_at,
            corner_ids_at,
            corner_vertices_at,
            edge_values_at,
            concave_at,
            flat_at,
            face_offsets_at,
            unused8,
            unused9,
            planes_at,
            tree_at,
        ] = slots;
        if unused8 != 0 || unused9 != 0 || r.u32le(at_s(0x20))? != 0 {
            return Err(bad(W, s, "a shape slot read nowhere yet is set"));
        }
        let vertices = (0..nv)
            .map(|v| f32s3(&r, crate::at(vertices_at, v, 12)))
            .collect::<Result<Vec<_>>>()?;
        let corners = u16s(&r, corner_vertices_at, nc)?;
        // faces in the low half of the word, tree nodes in the high half
        let (nf, nodes) = (
            usize::from(r.u16le(at_s(0x1C))?),
            usize::from(r.u16le(at_s(0x1E))?),
        );
        let faces = u16s(&r, face_offsets_at, nf.saturating_add(1))?;
        if faces.first() != Some(&0)
            || faces.last().map(|&l| usize::from(l)) != Some(nc)
            || faces.windows(2).any(|w| match w {
                [a, b] => usize::from(*b) < usize::from(*a).saturating_add(3),
                _ => true,
            })
        {
            return Err(bad(
                W,
                face_offsets_at,
                "face offsets do not run from 0 to the corners in steps of at least 3",
            ));
        }
        if corners.iter().any(|&v| usize::from(v) >= nv) {
            return Err(bad(
                W,
                corner_vertices_at,
                "a corner names a vertex past the vertices",
            ));
        }
        let edges = if edge_values_at != 0 {
            (0..nc)
                .map(|c| {
                    Ok(Edge {
                        value: f32::from_bits(r.u32le(crate::at(edge_values_at, c, 4))?),
                        concave: bit(&r, concave_at, c)?,
                        flat: bit(&r, flat_at, c)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        if (tree_at != 0) != (nodes != 0) || (nodes != 0 && nodes.saturating_add(1) != nf) {
            return Err(bad(W, at_s(0x1C), format!("{nodes} tree nodes for {nf} faces")));
        }
        let tree = if tree_at != 0 {
            (0..nodes)
                .map(|k| r.u64le(crate::at(tree_at, k, 8)))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let planes = if planes_at != 0 {
            (0..nf)
                .map(|f| {
                    let o = crate::at(planes_at, f, 8);
                    Ok(Plane {
                        normal: [
                            i16::from_le_bytes(r.arr(o)?),
                            i16::from_le_bytes(r.arr(o.saturating_add(2))?),
                        ],
                        offset: f32::from_bits(r.u32le(o.saturating_add(4))?),
                    })
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let shape = Shape {
            min: f32s3(&r, s)?,
            max: f32s3(&r, at_s(12))?,
            vertices,
            faces,
            corners,
            edges,
            tree,
            planes,
        };
        let (offsets, ids) = shape.corner_index();
        if u16s(&r, vertex_offsets_at, nv.saturating_add(1))? != offsets
            || u16s(&r, corner_ids_at, nc)? != ids
        {
            return Err(bad(
                W,
                vertex_offsets_at,
                "the vertex -> corner index disagrees with the faces",
            ));
        }
        shapes.push(shape);
    }
    let mut parts = Vec::with_capacity(nparts);
    for i in 0..nparts {
        let o = crate::at(part_at, i, PART);
        let word = |k: usize| r.u32le(o.saturating_add(k));
        let f = |k: usize| -> Result<f32> { Ok(f32::from_bits(word(k)?)) };
        parts.push(Part {
            rotation: [f(0)?, f(4)?, f(8)?, f(0xC)?],
            position: [f(0x10)?, f(0x14)?, f(0x18)?],
            w: f(0x1C)?,
            word20: word(0x20)?,
            kind: word(0x24)?,
            ids: [word(0x28)?, word(0x2C)?, word(0x30)?],
            word34: word(0x34)?,
            shape: word(0x38)?,
            word3c: word(0x3C)?,
            material: word(0x40)?,
            flags: word(0x44)?,
        });
    }
    Ok(PhysicsResource {
        version,
        word08,
        body,
        shapes,
        parts,
    })
}

/// Writes `v` at `at` of the resource being built.
fn put(out: &mut [u8], at: usize, v: &[u8]) -> Result<()> {
    let total = out.len();
    at.checked_add(v.len())
        .and_then(|end| out.get_mut(at..end))
        .ok_or_else(|| {
            bad(
                W,
                at,
                format!("{} bytes past the end of the {total}-byte resource", v.len()),
            )
        })?
        .copy_from_slice(v);
    Ok(())
}

fn put_u32(out: &mut [u8], at: usize, v: u32) -> Result<()> {
    put(out, at, &v.to_le_bytes())
}

/// `n` as a u16 field; an error when it does not fit.
fn u16_len(n: usize) -> Result<u16> {
    u16::try_from(n).map_err(|_| bad(W, 0, format!("{n} does not fit a 16-bit field")))
}

/// The resource written in the layout of the module documentation (the order Studio's writer and the game's own
/// resources use). Refused: a shape past the u16 limits, edge data for some corners only, a tree of the wrong size, a
/// part naming a missing shape, a resource past the 32-bit offsets.
pub fn write(p: &PhysicsResource) -> Result<Vec<u8>> {
    for (i, s) in p.shapes.iter().enumerate() {
        let nf = s.face_count();
        if s.vertices.len() > MAX_U16 || s.corners.len() > MAX_U16 || nf == 0 || nf > MAX_U16 {
            return Err(bad(W, i, "a shape holds no face or more than u16 storage"));
        }
        if !(s.edges.is_empty() || s.edges.len() == s.corners.len()) {
            return Err(bad(W, i, "edge data for some corners only"));
        }
        if !(s.tree.is_empty() || s.tree.len() == nf.saturating_sub(1)) {
            return Err(bad(W, i, format!("{} tree nodes for {nf} faces", s.tree.len())));
        }
        if !(s.planes.is_empty() || s.planes.len() == nf) {
            return Err(bad(W, i, format!("{} planes for {nf} faces", s.planes.len())));
        }
    }
    if let Some(part) = p
        .parts
        .iter()
        .find(|q| q.shape != NO_SHAPE && usize::try_from(q.shape).map_or(true, |i| i >= p.shapes.len()))
    {
        return Err(bad(
            W,
            usize::try_from(part.shape).unwrap_or(usize::MAX),
            "a part names a shape past the shapes",
        ));
    }
    let too_big = || bad(W, 0, "the resource does not fit the 32-bit offsets");
    let shape_at = BODY.checked_add(p.body.len()).ok_or_else(too_big)?;
    let part_at = p
        .shapes
        .len()
        .checked_mul(SHAPE)
        .and_then(|n| shape_at.checked_add(n))
        .ok_or_else(too_big)?;
    let end = p
        .parts
        .len()
        .checked_mul(PART)
        .and_then(|n| part_at.checked_add(n))
        .ok_or_else(too_big)?;
    let mut out = vec![0u8; end];
    put_u32(&mut out, 0, p.version)?;
    put_u32(&mut out, 4, MAGIC)?;
    put_u32(&mut out, 8, p.word08)?;
    put_u32(&mut out, 0xC, u32_len(W, p.parts.len())?)?;
    put_u32(&mut out, 0x18, u32_len(W, p.shapes.len())?)?;
    for (k, target) in [BODY, shape_at, part_at, part_at, end].into_iter().enumerate() {
        let at = crate::at(0x24, k, 4);
        put_u32(&mut out, at, u32_len(W, target.saturating_sub(at))?)?;
    }
    put(&mut out, BODY, &p.body)?;
    for (i, s) in p.shapes.iter().enumerate() {
        let o = crate::at(shape_at, i, SHAPE);
        for (k, v) in s.min.iter().chain(&s.max).enumerate() {
            put_u32(&mut out, crate::at(o, k, 4), v.to_bits())?;
        }
        put(
            &mut out,
            o.saturating_add(0x18),
            &u16_len(s.vertices.len())?.to_le_bytes(),
        )?;
        put(
            &mut out,
            o.saturating_add(0x1A),
            &u16_len(s.corners.len())?.to_le_bytes(),
        )?;
        // faces in the low half of the word, tree nodes in the high half
        put(
            &mut out,
            o.saturating_add(0x1C),
            &u16_len(s.face_count())?.to_le_bytes(),
        )?;
        put(
            &mut out,
            o.saturating_add(0x1E),
            &u16_len(s.tree.len())?.to_le_bytes(),
        )?;
    }
    for (i, q) in p.parts.iter().enumerate() {
        let o = crate::at(part_at, i, PART);
        for (k, v) in q.rotation.iter().chain(&q.position).chain([&q.w]).enumerate() {
            put_u32(&mut out, crate::at(o, k, 4), v.to_bits())?;
        }
        let [id0, id1, id2] = q.ids;
        for (at, v) in [
            (0x20, q.word20),
            (0x24, q.kind),
            (0x28, id0),
            (0x2C, id1),
            (0x30, id2),
            (0x34, q.word34),
            (0x38, q.shape),
            (0x3C, q.word3c),
            (0x40, q.material),
            (0x44, q.flags),
        ] {
            put_u32(&mut out, o.saturating_add(at), v)?;
        }
    }
    let slot = |i: usize, k: usize| crate::at(crate::at(shape_at, i, SHAPE).saturating_add(0x24), k, 4);
    // Appends `data` and stores its self-relative offset in `field`.
    let place = |out: &mut Vec<u8>, field: usize, data: &[u8]| -> Result<()> {
        let at = u32_len(W, out.len().saturating_sub(field))?;
        put_u32(out, field, at)?;
        out.extend_from_slice(data);
        Ok(())
    };
    let le16 = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let index: Vec<(Vec<u16>, Vec<u16>)> = p.shapes.iter().map(Shape::corner_index).collect();
    for (i, s) in p.shapes.iter().enumerate() {
        let v: Vec<u8> = s
            .vertices
            .iter()
            .flatten()
            .flat_map(|x| x.to_bits().to_le_bytes())
            .collect();
        place(&mut out, slot(i, 0), &v)?;
    }
    for (i, (offsets, _)) in index.iter().enumerate() {
        place(&mut out, slot(i, 1), &le16(offsets))?;
    }
    for (i, (_, ids)) in index.iter().enumerate() {
        place(&mut out, slot(i, 2), &le16(ids))?;
    }
    for (i, s) in p.shapes.iter().enumerate() {
        place(&mut out, slot(i, 3), &le16(&s.corners))?;
    }
    if p.shapes.iter().any(|s| !s.edges.is_empty()) {
        out.resize(crate::align(out.len(), 4), 0);
    }
    // 32 flags to a little-endian word, the first flag its lowest bit
    let bits = |flags: Vec<bool>| {
        flags
            .chunks(32)
            .flat_map(|c| {
                c.iter()
                    .rev()
                    .fold(0u32, |w, &f| w.wrapping_shl(1) | u32::from(f))
                    .to_le_bytes()
            })
            .collect::<Vec<u8>>()
    };
    for (i, s) in p.shapes.iter().enumerate() {
        if s.edges.is_empty() {
            continue;
        }
        let values: Vec<u8> = s
            .edges
            .iter()
            .flat_map(|e| e.value.to_bits().to_le_bytes())
            .collect();
        place(&mut out, slot(i, 4), &values)?;
        place(
            &mut out,
            slot(i, 5),
            &bits(s.edges.iter().map(|e| e.concave).collect()),
        )?;
        place(
            &mut out,
            slot(i, 6),
            &bits(s.edges.iter().map(|e| e.flat).collect()),
        )?;
    }
    for (i, s) in p.shapes.iter().enumerate() {
        place(&mut out, slot(i, 7), &le16(&s.faces))?;
    }
    let planes = p.shapes.iter().any(|s| !s.planes.is_empty());
    if planes {
        out.resize(crate::align(out.len(), 4), 0);
    }
    for (i, s) in p.shapes.iter().enumerate() {
        if s.planes.is_empty() {
            continue;
        }
        let bytes: Vec<u8> = s
            .planes
            .iter()
            .flat_map(|q| {
                let [a0, a1] = q.normal[0].to_le_bytes();
                let [b0, b1] = q.normal[1].to_le_bytes();
                let [o0, o1, o2, o3] = q.offset.to_bits().to_le_bytes();
                [a0, a1, b0, b1, o0, o1, o2, o3]
            })
            .collect();
        place(&mut out, slot(i, 10), &bytes)?;
    }
    let planes_end = out.len();
    if p.shapes.iter().any(|s| !s.tree.is_empty()) {
        out.resize(crate::align(out.len(), 8), 0);
        for (i, s) in p.shapes.iter().enumerate() {
            if s.tree.is_empty() {
                continue;
            }
            let nodes: Vec<u8> = s.tree.iter().flat_map(|n| n.to_le_bytes()).collect();
            place(&mut out, slot(i, 11), &nodes)?;
        }
        out.extend_from_slice(&[0u8; 8]);
    }
    // the resource runs at least 56 bytes past the planes
    if planes && out.len() < planes_end.saturating_add(56) {
        out.resize(planes_end.saturating_add(56), 0);
    }
    Ok(out)
}

// ---- encoding a triangle soup as Studio does ----------------------------------------------------------------------

/// A triangle soup welded as Studio welds one collision object (FUN_140161790): vertices of exactly equal position
/// (-0.0 taken as 0.0) are one, numbered by first use and keeping the first use's value; a triangle with two corners on
/// one vertex or no area is dropped; each triangle starts at its smallest vertex (its winding kept); a triangle equal to
/// an earlier one (same corners in the same order) is dropped. `source[t]` is the input triangle each kept one came
/// from.
#[derive(Clone, Debug, PartialEq)]
pub struct Welded {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub source: Vec<usize>,
    /// Input triangles dropped: degenerate (two corners welded together, or no area) and repeated.
    pub degenerate: usize,
    pub repeated: usize,
}

/// The position vertex `i` names, or the error for an index outside the vertex buffer.
fn position(positions: &[[f32; 3]], i: u32) -> Result<[f32; 3]> {
    let at = usize::try_from(i).unwrap_or(usize::MAX);
    positions
        .get(at)
        .copied()
        .ok_or_else(|| bad(W, at, "collision index is outside the vertex buffer"))
}

/// `triangles` of `positions` welded (see [`Welded`]). An index past the positions or a position that is not finite is
/// an error.
pub fn weld(positions: &[[f32; 3]], triangles: &[[u32; 3]]) -> Result<Welded> {
    let mut by_pos: HashMap<[u32; 3], u32> = HashMap::new();
    let mut w = Welded {
        positions: Vec::new(),
        triangles: Vec::new(),
        source: Vec::new(),
        degenerate: 0,
        repeated: 0,
    };
    let mut seen: HashSet<[u32; 3]> = HashSet::new();
    for (t, tri) in triangles.iter().enumerate() {
        let mut ids = [0u32; 3];
        for (id, &i) in ids.iter_mut().zip(tri) {
            let p = position(positions, i)
                .map_err(|_| bad(W, t, "collision index is outside the vertex buffer"))?;
            if p.iter().any(|c| !c.is_finite()) {
                return Err(bad(W, t, "collision vertex is not finite"));
            }
            // -0.0 and 0.0 are one position
            let key = p.map(|c| if c == 0.0 { 0 } else { c.to_bits() });
            let next = u32_len(W, w.positions.len())?;
            *id = *by_pos.entry(key).or_insert_with(|| {
                w.positions.push(p);
                next
            });
        }
        let [a, b, c] = ids;
        let [pa, pb, pc] = ids.map(|i| position(&w.positions, i));
        if a == b || b == c || a == c || area2([pa?, pb?, pc?]) <= 0.0 {
            w.degenerate = w.degenerate.saturating_add(1);
            continue;
        }
        let first = ids
            .iter()
            .enumerate()
            .min_by_key(|&(_, &v)| v)
            .map_or(0, |(k, _)| k);
        ids.rotate_left(first);
        if !seen.insert(ids) {
            w.repeated = w.repeated.saturating_add(1);
            continue;
        }
        w.triangles.push(ids);
        w.source.push(t);
    }
    Ok(w)
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let ([a0, a1, a2], [b0, b1, b2]) = (a, b);
    [a0 - b0, a1 - b1, a2 - b2]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    let ([a0, a1, a2], [b0, b1, b2]) = (a, b);
    a0 * b0 + a1 * b1 + a2 * b2
}

/// The length of the cross product of a triangle's edges, in f64 (Studio's area test).
fn area2(t: [[f32; 3]; 3]) -> f64 {
    let [a, b, c] = t.map(|p| p.map(f64::from));
    let ([u0, u1, u2], [v0, v1, v2]) = (sub3(b, a), sub3(c, a));
    let x = [u1 * v2 - u2 * v1, u2 * v0 - u0 * v2, u0 * v1 - u1 * v0];
    dot(x, x).sqrt()
}

/// A computed measure as the `f32` the encoder stores.
#[allow(clippy::cast_possible_truncation)] // rounds a computed f64 to f32; no stored bytes are narrowed
fn narrow(v: f64) -> f32 {
    v as f32
}

/// `v` as an index into `0..bins`, clamped.
#[allow(clippy::cast_possible_truncation)] // a computed float clamped to a bin index of at most 1023
fn bin(v: f64, bins: u32) -> u32 {
    (v.floor() as i64).clamp(0, i64::from(bins.saturating_sub(1))) as u32
}

/// One shape of welded triangles (a batch: one material's triangles of a welded object, at most [`MAX_U16`] corners),
/// encoded as Studio's FUN_140158c20 and FUN_14015c790 do: the vertices recentred on the middle of their bounds (that
/// middle is the part's position, returned), triangles with no area after recentring dropped, the vertices the rest
/// use renumbered in first-use order, every corner's edge data ([`edges`]), and the tree with the faces in
/// its leaf order. `None` when no triangle is left.
pub fn mesh_shape(positions: &[[f32; 3]], triangles: &[[u32; 3]]) -> Result<Option<(Shape, [f32; 3])>> {
    if triangles.len() > MAX_U16 / 3 {
        return Err(bad(
            W,
            triangles.len(),
            "physics mesh face batch is outside native uint16 storage",
        ));
    }
    let mut used: Vec<u32> = Vec::new();
    let mut seen = HashSet::new();
    for t in triangles {
        for &v in t {
            position(positions, v)?;
            if seen.insert(v) {
                used.push(v);
            }
        }
    }
    if used.is_empty() {
        return Ok(None);
    }
    // the middle of the used vertices' bounds, in f64 then f32
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for &v in &used {
        for ((l, h), c) in lo.iter_mut().zip(&mut hi).zip(position(positions, v)?) {
            let c = f64::from(c);
            if c < *l {
                *l = c;
            }
            if *h < c {
                *h = c;
            }
        }
    }
    let mut centre = [0f32; 3];
    for ((m, l), h) in centre.iter_mut().zip(lo).zip(hi) {
        *m = narrow((l + h) * 0.5);
    }
    if centre.iter().any(|c| !c.is_finite()) {
        return Err(bad(W, 0, "collision value is outside finite float32 range"));
    }
    let local = |v: u32| -> Result<[f32; 3]> {
        let [x, y, z] = position(positions, v)?;
        let [cx, cy, cz] = centre;
        Ok([x - cx, y - cy, z - cz])
    };
    let mut remap: BTreeMap<u32, u16> = BTreeMap::new();
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut corners: Vec<u16> = Vec::new();
    for t in triangles {
        let [a, b, c] = *t;
        if area2([local(a)?, local(b)?, local(c)?]) <= 0.0 {
            continue;
        }
        for &v in t {
            let id = match remap.get(&v) {
                Some(&id) => id,
                None => {
                    let id = u16::try_from(vertices.len()).map_err(|_| {
                        bad(
                            W,
                            vertices.len(),
                            "collision geometry exceeds native uint16 storage",
                        )
                    })?;
                    vertices.push(local(v)?);
                    remap.insert(v, id);
                    id
                }
            };
            corners.push(id);
        }
    }
    if corners.is_empty() {
        return Ok(None);
    }
    let faces: Vec<u16> = (0..=corners.len() / 3)
        .map(|f| u16_len(f.saturating_mul(3)))
        .collect::<Result<_>>()?;
    let spans = face_spans(&faces);
    let bounds = bounds_of(vertices.iter().copied());
    let edge = edges(&vertices, &faces, &corners)?;
    let (tree, order) = if spans.len() > 1 {
        let recs: Vec<FaceRecord> = spans
            .iter()
            .enumerate()
            .map(|(f, &(a, b))| {
                let ps: Vec<[f32; 3]> = corners
                    .get(a..b)
                    .ok_or_else(|| bad(W, a, "face past the corners"))?
                    .iter()
                    .map(|&v| vertex(&vertices, v))
                    .collect::<Result<_>>()?;
                let n = ps.len() as f64;
                let sum = ps.iter().fold([0f64; 3], |acc, p| {
                    let ([a0, a1, a2], [p0, p1, p2]) = (acc, p.map(f64::from));
                    [a0 + p0, a1 + p1, a2 + p2]
                });
                Ok(FaceRecord {
                    bounds: bounds_of(ps.iter().copied()),
                    centroid: sum.map(|x| x / n),
                    index: u32_len(W, f)?,
                })
            })
            .collect::<Result<_>>()?;
        tree(recs, bounds)?
    } else {
        (Vec::new(), vec![0u32])
    };
    let [x0, y0, z0, x1, y1, z1] = bounds;
    let mut out = Shape {
        min: [x0, y0, z0],
        max: [x1, y1, z1],
        vertices,
        faces: vec![0],
        corners: Vec::with_capacity(corners.len()),
        edges: Vec::with_capacity(corners.len()),
        tree,
        planes: Vec::new(),
    };
    for f in order {
        let &(a, b) = usize::try_from(f)
            .ok()
            .and_then(|f| spans.get(f))
            .ok_or_else(|| bad(W, 0, "tree leaf names no face"))?;
        let (cs, es) = corners
            .get(a..b)
            .zip(edge.get(a..b))
            .ok_or_else(|| bad(W, a, "face past the corners"))?;
        out.corners.extend_from_slice(cs);
        out.edges.extend_from_slice(es);
        out.faces.push(u16_len(out.corners.len())?);
    }
    Ok(Some((out, centre)))
}

/// Each face's corner range `(first, end)` from its offsets.
fn face_spans(faces: &[u16]) -> Vec<(usize, usize)> {
    faces
        .windows(2)
        .filter_map(|w| match w {
            [a, b] => Some((usize::from(*a), usize::from(*b))),
            _ => None,
        })
        .collect()
}

/// The position of vertex `v`, or the error for a corner that names none.
fn vertex(vertices: &[[f32; 3]], v: u16) -> Result<[f32; 3]> {
    vertices.get(usize::from(v)).copied().ok_or_else(|| {
        bad(
            W,
            usize::from(v),
            format!("a corner names vertex {v} of {}", vertices.len()),
        )
    })
}

/// min x, y, z, max x, y, z of the points; a tie keeps the earlier value (Studio's FUN_140158550 and FUN_140158400
/// compare `<=` towards the kept one, which only matters for -0.0 against 0.0).
fn bounds_of(points: impl Iterator<Item = [f32; 3]>) -> [f32; 6] {
    let mut b: Option<[f32; 6]> = None;
    for [x, y, z] in points {
        match &mut b {
            None => b = Some([x, y, z, x, y, z]),
            Some(b) => grow(b, [x, y, z, x, y, z]),
        }
    }
    b.unwrap_or([0.0; 6])
}

/// `b` grown to hold `r` (boxes as min x, y, z, max x, y, z); a tie keeps `b`'s value.
fn grow(b: &mut [f32; 6], r: [f32; 6]) {
    let [bl0, bl1, bl2, bh0, bh1, bh2] = b;
    let [rl0, rl1, rl2, rh0, rh1, rh2] = r;
    for (kept, new) in [(bl0, rl0), (bl1, rl1), (bl2, rl2)] {
        if new < *kept {
            *kept = new;
        }
    }
    for (kept, new) in [(bh0, rh0), (bh1, rh1), (bh2, rh2)] {
        if *kept < new {
            *kept = new;
        }
    }
}

/// Every corner's [`Edge`] (FUN_14015dcf0): a corner's edge runs from its vertex to the next corner's. When exactly
/// one corner holds that directed edge and exactly one holds the opposite one, the two faces' normals (Newell's,
/// normalised; neither may be zero) give `d`, their dot product clamped to [-1, 1]: the edge is flat when `d >=
/// cos(0.1 deg)`; else, when the other face's centroid lies on or above this face's plane (through the corner's
/// vertex), it is concave if `d > 0`; else (convex) its value is the cosine of half the angle, `cos(acos(d) / 2)`.
/// Every other edge is [`Edge::OPEN`]. Refused: face offsets that do not run from 0 over every corner in order, a
/// corner that names no vertex.
pub fn edges(vertices: &[[f32; 3]], faces: &[u16], corners: &[u16]) -> Result<Vec<Edge>> {
    let nc = corners.len();
    let spans = face_spans(faces);
    let mut cursor = faces.first().map_or(0, |&f| usize::from(f));
    if cursor != 0 {
        return Err(bad(W, 0, format!("face offsets start at corner {cursor}")));
    }
    for &(a, b) in &spans {
        if a != cursor || b < a || b > nc {
            return Err(bad(
                W,
                a,
                format!("face offsets {a}..{b} do not follow on from corner {cursor} of {nc}"),
            ));
        }
        cursor = b;
    }
    if cursor != nc {
        return Err(bad(
            W,
            cursor,
            format!("face offsets end at corner {cursor} of {nc}"),
        ));
    }
    let mut face_of: Vec<usize> = Vec::with_capacity(nc);
    // the vertices each corner's edge runs between
    let mut ends: Vec<(u16, u16)> = Vec::with_capacity(nc);
    let mut normals = Vec::with_capacity(spans.len());
    let mut centroids = Vec::with_capacity(spans.len());
    for (f, &(a, b)) in spans.iter().enumerate() {
        let poly = corners.get(a..b).unwrap_or_default();
        let mut s = [0f64; 3];
        for &c in poly {
            let [x, y, z] = vertex(vertices, c)?.map(f64::from);
            let [s0, s1, s2] = s;
            s = [s0 + x, s1 + y, s2 + z];
        }
        let n = poly.len() as f64;
        centroids.push(s.map(|x| x / n));
        normals.push(newell(vertices, poly)?);
        face_of.extend(poly.iter().map(|_| f));
        ends.extend(
            poly.iter()
                .zip(poly.iter().cycle().skip(1))
                .map(|(&a, &b)| (a, b)),
        );
    }
    let mut holders: HashMap<(u16, u16), (usize, usize)> = HashMap::new();
    for (c, &e) in ends.iter().enumerate() {
        let h = holders.entry(e).or_insert((c, 0));
        h.1 = h.1.saturating_add(1);
    }
    let cos_flat = 0.001_745_329_251_994_329_6f64.cos();
    // a corner's face's normal and centroid
    let face_data = |c: usize| -> Result<([f64; 3], [f64; 3])> {
        face_of
            .get(c)
            .and_then(|&f| normals.get(f).copied().zip(centroids.get(f).copied()))
            .ok_or_else(|| bad(W, c, "a corner without a face"))
    };
    ends.iter()
        .enumerate()
        .map(|(c, &(a, b))| {
            if holders.get(&(a, b)).map(|h| h.1) != Some(1) {
                return Ok(Edge::OPEN);
            }
            let Some(&(o, 1)) = holders.get(&(b, a)) else {
                return Ok(Edge::OPEN);
            };
            let ((n1, _), (n2, c2)) = (face_data(c)?, face_data(o)?);
            if dot(n1, n1).sqrt() == 0.0 || dot(n2, n2).sqrt() == 0.0 {
                return Ok(Edge::OPEN);
            }
            let d = dot(n1, n2).clamp(-1.0, 1.0);
            let flat = cos_flat <= d;
            let va = vertex(vertices, a)?.map(f64::from);
            let above = dot(sub3(c2, va), n1) >= 0.0;
            Ok(if flat {
                Edge {
                    value: 1.0,
                    concave: false,
                    flat: true,
                }
            } else if above {
                Edge {
                    value: 1.0,
                    concave: d > 0.0,
                    flat: false,
                }
            } else {
                Edge {
                    value: narrow((d.acos() * 0.5).cos()),
                    concave: false,
                    flat: false,
                }
            })
        })
        .collect()
}

/// A polygon's unit normal by Newell's method in f64 (FUN_14015ef10), zero when it has no area.
fn newell(vertices: &[[f32; 3]], poly: &[u16]) -> Result<[f64; 3]> {
    let (mut x, mut y, mut z) = (0f64, 0f64, 0f64);
    for (&i, &j) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        let [a0, a1, a2] = vertex(vertices, i)?.map(f64::from);
        let [b0, b1, b2] = vertex(vertices, j)?.map(f64::from);
        z += (a0 - b0) * (b1 + a1);
        x += (a1 - b1) * (b2 + a2);
        y += (a2 - b2) * (b0 + a0);
    }
    let l = (x * x + y * y + z * z).sqrt();
    Ok(if l <= 0.0 { [0.0; 3] } else { [x / l, y / l, z / l] })
}

// ---- the tree (FUN_140158700, FUN_14015c4b0, FUN_14015b6b0, FUN_14015f0f0) ------------------------------------------

#[derive(Clone, Copy, Debug)]
struct FaceRecord {
    bounds: [f32; 6],
    centroid: [f64; 3],
    index: u32,
}

/// A total order of floats by `<` and `==` (as the C++ comparators: -0.0 equals 0.0; no NaN reaches here).
fn order_f64(a: f64, b: f64) -> std::cmp::Ordering {
    if a < b {
        std::cmp::Ordering::Less
    } else if b < a {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

/// The box holding every record's.
fn union(recs: &[FaceRecord]) -> Result<[f32; 6]> {
    let (first, rest) = recs
        .split_first()
        .ok_or_else(|| bad(W, 0, "a tree node without faces"))?;
    let mut b = first.bounds;
    for r in rest {
        grow(&mut b, r.bounds);
    }
    Ok(b)
}

/// How many leaves the left child of heap node `i` holds in a complete binary tree whose `n` leaves are nodes
/// `n - 1 ..= 2n - 2`.
fn left_leaves(i: usize, n: usize) -> usize {
    let (first, last) = (n.saturating_sub(1), n.saturating_mul(2).saturating_sub(2));
    let (mut lo, mut width, mut count) = (i.saturating_mul(2).saturating_add(1), 1usize, 0usize);
    while lo <= last {
        let (a, b) = (
            lo.max(first),
            lo.saturating_add(width).saturating_sub(1).min(last),
        );
        if a <= b {
            count = count.saturating_add(b.saturating_sub(a)).saturating_add(1);
        }
        lo = lo.saturating_mul(2).saturating_add(1);
        width = width.saturating_mul(2);
    }
    count
}

/// The tree over the faces: node `i`'s children are `2i + 1` and `2i + 2`; a node splits its faces sorted by centroid
/// on the axis their centroids spread most along (ties: by bounds' min x, then index), the left taking as many as its
/// subtree has leaves; when the right half then starts lower in x than the left, the halves swap places (the last
/// faces become the left), and if that still holds the faces are sorted by min x (then index). Each node holds both
/// children's boxes quantised in its own box ([`node`]); a child's box is what its parent's node decodes to. Returns
/// the nodes and the faces in leaf order.
fn tree(recs: Vec<FaceRecord>, root: [f32; 6]) -> Result<(Vec<u64>, Vec<u32>)> {
    let n = recs.len();
    let inner = n.saturating_sub(1);
    let mut nodes = vec![0u64; inner];
    let mut order = vec![0u32; n];
    let mut stack = vec![(0usize, recs, root)];
    while let Some((i, mut faces, parent)) = stack.pop() {
        if i >= inner {
            let (face, slot) = faces
                .first()
                .zip(order.get_mut(i.saturating_sub(inner)))
                .ok_or_else(|| bad(W, i, "a tree leaf outside the leaf order"))?;
            *slot = face.index;
            continue;
        }
        let nl = left_leaves(i, n);
        if nl > faces.len() {
            return Err(bad(
                W,
                i,
                format!("node {i} splits off {nl} of {} faces", faces.len()),
            ));
        }
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for f in &faces {
            for ((l, h), c) in lo.iter_mut().zip(&mut hi).zip(f.centroid) {
                *l = l.min(c);
                *h = h.max(c);
            }
        }
        let ([l0, l1, l2], [h0, h1, h2]) = (lo, hi);
        let (s0, s1, s2) = (h0 - l0, h1 - l1, h2 - l2);
        let (mut axis, widest) = if s0 < s1 { (1, s1) } else { (0, s0) };
        if widest < s2 {
            axis = 2;
        }
        let along = |f: &FaceRecord| {
            let [x, y, z] = f.centroid;
            match axis {
                0 => x,
                1 => y,
                _ => z,
            }
        };
        let min_x = |f: &FaceRecord| {
            let [x, ..] = f.bounds;
            f64::from(x)
        };
        faces.sort_by(|a, b| {
            order_f64(along(a), along(b))
                .then(order_f64(min_x(a), min_x(b)))
                .then(a.index.cmp(&b.index))
        });
        let halves = |faces: &[FaceRecord]| -> Result<([f32; 6], [f32; 6])> {
            let (l, r) = faces
                .split_at_checked(nl)
                .ok_or_else(|| bad(W, i, "node split past its faces"))?;
            Ok((union(l)?, union(r)?))
        };
        let low_x = |b: [f32; 6]| {
            let [x, ..] = b;
            x
        };
        let (mut lb, mut rb) = halves(&faces)?;
        if low_x(rb) < low_x(lb) {
            let k = faces.len().saturating_sub(nl);
            faces.rotate_left(k);
            (lb, rb) = halves(&faces)?;
            if low_x(rb) < low_x(lb) {
                faces.sort_by(|a, b| order_f64(min_x(a), min_x(b)).then(a.index.cmp(&b.index)));
                (lb, rb) = halves(&faces)?;
            }
        }
        let w = node(parent, lb, rb)?;
        *nodes
            .get_mut(i)
            .ok_or_else(|| bad(W, i, "a tree node past the nodes"))? = w;
        let (dl, dr) = decode_node(parent, w);
        let right = faces.split_off(nl);
        let child = i.saturating_mul(2);
        stack.push((child.saturating_add(2), right, dr));
        stack.push((child.saturating_add(1), faces, dl));
    }
    Ok((nodes, order))
}

/// `v`'s plane in `bins` steps of `lo..hi`, from `lo` (a min plane, rounded down) or from `hi` (a max plane): the
/// largest step whose plane does not cut into the box it bounds (FUN_14015f0f0).
fn quantise(lo: f32, hi: f32, v: f32, bins: u32, from_max: bool) -> u32 {
    let step = narrow(f64::from(hi) - f64::from(lo)) / bins as f32;
    if step <= 0.0 {
        return 0;
    }
    let d = if from_max {
        f64::from(hi) - f64::from(v)
    } else {
        f64::from(v) - f64::from(lo)
    };
    let mut q = bin(d / f64::from(step), bins);
    let plane = |k: u32| {
        let s = step * k as f32;
        if from_max { hi - s } else { lo + s }
    };
    let cuts = |x: f32| if from_max { x < v } else { v < x };
    let top = bins.saturating_sub(1);
    while q != 0 && cuts(plane(q)) {
        q = q.saturating_sub(1);
    }
    while q < top && !cuts(plane(q.saturating_add(1))) {
        q = q.saturating_add(1);
    }
    q
}

/// A node: the inner min planes (the larger of the two children's mins, 10 bits each of x, y, z) with which child
/// owns the low y and z (bits 30, 31; the left always owns the low x), and the inner max planes (10, 9 and 10 bits)
/// with which child owns each max (bits 61-63), all quantised in the parent's box (FUN_14015c4b0).
fn node(parent: [f32; 6], left: [f32; 6], right: [f32; 6]) -> Result<u64> {
    let [pl0, pl1, pl2, ph0, ph1, ph2] = parent;
    let [ll0, ll1, ll2, lh0, lh1, lh2] = left;
    let [rl0, rl1, rl2, rh0, rh1, rh2] = right;
    if ll0 > rl0 {
        return Err(bad(W, 0, "packed collision tree has an invalid low-X owner"));
    }
    let mut w = 0u64;
    // (shift, parent min, parent max, left min, right min)
    for (shift, lo, hi, l, r) in [
        (0, pl0, ph0, ll0, rl0),
        (10, pl1, ph1, ll1, rl1),
        (20, pl2, ph2, ll2, rl2),
    ] {
        let inner = if r <= l { l } else { r };
        w |= u64::from(quantise(lo, hi, inner, 1024, false)).wrapping_shl(shift);
    }
    if ll1 > rl1 {
        w |= 1 << 30;
    }
    if ll2 > rl2 {
        w |= 1 << 31;
    }
    // (shift, owner bit, bins, parent min, parent max, left max, right max)
    for (shift, owner, bins, lo, hi, l, r) in [
        (32, 61, 1024, pl0, ph0, lh0, rh0),
        (42, 62, 512, pl1, ph1, lh1, rh1),
        (51, 63, 1024, pl2, ph2, lh2, rh2),
    ] {
        let inner = if l <= r { l } else { r };
        w |= u64::from(quantise(lo, hi, inner, bins, true)).wrapping_shl(shift);
        if r > l {
            w |= 1u64.wrapping_shl(owner);
        }
    }
    Ok(w)
}

/// The two child boxes a node gives in its parent's box (FUN_14015b6b0), each min x, y, z, max x, y, z: the left
/// keeps the parent's planes and the right takes the inner ones, each owner bit swapping its side. The root's box is
/// its shape's bounds.
pub fn decode_node(parent: [f32; 6], w: u64) -> ([f32; 6], [f32; 6]) {
    let [pl0, pl1, pl2, ph0, ph1, ph2] = parent;
    // the right child's min and max on an axis: the inner planes in the parent's box
    let inner = |lo: f32, hi: f32, min_bits: u64, max_bits: u64, max_bins: f64| {
        let ext = f64::from(hi - lo);
        let smin = narrow(ext * 0.000_976_562_5);
        let smax = narrow(ext / max_bins);
        (min_bits as f32 * smin + lo, hi - max_bits as f32 * smax)
    };
    let (rl0, rh0) = inner(pl0, ph0, w & 0x3FF, (w >> 32) & 0x3FF, 1024.0);
    let (rl1, rh1) = inner(pl1, ph1, (w >> 10) & 0x3FF, (w >> 42) & 0x1FF, 512.0);
    let (rl2, rh2) = inner(pl2, ph2, (w >> 20) & 0x3FF, (w >> 51) & 0x3FF, 1024.0);
    // each owner bit swaps the two children's plane
    let owned = |bit: u32, l: f32, r: f32| {
        if w.wrapping_shr(bit) & 1 == 1 {
            (r, l)
        } else {
            (l, r)
        }
    };
    let (ll1, rl1) = owned(30, pl1, rl1);
    let (ll2, rl2) = owned(31, pl2, rl2);
    let (lh0, rh0) = owned(61, ph0, rh0);
    let (lh1, rh1) = owned(62, ph1, rh1);
    let (lh2, rh2) = owned(63, ph2, rh2);
    ([pl0, ll1, ll2, lh0, lh1, lh2], [rl0, rl1, rl2, rh0, rh1, rh2])
}
