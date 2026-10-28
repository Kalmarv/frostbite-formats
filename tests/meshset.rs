//! Tests of the MeshSet reader and writer (`frostbite_formats::meshset`).
//!
//! - `the_games_own_meshsets_round_trip` (`installs.skate/Data`, `tools.oodle9`): every MeshSet of every TOC (all
//!   three LOD record forms, both headers, the development mesh with data after its relocation table) reads with its
//!   resource metadata and writes back byte-identical, the metadata too.
//! - Synthetic, on the first rigid mesh of `Win32/buildkititems.toc`: an edit of the typed fields and names is what is
//!   written and read back; a relocation table in another order reads the same, one naming a field that is no
//!   pointer, and metadata that misplaces it, are refused.

#![allow(clippy::unwrap_used)]

mod inputs;

use frostbite_formats::MESHSET;
use frostbite_formats::folder::{Folder, Source};
use frostbite_formats::meshset::{self, LOD_MAP, LOD_MASKS, LOD_PLAIN};
use inputs::{Game, Tool, install_file, tool};
use primase_foundations::compress::oodle::{Oodle, Version};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn oodle() -> Option<Oodle> {
    let p = tool(Tool::Oodle9)?;
    // SAFETY: the configuration names the game's own Oodle DLL as tools.oodle9.
    #[allow(unsafe_code)]
    let o = unsafe { Oodle::load(p, Version::V9) };
    Some(o.expect("tools.oodle9 loads"))
}

/// A stored MeshSet: its name, bytes and resource metadata.
type Stored = (String, Vec<u8>, [u8; 16]);

/// Every MeshSet of the TOCs `only` names (all when empty), once per stored file.
fn meshsets(only: &[&str]) -> R<Option<Vec<Stored>>> {
    let (Some(data), Some(oodle)) = (install_file(Game::Skate, "Data"), oodle()) else {
        return Ok(None);
    };
    let folder = Folder::open(&data)?;
    let src = Source {
        folder: &folder,
        base: Some(&folder),
        oodle: Some(&oodle),
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for toc in folder
        .tocs
        .iter()
        .filter(|t| only.is_empty() || only.contains(&t.as_str()))
    {
        for b in &src.listing(toc)?.bundles {
            for (i, a) in b.manifest.res.iter().enumerate() {
                if a.rtype != MESHSET {
                    continue;
                }
                let e = b.resource(i).expect("resource index of the manifest");
                let (loc, at, _) = e.file;
                if seen.insert((loc.patch, loc.chunk, loc.archive, at)) {
                    out.push((e.name, src.payload(e.file, e.size as usize)?, a.rmeta));
                }
            }
        }
    }
    Ok(Some(out))
}

#[test]
fn the_games_own_meshsets_round_trip() -> R {
    let Some(all) = meshsets(&[])? else {
        return Ok(());
    };
    let mut forms = std::collections::BTreeMap::<(usize, usize, bool), usize>::new();
    let mut tails = 0;
    let mut flags = std::collections::BTreeMap::<u8, usize>::new();
    let mut inline_lods = 0;
    for (name, bytes, meta) in &all {
        let m = meshset::read_resource(bytes, meta).map_err(|e| format!("{name}: {e}"))?;
        let (again, again_meta) = meshset::write(&m).map_err(|e| format!("{name}: {e}"))?;
        if again != *bytes {
            let at = again
                .iter()
                .zip(bytes)
                .position(|(a, b)| a != b)
                .unwrap_or(again.len().min(bytes.len()));
            let w = |b: &[u8]| format!("{:02x?}", &b[at.saturating_sub(8)..(at + 24).min(b.len())]);
            panic!(
                "{name}: written bytes differ from {at:#x} ({} written, {} stored)
  written {}
  stored  {}",
                again.len(),
                bytes.len(),
                w(&again),
                w(bytes)
            );
        }
        assert_eq!(again_meta, *meta, "{name}: metadata");
        // the reader alone reads the same records
        let plain = meshset::read(bytes)?;
        assert_eq!(plain.lods, m.lods, "{name}");
        *forms
            .entry((m.header.len(), m.lod_size(), m.bone_bounds.is_some()))
            .or_default() += 1;
        tails += usize::from(!m.tail.is_empty());
        // `MeshSet::tail` is the geometry of the LOD with no chunk: its vertex and index bytes and four more
        let inline: Vec<_> = m.lods.iter().filter(|l| l.chunk == [0u8; 16]).collect();
        inline_lods += inline.len();
        match inline.as_slice() {
            [] => assert!(m.tail.is_empty(), "{name}: a tail and every LOD has a chunk"),
            [lod] => {
                let (vb, ib) = (lod.vertex_bytes as usize, lod.index_bytes as usize);
                assert_eq!(m.tail.len(), vb + ib + 4, "{name}: tail size");
                // the vertex buffer, then the index buffer, then four zero bytes: every index names a vertex
                let vertices = lod.sections.iter().map(|s| s.vertices).max().unwrap_or(0);
                let width = usize::from(lod.index_size);
                for index in m.tail[vb..vb + ib].chunks_exact(width) {
                    let mut le = [0u8; 4];
                    le[..width].copy_from_slice(index);
                    assert!(
                        u32::from_le_bytes(le) < vertices,
                        "{name}: an index past the vertices"
                    );
                }
                assert_eq!(m.tail[vb + ib..], [0; 4], "{name}: the last four bytes");
            }
            _ => panic!("{name}: {} LODs without a chunk", inline.len()),
        }
        for sec in m.lods.iter().flat_map(|l| &l.sections) {
            *flags.entry(sec.flags).or_default() += 1;
        }
    }
    eprintln!(
        "{} MeshSets round trip; (header, LOD record, bone bounds): {forms:?}; {tails} with trailing data, {inline_lods} LODs without a chunk; section flags: {flags:x?}",
        all.len()
    );
    assert_eq!(tails, inline_lods);
    // the documented range of `Section::flags`
    assert!(flags.keys().all(|&f| f < 0x20), "section flags: {flags:x?}");
    for lod in [LOD_PLAIN, LOD_MASKS, LOD_MAP] {
        assert!(forms.keys().any(|k| k.1 == lod), "LOD record {lod:#x} occurs");
    }
    assert!(tails > 0);
    Ok(())
}

#[test]
fn edits_are_written_and_bad_tables_refused() -> R {
    let Some(all) = meshsets(&["Win32/buildkititems.toc"])? else {
        return Ok(());
    };
    let (name, bytes, meta) = all
        .iter()
        .find(|(_, b, _)| meshset::read(b).is_ok_and(|m| m.lod_size() == LOD_PLAIN))
        .unwrap();
    let mut m = meshset::read_resource(bytes, meta)?;
    m.name = format!("{}_renamed", m.name);
    m.lods[0].name.push_str("_x");
    let s = &mut m.lods[0].sections[0];
    s.name = "Another_M".into();
    s.key ^= 0x5555;
    s.primitives -= 1;
    let (key, primitives) = (s.key, s.primitives);
    m.lods[0].lists[2] = vec![0, 0, 0];
    let (w, wm) = meshset::write(&m)?;
    let back = meshset::read_resource(&w, &wm)?;
    assert_eq!(back.name, m.name, "{name}");
    assert_eq!(back.lods[0].name, m.lods[0].name);
    let b = &back.lods[0].sections[0];
    assert_eq!(
        (b.name.as_str(), b.key, b.primitives),
        ("Another_M", key, primitives)
    );
    assert_eq!(back.lods[0].lists[2], [0, 0, 0]);
    // the relocation table in another order reads the same; a table naming a field that is no pointer, and metadata
    // naming another table, are refused
    let table = u32::from_le_bytes(meta[..4].try_into()?) as usize;
    let mut swapped = bytes.clone();
    swapped[table + 4..table + 8].copy_from_slice(&bytes[table..table + 4]);
    swapped[table..table + 4].copy_from_slice(&bytes[table + 4..table + 8]);
    assert_eq!(
        meshset::read_resource(&swapped, meta)?,
        meshset::read_resource(bytes, meta)?
    );
    let mut wrong = bytes.clone();
    let first = u32::from_le_bytes(bytes[table..table + 4].try_into()?);
    wrong[table..table + 4].copy_from_slice(&(first + 4).to_le_bytes());
    assert!(meshset::read_resource(&wrong, meta).is_err());
    let mut shifted = *meta;
    shifted[..4].copy_from_slice(&(table as u32 + 16).to_le_bytes());
    assert!(meshset::read_resource(bytes, &shifted).is_err());
    // a bone mask on a plain LOD record has nowhere to go
    let mut masked = meshset::read(bytes)?;
    masked.lods[0].bone_masks = Some(vec![[0; meshset::MASK]; masked.lods[0].sections.len()]);
    assert!(meshset::write(&masked).is_err());
    Ok(())
}

/// A one-section mesh from the rigid builder, written: its bytes and resource metadata.
fn built() -> (Vec<u8>, [u8; 16]) {
    let part = frostbite_formats::rigid::Part {
        material: "default_mat".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        triangles: vec![[0, 1, 2]],
        ..Default::default()
    };
    let m = frostbite_formats::rigid::build("world/test/a_mesh", [3; 16], &[part]).unwrap();
    meshset::write(&m.meshset).unwrap()
}

/// Counts, pointers and sizes that run past the data are refused with an error (naming where), not a panic.
#[test]
fn hostile_pointers_and_counts_are_refused() {
    let (bytes, meta) = built();
    assert!(meshset::read_resource(&bytes, &meta).is_ok());
    for n in 0..bytes.len() {
        let _ = meshset::read(&bytes[..n]);
    }
    // the header's LOD pointer, the LOD's section count and section pointer, a list count, the name pointer
    let lod = 0xd0;
    for at in [
        0x30,
        0x68,
        lod + 8,
        lod + 0x0c,
        lod + 0x14,
        lod + 0x18,
        lod + 0x8c,
    ] {
        let width = if matches!(at, 0xd8 | 0xe4) { 4 } else { 8 };
        for v in [u64::MAX, 0xFFFF_FFFF, 0x7FFF_FFFF_FFFF] {
            let mut c = bytes.clone();
            c[at..at + width].copy_from_slice(&v.to_le_bytes()[..width]);
            let e = meshset::read(&c).unwrap_err().to_string();
            assert!(e.starts_with("meshset at "), "{at:#x}: {e}");
        }
    }
    let sizes = frostbite_formats::meshset::read(&bytes).unwrap();
    let section = &sizes.lods[0].sections[0];
    // 3 vertices of 8 + 4 + 4 bytes in three streams
    assert_eq!(section.stream_offset(0), Some(0));
    assert_eq!(section.stream_offset(1), Some(24));
    assert_eq!(section.stream_offset(3), Some(48));
    assert_eq!(section.stream_offset(4), None);
    let mut far = section.clone();
    far.vertex_offset = u32::MAX;
    far.vertices = u32::MAX;
    assert_eq!(far.stream_offset(0), Some(u32::MAX as usize));
}

/// The enumerations and flag words of the games own MeshSets: the kind byte, the layout flag words, the primitive type.
/// Prints the census of every bit and asserts the relations that name them.
#[test]
fn the_games_own_meshsets_name_their_enumerations() -> R {
    use frostbite_formats::meshset::{MeshType, PrimitiveType, usage};
    let Some(all) = meshsets(&[])? else {
        return Ok(());
    };
    let mut by_type = std::collections::BTreeMap::<u8, (u64, u64, u64, u64)>::new();
    let mut set_bits = [0u64; 32];
    let mut lod_bits = [0u64; 32];
    let mut base_vs_first = [[0u64; 2]; 2];
    let mut streaming_vs_chunk = [[0u64; 2]; 2];
    let mut primitives = std::collections::BTreeMap::<u8, u64>::new();
    let (mut meshes, mut lods) = (0u64, 0u64);
    let mut header_streaming = [[0u64; 2]; 2];
    let mut instancing = [[0u64; 2]; 2];
    let mut multi = std::collections::BTreeMap::<(bool, bool, bool), u64>::new();
    let mut record_by_kind = std::collections::BTreeMap::<(u8, usize), u64>::new();
    for (name, bytes, meta) in &all {
        let m = meshset::read_resource(bytes, meta)?;
        meshes += 1;
        let ty = m.header[meshset::HEADER_TYPE];
        let e = by_type.entry(ty).or_default();
        e.0 += 1;
        e.1 += u64::from(m.header.len() == meshset::HEADER_BONES);
        let skinned_decl = m
            .lods
            .iter()
            .flat_map(|l| &l.sections)
            .any(|s| s.decl.element(usage::BLEND_INDICES).is_some());
        e.2 += u64::from(skinned_decl);
        e.3 += u64::from(
            m.lods
                .iter()
                .flat_map(|l| &l.sections)
                .all(|s| s.decl.element(usage::BLEND_INDICES).is_some()),
        );
        assert!(m.mesh_type().is_some(), "{name}: kind {ty}");
        *record_by_kind.entry((ty, m.lod_size())).or_default() += 1;
        header_streaming[usize::from(m.layout_flags() & 1 != 0)]
            [usize::from(m.lods.iter().any(|l| l.chunk == [0; 16]))] += 1;
        instancing[usize::from(m.layout_flags() >> 2 & 1 != 0)]
            [usize::from(m.lods.iter().any(|l| l.flags() >> 4 & 1 != 0))] += 1;
        for lod in &m.lods {
            let streams = |s: &meshset::Section| s.decl.strides.len() >= 2;
            *multi
                .entry((
                    lod.flags() >> 9 & 1 != 0,
                    lod.sections.iter().all(streams),
                    lod.sections.iter().any(streams),
                ))
                .or_default() += 1;
        }
        for (b, n) in set_bits.iter_mut().enumerate() {
            *n += u64::from(m.layout_flags() >> b & 1);
        }
        for (i, lod) in m.lods.iter().enumerate() {
            lods += 1;
            for (b, n) in lod_bits.iter_mut().enumerate() {
                *n += u64::from(lod.flags() >> b & 1);
            }
            base_vs_first[usize::from(lod.flags() & 1 != 0)][usize::from(i == 0)] += 1;
            streaming_vs_chunk[usize::from(lod.flags() >> 6 & 1 != 0)][usize::from(lod.chunk != [0; 16])] +=
                1;
            for s in &lod.sections {
                *primitives
                    .entry(s.record[meshset::SECTION_PRIMITIVE])
                    .or_default() += 1;
            }
        }
    }
    println!("{meshes} MeshSets, {lods} LODs");
    println!(
        "kind byte -> (meshes, with bone-bounds header, with blend indices in some section, in every section): {by_type:?}"
    );
    println!(
        "header flag word bit counts: {:?}",
        set_bits
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .collect::<Vec<_>>()
    );
    println!(
        "LOD flag word bit counts: {:?}",
        lod_bits
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .collect::<Vec<_>>()
    );
    println!("bit 0 set x is LOD 0 [[no,no],[no,yes]..]: {base_vs_first:?}");
    println!("bit 6 set x has a geometry chunk: {streaming_vs_chunk:?}");
    println!("section primitive byte: {primitives:?}");
    println!("header bit 0 clear x has an inline LOD: {header_streaming:?}");
    println!("header bit 2 x some LOD has bit 4: {instancing:?}");
    println!("LOD bit 9 x every section has two or more streams / some section does: {multi:?}");
    println!("kind x LOD record size: {record_by_kind:?}");
    // the kind names the LOD record form: plain (0xb0), bone map (0xbc), bone masks (0xb8)
    assert_eq!(
        record_by_kind.keys().copied().collect::<Vec<_>>(),
        [
            (MeshType::Rigid as u8, LOD_PLAIN),
            (MeshType::Skinned as u8, LOD_MAP),
            (MeshType::Composite as u8, LOD_MASKS)
        ]
    );
    // the flag bits that are named: set and clear exactly with what they say
    assert_eq!(
        header_streaming,
        [[0, 1], [47_757, 0]],
        "header bit 0 is clear for the one mesh with an inline LOD"
    );
    assert_eq!(
        instancing,
        [[47_437, 0], [0, 321]],
        "header bit 2 is set exactly when some LOD sets its bit 4"
    );
    assert_eq!(
        streaming_vs_chunk,
        [[1, 0], [0, 126_013]],
        "LOD bit 6 is set exactly when the LOD has a geometry chunk"
    );
    for (ty, (n, bones, _, _)) in &by_type {
        assert_eq!(
            *bones,
            if *ty == MeshType::Rigid as u8 { 0 } else { *n },
            "kind {ty}"
        );
    }
    assert_eq!(
        primitives.keys().copied().collect::<Vec<_>>(),
        [PrimitiveType::TriangleList as u8]
    );
    Ok(())
}
