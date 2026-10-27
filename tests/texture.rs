//! Texture resource headers: hand-built headers read and written back, the refusals, the level sizes of the formats
//! ReSkate decodes; and, on the game's data (`installs.skate/Data`, `tools.oodle9`), every texture resource of
//! `Win32/items.toc` read and written back byte for byte, its chunk size the sum of its mip sizes, and for a 2D
//! texture or array (kinds 0 and 3) of a format the table knows its mip sizes the chain of one slice; every resource id
//! is odd and byte 0x21 is the level count; the name hash is the djb2-xor of the lowercase name, the pool id is 0, the
//! flags are one of the four stock values; among the textures whose first mip is 0, `mip_block_offsets` and byte 0x20 are
//! zero in most (counted, printed and asserted to be most of them). Other kinds (a
//! volume's levels count its depth) are counted, not checked.

#![allow(clippy::unwrap_used)]

mod inputs;

use frostbite_formats::bundle;
use frostbite_formats::folder::{Folder, Source};
use frostbite_formats::texture::{self, Storage, TextureHeader, flags};
use inputs::{Game, Tool, install_file, tool};
use primase_foundations::compress::oodle::{Oodle, Version};
use std::collections::BTreeMap;

type R = Result<(), Box<dyn std::error::Error>>;

/// A 64x32 BC7 header of a 2-slice array, 7 levels, its unread bytes distinct so a misplaced field shows.
fn header() -> TextureHeader {
    let mut mip_sizes = [0u32; 15];
    let chain = texture::chain(Storage::Block(16), 64, 32, 7).expect("a 64x32 chain");
    mip_sizes[..7].copy_from_slice(&chain);
    TextureHeader {
        mip_block_offsets: [0x0403_0201, 0x0807_0605],
        kind: 3,
        format: 67,
        pool_id: 0x0a0b_0c0d,
        flags: 1,
        width: 64,
        height: 32,
        depth: 2,
        slices: 2,
        mip_count: 7,
        first_mip: 1,
        unknown_20: [1, 7, 0, 0, 2, 1, 0, 0],
        chunk: std::array::from_fn(|i| 0x40 + i as u8),
        mip_sizes,
        chunk_size: chain.iter().sum(),
        name_hash: 0x7061_7365,
        unknown_7c: 0x1122_3344,
        unknown_80: std::array::from_fn(|i| 0x80 + i as u8),
    }
}

#[test]
fn a_header_reads_back_as_written() -> R {
    let h = header();
    let bytes = texture::write(&h)?;
    assert_eq!(bytes.len(), texture::HEADER_LEN);
    assert_eq!(&bytes[0x00..0x08], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(&bytes[0x10..0x14], &0x0a0b_0c0du32.to_le_bytes());
    assert_eq!(&bytes[0x78..0x7c], &0x7061_7365u32.to_le_bytes());
    assert_eq!(&bytes[0x7c..0x80], &0x1122_3344u32.to_le_bytes());
    assert_eq!(bytes[0x80], 0x80);
    // fields at their offsets
    assert_eq!(&bytes[0x08..0x10], &[3, 0, 0, 0, 67, 0, 0, 0]);
    assert_eq!(&bytes[0x16..0x18], &64u16.to_le_bytes());
    assert_eq!((bytes[0x1e], bytes[0x1f]), (7, 1));
    assert_eq!(bytes[0x28], 0x40);
    assert_eq!(&bytes[0x38..0x3c], &(64 * 32u32).to_le_bytes());
    assert_eq!(&bytes[0x74..0x78], &h.chunk_size.to_le_bytes());
    assert_eq!(texture::read(&bytes)?, h);
    Ok(())
}

#[test]
fn bad_headers_are_refused() -> R {
    let good = texture::write(&header())?;
    let err = |b: &[u8]| texture::read(b).unwrap_err().to_string();
    assert!(err(&good[..144]).contains("144 bytes, not 180"));
    let mut b = good.clone();
    b[0x16..0x18].copy_from_slice(&0u16.to_le_bytes());
    assert!(err(&b).contains("dimensions 0x32"));
    let mut b = good.clone();
    b[0x18..0x1a].copy_from_slice(&16385u16.to_le_bytes());
    assert!(err(&b).contains("dimensions 64x16385"));
    for m in [0u8, 16] {
        let mut b = good.clone();
        b[0x1e] = m;
        assert!(err(&b).contains(&format!("mip count {m}")));
    }
    let mut h = header();
    h.mip_count = 0;
    assert!(texture::write(&h).is_err());
    Ok(())
}

#[test]
fn level_sizes_follow_the_format() {
    assert_eq!(texture::block(20), Some(Storage::Texel(4)));
    assert_eq!(texture::block(55), Some(Storage::Block(8)));
    assert_eq!(texture::block(66), Some(Storage::Block(16)));
    assert_eq!(texture::block(0), None);
    // partial blocks are whole blocks; a level is at least one block
    assert_eq!(texture::level_size(Storage::Block(16), 5, 3), Some(32));
    assert_eq!(texture::level_size(Storage::Block(8), 1, 1), Some(8));
    assert_eq!(texture::level_size(Storage::Texel(4), 3, 2), Some(24));
    // a non-square chain: each side halved, at least 1
    assert_eq!(
        texture::chain(Storage::Texel(4), 8, 2, 4),
        Some(vec![64, 16, 8, 4])
    );
    assert_eq!(texture::level_size(Storage::Texel(4), u32::MAX, 2), None);
}

#[test]
fn every_texture_of_the_items_toc_reads_and_writes_back() -> R {
    let (Some(data), Some(dll)) = (install_file(Game::Skate, "Data"), tool(Tool::Oodle9)) else {
        return Ok(());
    };
    let folder = Folder::open(data)?;
    // SAFETY: the configuration names the game's own Oodle DLL as tools.oodle9.
    #[allow(unsafe_code)]
    let oodle = unsafe { Oodle::load(dll, Version::V9) }?;
    let src = Source {
        folder: &folder,
        base: Some(&folder),
        oodle: Some(&oodle),
    };
    let toc = folder.toc("Win32/items.toc")?;
    let (mut read, mut formats, mut kinds) = (
        0usize,
        BTreeMap::<u32, usize>::new(),
        BTreeMap::<u32, usize>::new(),
    );
    let mut seen = std::collections::BTreeSet::new();
    // among the textures whose chunk holds every level (first mip 0): how many have both mip block offsets zero, byte 0x20 zero
    let (mut first0, mut zero_00, mut zero_20) = (0usize, 0usize, 0usize);
    for (_, region) in &toc.bundles {
        let (files, inline) = bundle::read_region(region)?;
        let Some((m, first, _)) = src.manifest(&files, &inline)? else {
            continue;
        };
        for (i, a) in m.res.iter().enumerate() {
            if a.rtype != frostbite_formats::TEXTURE || !seen.insert(a.rid) {
                continue;
            }
            let bytes = src.payload(files[first + m.ebx.len() + i], a.size as usize)?;
            let h = texture::read(&bytes).map_err(|e| format!("{}: {e}", a.name))?;
            assert_eq!(texture::write(&h)?, bytes, "{}", a.name);
            let levels = usize::from(h.mip_count);
            assert!(h.mip_sizes[levels..].iter().all(|&s| s == 0), "{}", a.name);
            assert_eq!(h.chunk_size, h.mip_sizes.iter().sum::<u32>(), "{}", a.name);
            // every texture's id is odd (tagged); byte 0x21 is the level count in every texture
            assert_eq!(a.rid & 1, 1, "{}", a.name);
            assert_eq!(h.unknown_20[1], h.mip_count, "{}", a.name);
            // the named fields: the name hash is the djb2-xor of the lowercase resource name, the pool id is unused, the
            // flags use only the bits stock textures set, and the kind is one of the four
            assert_eq!(
                h.name_hash,
                frostbite_formats::djb2(&a.name.to_ascii_lowercase()),
                "{}",
                a.name
            );
            assert_eq!(h.pool_id, 0, "{}", a.name);
            assert!(
                [
                    0,
                    flags::STREAMING,
                    flags::STREAMING | flags::ON_DEMAND_LOADED,
                    flags::NO_SKIPMIP
                ]
                .contains(&h.flags),
                "{}: flags {:#x}",
                a.name,
                h.flags
            );
            assert!(h.kind <= 3, "{}: kind {}", a.name, h.kind);
            assert!(h.unknown_20[0] >= h.first_mip, "{}", a.name);
            if h.first_mip == 0 {
                first0 += 1;
                zero_00 += usize::from(h.mip_block_offsets == [0; 2]);
                zero_20 += usize::from(h.unknown_20[0] == 0);
            }
            *kinds.entry(h.kind).or_default() += 1;
            if let (0 | 3, Some(s)) = (h.kind, texture::block(h.format)) {
                let chain = texture::chain(s, u32::from(h.width), u32::from(h.height), h.mip_count).unwrap();
                assert_eq!(&h.mip_sizes[..levels], &chain[..], "{}", a.name);
            }
            *formats.entry(h.format).or_default() += 1;
            read += 1;
        }
    }
    println!("{read} texture headers; by format {formats:?}; by kind {kinds:?}");
    println!("first mip 0: {first0}; mip block offsets zero in {zero_00}, byte 0x20 zero in {zero_20}");
    // what a writer should store for a texture whose chunk holds every level (a stated choice, not a rule): zero, the
    // most common value of both among such textures
    assert!(
        2 * zero_00 > first0 && 2 * zero_20 > first0,
        "{zero_00} {zero_20} of {first0}"
    );
    assert!(read > 1000, "{read}");
    Ok(())
}
