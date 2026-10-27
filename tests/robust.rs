//! Hostile structure in the container readers is a named refusal, never an abort or a mis-read: regression inputs of the
//! robustness census (nested native-DB containers, a Latin-1 name that does not round-trip, a LOD count past the header).

#![allow(clippy::unwrap_used)]

use frostbite_formats::db::{self, MAX_DEPTH, Node};
use frostbite_formats::meshset;
use frostbite_formats::{binding, physics, toc};

/// `levels` unnamed objects, one inside the next, the innermost empty: a node tag (2, unnamed) and the varint size of
/// what it holds.
fn nested(levels: usize) -> Vec<u8> {
    let mut inner: Vec<u8> = Vec::new();
    for _ in 0..levels {
        let mut n = inner.len();
        let mut size = Vec::new();
        loop {
            let b = (n & 127) as u8;
            n >>= 7;
            if n == 0 {
                size.push(b);
                break;
            }
            size.push(b | 128);
        }
        let mut node = vec![0x82];
        node.extend(size);
        node.extend(&inner);
        inner = node;
    }
    inner
}

/// 100,000 nested lists (about 300 KB) overflowed an 8 MiB stack and aborted the process; 10,000 read. The bound is
/// [`MAX_DEPTH`] levels, refused by name.
#[test]
fn containers_nested_past_the_bound_are_refused() {
    assert!(db::read_node(&nested(MAX_DEPTH), 0).is_ok());
    let e = db::read_node(&nested(MAX_DEPTH + 1), 0).unwrap_err().to_string();
    assert!(e.contains("nest deeper than"), "{e}");
    let e = db::read_node(&nested(100_000), 0).unwrap_err().to_string();
    assert!(e.contains("nest deeper than"), "{e}");
}

/// A name with a byte over 0x7f (Latin-1: one byte a character) advanced the reader by its UTF-8 length, so the
/// writer's own output failed to read back ("container length mismatch").
#[test]
fn a_latin1_name_round_trips() {
    let leaf = |name: &str, value: u32| Node {
        ty: 8,
        named: true,
        name: name.into(),
        payload: value.to_le_bytes().to_vec(),
        ..Node::default()
    };
    let root = Node {
        ty: 2,
        terminated: true,
        children: vec![leaf("café", 3), leaf("naïve ünïcode ÿ", 4), leaf("b", 5)],
        ..Node::default()
    };
    let mut bytes = Vec::new();
    db::write_node(&root, &mut bytes).unwrap();
    let (back, end) = db::read_node(&bytes, 0).unwrap();
    assert_eq!(back, root);
    assert_eq!(end, bytes.len());
    assert_eq!(back.field("café").and_then(Node::u32), Some(3));
}

/// The LOD record pointers lie between 0x30 and the name at 0x68: a count of more would read "pointers" out of the
/// header's other fields. A header of an unknown size is refused before anything else.
#[test]
fn a_lod_count_past_the_header_is_refused() {
    let mut d = vec![0u8; 0x200];
    for (at, v) in [(0, 0xc0u32), (4, 0xb0), (8, 0x180)] {
        d[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    // the name, at 0x100 (pointers are stored relative to byte 16)
    for at in [0x68, 0x70] {
        d[at..at + 8].copy_from_slice(&0xf0u64.to_le_bytes());
    }
    d[0x100] = b'x';
    d[0xb4..0xb6].copy_from_slice(&((meshset::MAX_LODS + 1) as u16).to_le_bytes());
    let e = meshset::read(&d).unwrap_err().to_string();
    assert!(e.contains("8 LODs"), "{e}");
    d[0xb4..0xb6].copy_from_slice(&0u16.to_le_bytes());
    let m = meshset::read(&d).unwrap();
    assert!(m.lods.is_empty() && m.name == "x");
    d[0] = 0xb0;
    let e = meshset::read(&d).unwrap_err().to_string();
    assert!(e.contains("header of 0xc0 bytes"), "{e}");
}

/// A hash-map entry of `i32::MIN` made `-v - 1` overflow (a panic in debug builds); it names a slot no table has.
#[test]
fn a_toc_hash_entry_of_i32_min_finds_nothing() {
    let key = b"levels/test".to_vec();
    assert_eq!(toc::lookup(&[i32::MIN], std::slice::from_ref(&key), &key), None);
    assert_eq!(toc::lookup(&[-1], std::slice::from_ref(&key), &key), Some(0));
}

/// A resource whose face offsets step to `0xFFFF` made `w[0] + 3` overflow a `u16`; the offsets are refused by name.
#[test]
fn a_physics_face_offset_of_0xffff_is_refused() {
    let (positions, triangles) = (
        vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        vec![[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]],
    );
    let (shape, centre) = physics::mesh_shape(&positions, &triangles).unwrap().unwrap();
    let res = physics::PhysicsResource {
        version: 2,
        word08: 0,
        body: Vec::new(),
        shapes: vec![shape],
        parts: vec![physics::Part::mesh(0, centre, [1, 2, 3], 96)],
    };
    let mut b = physics::write(&res).unwrap();
    assert_eq!(physics::read(&b).unwrap(), res);
    // the first shape starts at 0x38 (no body); its face-offset slot is at +0x24 + 4 * 7, relative to the slot
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    let slot = 0x38 + 0x24 + 4 * 7;
    let faces = slot + u32_at(&b, slot);
    b[faces + 2..faces + 4].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let e = physics::read(&b).unwrap_err().to_string();
    assert!(e.contains("face offsets"), "{e}");
}

/// An entry near `u32::MAX` made the buffer's end wrap (a panic in debug builds); the end is reported whole.
#[test]
fn a_binding_buffer_end_past_u32_is_reported_whole() {
    let group = binding::Group {
        name: 1,
        size: 16,
        flags: [0; 4],
        words: [0; 4],
        params: Vec::new(),
        entries: vec![
            binding::Entry {
                at: u32::MAX - 1,
                marker: 0xFFFF,
                kind: 0x30,
            },
            binding::Entry {
                at: u32::MAX,
                marker: 0xFFFF,
                kind: 0x30,
            },
        ],
    };
    assert_eq!(group.buffer_end(), u64::from(u32::MAX - 1) + 12);
}

/// A TOC's error offsets are into the file, envelope included: the bytes after the envelope start at 0x22C.
#[test]
fn a_toc_error_offset_counts_the_envelope() {
    let region = frostbite_formats::bundle::write_region(&[], &[]).unwrap();
    let mut b = toc::write(&[("a/b".to_string(), region)], &[], 0).unwrap();
    // the bundle count is the third word after the envelope
    b[toc::ENVELOPE + 8..toc::ENVELOPE + 12].copy_from_slice(&(-1i32).to_be_bytes());
    match toc::read(&b).unwrap_err() {
        frostbite_formats::Error::Format { offset, detail, .. } => {
            assert_eq!(offset, toc::ENVELOPE + 8);
            assert!(
                detail.contains("negative") && detail.contains("expected"),
                "{detail}"
            );
        }
        other => panic!("{other}"),
    }
    let e = toc::read(&b[..100]).unwrap_err().to_string();
    assert!(
        e.contains("toc at 0x0: envelope is invalid: expected the magic"),
        "{e}"
    );
}

/// Raw CAS blocks that claim 16 MiB each and hold nothing: the decoded size the headers add up to is refused before a
/// buffer of it is reserved (200 headers of 8 bytes would otherwise reserve and zero 3 GiB).
#[test]
fn raw_cas_blocks_claiming_more_than_they_hold_are_refused_before_allocation() {
    use frostbite_formats::cas::Cas;
    use primase_foundations::compress::Decompressor;
    let block = [0u8, 0xFF, 0xFF, 0xFF, 0, 0x70, 0, 0];
    let stream: Vec<u8> = block.iter().copied().cycle().take(8 * 200).collect();
    let claimed = 200 * 0x00FF_FFFF;
    let e = Cas { oodle: None }.decompress(&stream, claimed).unwrap_err();
    assert!(e.to_string().contains("size"), "{e}");
}

/// A depot whose entries each have a descriptor of their own, all pointing at the same 4,000 bytes, made the reader
/// copy the bytes once per entry (4 MB from a 44 KB payload; a larger payload squares). The blocks together may not hold
/// more bytes than the payload.
#[test]
fn depot_entries_cannot_multiply_one_block() {
    use frostbite_formats::depot;
    let (entries, data_len) = (1000usize, 4000usize);
    let data_at = 0x10 + 16 * entries;
    let desc_at = (data_at + data_len).next_multiple_of(16);
    let mut d = Vec::new();
    d.extend(0u64.to_le_bytes());
    d.extend(0u64.to_le_bytes());
    for i in 0..entries {
        d.extend((i as u64).to_le_bytes());
        d.extend(((desc_at + 24 * i) as u64).to_le_bytes());
    }
    d.resize(desc_at, 0);
    for i in 0..entries {
        d.extend((data_at as u64).to_le_bytes());
        d.extend((i as u64).to_le_bytes());
        d.extend((data_len as u64).to_le_bytes());
    }
    let mut meta = [0u8; 16];
    meta[0..4].copy_from_slice(&(d.len() as u32).to_le_bytes());
    meta[12..16].copy_from_slice(&(entries as u32).to_le_bytes());
    let e = depot::read(&d, &meta).unwrap_err().to_string();
    assert!(e.contains("more than the payload"), "{e}");
}

/// A pointer to an import the document does not have made `compact` index its import map out of range (a panic); it is
/// an error naming the pointer, and a pointer to a missing instance is the same.
#[test]
fn compacting_a_document_with_a_dangling_pointer_is_an_error() {
    use frostbite_formats::ebx::{self, Pointer, Value, carry};
    for dangling in [Pointer::External(9), Pointer::Internal(9)] {
        let mut doc = ebx::read(include_bytes!("fixtures/ebx/refl.ebx")).unwrap();
        let root = doc.instances[0].object.as_mut().unwrap();
        *root.field_mut("child").unwrap() = Value::Pointer(dangling);
        let e = carry::compact(&mut doc).unwrap_err().to_string();
        assert!(e.contains("pointer to") && e.contains('9'), "{e}");
    }
}
