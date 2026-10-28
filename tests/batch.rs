//! Synthetic tests of the chunk metadata rows, the `layout.toc` reader and the Patch TOC merge, built here.
//! The same readings are checked on the game's files by tests/real_data_batch.rs.

#![allow(clippy::unwrap_used)]

use frostbite_formats::bundle::{self, BundleMetric, ChunkMeta, ChunkMetaBody};
use frostbite_formats::db::{self, Node};
use frostbite_formats::layout;
use frostbite_formats::toc::{self, Loc, TocChunk, flags};

type R = Result<(), Box<dyn std::error::Error>>;

fn rows() -> Vec<ChunkMeta> {
    vec![
        ChunkMeta {
            h64: 0x0123_4567_89AB_CDEF,
            meta: ChunkMetaBody::Empty,
        },
        ChunkMeta {
            h64: 7,
            meta: ChunkMetaBody::FirstMip(-1),
        },
        ChunkMeta {
            h64: u64::MAX,
            meta: ChunkMetaBody::Container {
                container: "items/x.1234_bundle".to_string(),
                metrics: vec![
                    BundleMetric {
                        resource_type: 0xF7A9_3925,
                        name: "persistentTextureSize".to_string(),
                        value: 0x4000,
                    },
                    BundleMetric {
                        resource_type: 1,
                        name: "bundledMeshSizes".to_string(),
                        value: u32::MAX,
                    },
                ],
            },
        },
    ]
}

fn rewritten(bytes: &[u8], change: impl FnOnce(&mut Node)) -> Vec<u8> {
    let (mut root, _) = db::read_node(bytes, 0).unwrap();
    change(&mut root);
    let mut out = Vec::new();
    db::write_node(&root, &mut out).unwrap();
    out
}

#[test]
fn chunk_meta_rows_write_and_read_back() -> R {
    let rows = rows();
    let bytes = bundle::write_chunk_meta(&rows)?;
    assert_eq!(bundle::read_chunk_meta(&bytes)?, rows);
    // the list is named chunkMeta and holds one object a row
    let (root, end) = db::read_node(&bytes, 0)?;
    assert_eq!(
        (root.name.as_str(), root.ty, root.children.len(), end),
        ("chunkMeta", 1, 3, bytes.len())
    );
    let first = root.children.first().unwrap().children.first().unwrap();
    assert_eq!(
        (first.name.as_str(), first.payload.clone()),
        ("h64", 0x0123_4567_89AB_CDEFu64.to_le_bytes().to_vec())
    );
    // no rows: the empty list (what Studio stores), read back as no rows like the empty metadata of the stock bundles
    // without chunks
    let empty = bundle::write_chunk_meta(&[])?;
    assert!(!empty.is_empty());
    assert!(bundle::read_chunk_meta(&empty)?.is_empty());
    assert!(bundle::read_chunk_meta(&[])?.is_empty());
    Ok(())
}

#[test]
fn chunk_meta_of_an_unknown_shape_is_refused() -> R {
    let bytes = bundle::write_chunk_meta(&rows())?;
    // a meta object with a child no shape names
    let changed = rewritten(&bytes, |root| {
        root.children[1].children[1].children[0].name = "lastMip".to_string();
    });
    let e = bundle::read_chunk_meta(&changed).unwrap_err().to_string();
    assert!(e.contains("lastMip"), "{e}");
    // a row without h64
    let changed = rewritten(&bytes, |root| {
        root.children[0].children.remove(0);
    });
    assert!(bundle::read_chunk_meta(&changed).is_err());
    // not a chunkMeta list, or something after it
    let changed = rewritten(&bytes, |root| root.name = "other".to_string());
    assert!(bundle::read_chunk_meta(&changed).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(bundle::read_chunk_meta(&trailing).is_err());
    Ok(())
}

fn leaf(name: &str, ty: u8, payload: &[u8]) -> Node {
    Node {
        ty,
        named: true,
        name: name.to_string(),
        payload: payload.to_vec(),
        ..Node::default()
    }
}

fn list(name: &str, children: Vec<Node>) -> Node {
    Node {
        ty: 1,
        named: true,
        name: name.to_string(),
        children,
        terminated: true,
        ..Node::default()
    }
}

fn object(name: Option<&str>, children: Vec<Node>) -> Node {
    Node {
        ty: 2,
        named: name.is_some(),
        name: name.unwrap_or_default().to_string(),
        children,
        terminated: true,
        ..Node::default()
    }
}

fn item(ty: u8, payload: &[u8]) -> Node {
    Node {
        ty,
        payload: payload.to_vec(),
        ..Node::default()
    }
}

fn layout_bytes(chunk_name: Node) -> Vec<u8> {
    let root = object(
        Some(""),
        vec![
            list(
                "superBundles",
                vec![
                    object(None, vec![leaf("name", 7, b"Win32/globals\0")]),
                    object(None, vec![leaf("name", 7, b"Win32/levels/x/x\0")]),
                ],
            ),
            list("fs", vec![item(7, b"initfs_Win32\0")]),
            leaf("head", 8, &625_200u32.to_le_bytes()),
            object(
                Some("installManifest"),
                vec![
                    list(
                        "installChunks",
                        vec![object(
                            None,
                            vec![
                                leaf("id", 15, &[3; 16]),
                                chunk_name,
                                leaf("persistentIndex", 8, &9u32.to_le_bytes()),
                                list("superbundles", vec![item(7, b"Win32/globals\0")]),
                                list("requiredChunks", vec![item(15, &[4; 16])]),
                                leaf("alwaysInstalled", 6, &[1]),
                            ],
                        )],
                    ),
                    object(
                        Some("settings"),
                        vec![leaf("bundleFormat", 9, &77u64.to_le_bytes())],
                    ),
                ],
            ),
        ],
    );
    let mut envelope = toc::MAGIC.to_vec();
    envelope.resize(toc::ENVELOPE, 0);
    db::write_file(&envelope, &root, &[]).unwrap()
}

#[test]
fn a_layout_reads_its_typed_keys_and_writes_back() -> R {
    let bytes = layout_bytes(leaf("name", 7, b"configurations/layout/a\0"));
    let l = layout::read(&bytes)?;
    assert_eq!(l.super_bundles, ["Win32/globals", "Win32/levels/x/x"]);
    assert_eq!(l.fs, ["initfs_Win32"]);
    assert_eq!(l.head, Some(625_200));
    assert_eq!(l.bundle_format, Some(77));
    let c = &l.install_chunks[0];
    assert_eq!(c.id, Some([3; 16]));
    assert_eq!(c.persistent_index, Some(9));
    assert_eq!(c.always_installed, Some(true));
    assert_eq!(c.superbundles, ["Win32/globals"]);
    assert_eq!(c.required_chunks, [[4; 16]]);
    assert_eq!(c.directory(), "configurations/layout/a");
    assert_eq!(l.write()?, bytes);
    // a chunk with only installBundle takes its directory from it
    let l = layout::read(&layout_bytes(leaf(
        "installBundle",
        7,
        b"Win32/configurations/layout/b\0",
    )))?;
    assert_eq!(l.install_chunks[0].directory(), "configurations/layout/b");
    // a typed key of another type is refused, naming the key
    let e = layout::read(&layout_bytes(leaf("name", 8, &[0; 4])))
        .unwrap_err()
        .to_string();
    assert!(e.contains("name"), "{e}");
    Ok(())
}

fn chunk(guid: u8, at: u32) -> TocChunk {
    TocChunk {
        guid: [guid; 16],
        file: Some((
            Loc {
                patch: true,
                chunk: 1,
                archive: 1,
            },
            at,
            16,
        )),
    }
}

fn region(at: u32) -> Vec<u8> {
    let archive = Loc {
        patch: true,
        chunk: 1,
        archive: 1,
    };
    bundle::write_region(&[(archive, at, 16)], &[]).unwrap()
}

#[test]
fn a_patch_toc_merges_with_its_base_by_its_flags() -> R {
    let base = toc::read(&toc::write(
        &[
            ("win32/a".to_string(), region(0)),
            ("win32/b".to_string(), region(16)),
            ("win32/c".to_string(), region(32)),
        ],
        &[chunk(1, 100), chunk(2, 200), chunk(3, 300)],
        0,
    )?)?;
    let both = flags::HAS_BASE_BUNDLES | flags::HAS_BASE_CHUNKS;
    let removed = TocChunk {
        guid: [3; 16],
        file: None,
    };
    let patch = toc::read(&toc::write(
        &[
            ("Win32/B".to_string(), region(48)),
            ("win32/d".to_string(), region(64)),
        ],
        &[chunk(2, 222), chunk(4, 444), removed.clone()],
        both,
    )?)?;
    assert!(patch.has_base_bundles() && patch.has_base_chunks());
    let merged = toc::merge(&base, &patch)?;
    toc::verify(&merged)?;
    // the base's bundles the patch does not name and the patch's (the name matched ignoring case), in the slots of the
    // perfect hash
    let mut names: Vec<&str> = merged.bundles.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Win32/B", "win32/a", "win32/c", "win32/d"]);
    let region_of = |n: &str| {
        merged
            .bundles
            .iter()
            .find(|(m, _)| m == n)
            .map(|(_, r)| r.clone())
    };
    assert_eq!(region_of("Win32/B"), Some(region(48)));
    assert_eq!(region_of("win32/a"), Some(region(0)));
    // the base's chunks the patch does not name and the patch's; a removed one drops the base's and is not listed
    let mut chunks: Vec<([u8; 16], Option<u32>)> = merged
        .chunks
        .iter()
        .map(|c| (c.guid, c.file.map(|(_, at, _)| at)))
        .collect();
    chunks.sort();
    assert_eq!(
        chunks,
        [([1; 16], Some(100)), ([2; 16], Some(222)), ([4; 16], Some(444))]
    );
    assert_eq!(merged.flags, 0, "the merged TOC stands alone");
    // it writes and reads back as it is
    let again = toc::read(&toc::write(&merged.bundles, &merged.chunks, merged.flags)?)?;
    assert_eq!(
        (again.bundles, again.chunks),
        (merged.bundles.clone(), merged.chunks.clone())
    );

    // only the bundles asked for: the chunks are the patch's, the removed one kept as removed
    let bundles_only = toc::read(&toc::write(
        &[("win32/d".to_string(), region(64))],
        &[chunk(4, 444), removed.clone()],
        flags::HAS_BASE_BUNDLES,
    )?)?;
    let merged = toc::merge(&base, &bundles_only)?;
    assert_eq!(merged.bundles.len(), 4);
    assert_eq!(merged.chunks.len(), 2);
    assert!(merged.chunks.contains(&removed) && merged.chunks.contains(&chunk(4, 444)));
    // no flag: the patch alone
    let alone = toc::read(&toc::write(
        &[("win32/d".to_string(), region(64))],
        &[chunk(4, 444)],
        0,
    )?)?;
    let merged = toc::merge(&base, &alone)?;
    assert_eq!((merged.bundles, merged.chunks), (alone.bundles, alone.chunks));
    // against nothing, a flagged TOC is its own bundles and chunks (a superbundle the folder adds)
    let merged = toc::merge(&toc::Toc::default(), &patch)?;
    assert_eq!(merged.bundles.len(), patch.bundles.len());
    for b in &patch.bundles {
        assert!(merged.bundles.contains(b));
    }
    assert_eq!(merged.chunks.len(), 2);
    Ok(())
}
