//! Tests of the rigid mesh builder (`frostbite_formats::rigid`): the stream declaration's stored bytes, a built mesh
//! that writes and reads back, and the refusals for parts the builder cannot lay out.

#![allow(clippy::unwrap_used)]

use frostbite_formats::{meshset, rigid};

fn triangle() -> rigid::Part {
    rigid::Part {
        material: "default_mat".into(),
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: vec![[0.0, 0.0, 1.0]; 3],
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; 3],
        uv: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        triangles: vec![[0, 1, 2]],
        ..rigid::Part::default()
    }
}

#[test]
fn declaration_stores_three_streams() {
    let d = rigid::declaration();
    let mut want = [0u8; meshset::DECL_SIZE];
    for (e, slot) in want.as_chunks_mut::<4>().0.iter_mut().take(16).enumerate() {
        slot.copy_from_slice(&match e {
            0 => [1, 0x08, 0, 0],
            1 => [33, 0x06, 0, 1],
            2 => [52, 0x0d, 0, 2],
            _ => [0, 0, 0xff, 0],
        });
    }
    want[64] = 8;
    want[66] = 4;
    want[68] = 4;
    want[96] = 3;
    want[97] = 3;
    assert_eq!(d.bytes, want);
    assert_eq!(meshset::Decl::read(&d.bytes).unwrap(), d);
}

#[test]
fn built_mesh_writes_and_reads_back() {
    let built = rigid::build("world/test/a_mesh", [7; 16], &[triangle()]).unwrap();
    // 3 vertices of 16 bytes, padded to 16, then 3 indices of 2 bytes padded to 4, the chunk padded to 16
    assert_eq!(built.chunk.len(), 64);
    let (bytes, _meta) = meshset::write(&built.meshset).unwrap();
    let read = meshset::read(&bytes).unwrap();
    assert_eq!(read.lods[0].sections[0].vertices, 3);
    assert_eq!(read.lods[0].sections[0].primitives, 1);
}

#[test]
fn refuses_parts_it_cannot_lay_out() {
    let mut past = triangle();
    past.triangles = vec![[0, 1, 3]];
    let e = rigid::build("m", [0; 16], &[past.clone()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("past its 3 vertices"), "{e}");
    // the measure refuses the same index instead of indexing past the vertices
    let e = rigid::measure(&past).unwrap_err().to_string();
    assert!(e.contains("index 3 past the 3 vertices"), "{e}");
    let mut short = triangle();
    short.uv.pop();
    assert!(
        rigid::measure(&{
            let mut p = short.clone();
            p.triangles = vec![[0, 1, 2]];
            p
        })
        .is_err()
    );
    assert!(rigid::build("m", [0; 16], &[short]).is_err());
    assert!(rigid::build("m", [0; 16], &[]).is_err());
    assert!(rigid::build("m", [0; 16], &vec![triangle(); 256]).is_err());
}
