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
    }
    eprintln!(
        "{} MeshSets round trip; (header, LOD record, bone bounds): {forms:?}; {tails} with trailing data",
        all.len()
    );
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
