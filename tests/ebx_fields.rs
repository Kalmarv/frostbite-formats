//! EBX field semantics on documents built here, byte by byte, from the format's rules: the two 64-bit integer codes
//! (0x11 signed, 0x12 unsigned), floats that are not finite, and type references, which name a slot of the EFIX type
//! list and not an entry of the reflection table. Each document has one exported `Wide` instance whose fields are
//! built from the arguments, and an EFIX list that may order its types differently from the reflection table (and
//! names one the table lacks), so that an index into the wrong list resolves to the wrong type.
//!
//! The values asserted are the ones the bytes are built to hold; the 64-bit codes' signedness is the game's own (its
//! field names, `CompareInt64EntityData.A` is code 0x11, `CompareUint64EntityData.A` code 0x12), checked over the
//! install by `real_data_fields.rs`.

#![allow(clippy::unwrap_used)]

use frostbite_formats::ebx::{self, Document, FieldType, Value, carry};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The identities a document's types have: the reflected `Wide` and `Other`, and one the table does not reflect.
const WIDE: usize = 0;
const OTHER: usize = 1;
const UNREFLECTED: usize = 2;
const GUIDS: [[u8; 16]; 3] = [[0x10; 16], [0x11; 16], [0x12; 16]];
const SIGNATURES: [u32; 3] = [0x1111_1111, 0x2222_2222, 0x3333_3333];

/// What the instance's fields hold, as the bytes of each.
#[derive(Clone, Copy)]
struct Fields {
    /// Code 0x11.
    stamp: u64,
    /// Code 0x12.
    mask: u64,
    /// A 32-bit float.
    gain: u32,
    /// A 64-bit float.
    ratio: u64,
    /// Type references as stored.
    kind: u32,
    other: u32,
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

fn words(out: &mut Vec<u8>, w: &[u32]) {
    for x in w {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

/// A REFL document. `efix` says which identity each slot of the EFIX type list holds.
fn build(f: Fields, efix: [usize; 3]) -> Vec<u8> {
    let slot_of = |identity: usize| efix.iter().position(|&i| i == identity).unwrap() as u16;

    // EBXD: 12 bytes to the 16-aligned start of the data, then the exported instance's GUID, its 56 bytes
    let mut data = vec![0u8; 12];
    data.extend_from_slice(&[0xAB; 16]);
    data.extend_from_slice(&slot_of(WIDE).to_le_bytes());
    data.extend_from_slice(&[0; 6]);
    data.extend_from_slice(&f.stamp.to_le_bytes());
    data.extend_from_slice(&f.mask.to_le_bytes());
    data.extend_from_slice(&f.gain.to_le_bytes());
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(&f.ratio.to_le_bytes());
    for t in [f.kind, f.other] {
        words(&mut data, &[t, 0]);
    }
    // the writer's layout: the array section after the instance at the next 16 bytes, 32 bytes of it, then the
    // boxed-value and string sections (both empty)
    data.resize(12 + 112, 0);

    let mut efix_body = vec![0xA0; 16];
    words(&mut efix_body, &[3]);
    efix.iter().for_each(|&i| efix_body.extend_from_slice(&GUIDS[i]));
    words(&mut efix_body, &[3]);
    words(&mut efix_body, &efix.map(|i| SIGNATURES[i]));
    // one exported instance at data offset 16; no pointers, resources, imports; the type references that are set
    words(&mut efix_body, &[1, 1, 16, 0, 0, 0, 0]);
    let set: Vec<u32> = [(f.kind, 56), (f.other, 64)]
        .into_iter()
        .filter(|(t, _)| *t != 0)
        .map(|(_, at)| at)
        .collect();
    words(&mut efix_body, &[u32::try_from(set.len()).unwrap()]);
    words(&mut efix_body, &set);
    // the section offsets, then the (empty) list of boxed-value references
    words(&mut efix_body, &[80, 112, 112, 0]);

    let names = ["Wide", "Other", "stamp", "mask", "gain", "ratio", "kind", "other"];
    let mut strings = Vec::new();
    let offsets: Vec<u32> = names
        .iter()
        .map(|n| {
            let at = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(n.as_bytes());
            strings.push(0);
            at
        })
        .collect();
    let stored = |ty: FieldType| (ty as u16) << 5;
    let mut refl = Vec::new();
    words(&mut refl, &[2]);
    for i in [WIDE, OTHER] {
        refl.extend_from_slice(&GUIDS[i]);
        words(&mut refl, &[SIGNATURES[i]]);
    }
    words(&mut refl, &[2]);
    for (name, fields, size) in [(offsets[0], 6u16, 56u16), (offsets[1], 0, 8)] {
        words(&mut refl, &[name, 0]);
        for h in [fields, stored(FieldType::Structure), size, 8] {
            refl.extend_from_slice(&h.to_le_bytes());
        }
    }
    words(&mut refl, &[6]);
    let fields = [
        (FieldType::Int64, 8),
        (FieldType::UInt64, 16),
        (FieldType::Float32, 24),
        (FieldType::Float64, 32),
        (FieldType::TypeRef, 40),
        (FieldType::TypeRef, 48),
    ];
    for (k, (ty, at)) in fields.into_iter().enumerate() {
        words(&mut refl, &[offsets[2 + k], at]);
        refl.extend_from_slice(&stored(ty).to_le_bytes());
        refl.extend_from_slice(&0u16.to_le_bytes());
    }
    // no array groups, no mapping
    words(&mut refl, &[0, 0]);
    refl.extend_from_slice(&strings);

    let mut body = b"EBX\0".to_vec();
    chunk(&mut body, b"EBXD", &data);
    chunk(&mut body, b"EFIX", &efix_body);
    let mut extra = Vec::new();
    words(&mut extra, &[0, 0]);
    chunk(&mut body, b"EBXX", &extra);
    chunk(&mut body, b"REFL", &refl);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn fields_of(d: &Document) -> &ebx::Object {
    d.instances[0].object.as_ref().unwrap()
}

fn plain() -> Fields {
    Fields {
        stamp: 0,
        mask: 0,
        gain: 0,
        ratio: 0,
        kind: 0,
        other: 0,
    }
}

#[test]
fn code_0x11_is_signed_and_0x12_unsigned() -> R {
    assert_eq!(FieldType::from_bits(0x11), FieldType::Int64);
    assert_eq!(FieldType::from_bits(0x12), FieldType::UInt64);
    let bytes = build(
        Fields {
            stamp: u64::MAX - 1,
            mask: u64::MAX - 1,
            ..plain()
        },
        [WIDE, OTHER, UNREFLECTED],
    );
    let doc = ebx::read(&bytes)?;
    let o = fields_of(&doc);
    assert_eq!(o.field("stamp"), Some(&Value::Int(-2)));
    assert_eq!(o.field("mask"), Some(&Value::UInt(u64::MAX - 1)));
    assert_eq!(ebx::write(&doc)?, bytes);

    // each field takes the range of its own signedness and no other
    let mut d = doc.clone();
    let o = d.instances[0].object.as_mut().unwrap();
    *o.field_mut("stamp").unwrap() = Value::UInt(1 << 63);
    assert!(ebx::write(&d).is_err(), "2^63 does not fit a signed field");
    let o = d.instances[0].object.as_mut().unwrap();
    *o.field_mut("stamp").unwrap() = Value::Int(i64::MIN);
    *o.field_mut("mask").unwrap() = Value::Int(-1);
    assert!(ebx::write(&d).is_err(), "-1 does not fit an unsigned field");
    Ok(())
}

#[test]
fn floats_keep_their_bits_whatever_they_are() -> R {
    // a quiet NaN, a signalling NaN with a payload, an infinity; and for the double a NaN with a low payload bit set
    for (gain, ratio) in [
        (0x7FC0_0000u32, 0x7FF8_0000_0000_0000u64),
        (0x7F80_0001, 0xFFF0_0000_0000_0001),
        (0xFFC0_1234, 0x7FF8_DEAD_BEEF_0001),
        (0x7F80_0000, 0xFFF0_0000_0000_0000),
        (0xFF80_0000, 0x7FF0_0000_0000_0000),
        (0x0000_0001, 0x0000_0000_0000_0001),
    ] {
        let bytes = build(
            Fields {
                gain,
                ratio,
                ..plain()
            },
            [WIDE, OTHER, UNREFLECTED],
        );
        let doc = ebx::read(&bytes)?;
        let o = fields_of(&doc);
        let (Some(Value::Float(g)), Some(Value::Float(r))) = (o.field("gain"), o.field("ratio")) else {
            panic!("the floats are not read as floats");
        };
        assert_eq!(f32::from_bits(gain).is_nan(), g.is_nan(), "{gain:#x}");
        assert_eq!(f64::from_bits(ratio).is_nan(), r.is_nan(), "{ratio:#x}");
        assert_eq!(r.to_bits(), ratio, "the double's bits are the stored bits");
        assert_eq!(
            ebx::write(&doc)?,
            bytes,
            "{gain:#x} {ratio:#x} are written back as stored"
        );
    }
    // a value no 32-bit float holds is still refused; a NaN the program makes is written as a NaN
    let doc = ebx::read(&build(plain(), [WIDE, OTHER, UNREFLECTED]))?;
    let mut d = doc.clone();
    *d.instances[0].object.as_mut().unwrap().field_mut("gain").unwrap() = Value::Float(1.0e300);
    assert!(ebx::write(&d).is_err());
    *d.instances[0].object.as_mut().unwrap().field_mut("gain").unwrap() = Value::Float(f64::NAN);
    let back = ebx::read(&ebx::write(&d)?)?;
    assert!(matches!(fields_of(&back).field("gain"), Some(Value::Float(g)) if g.is_nan()));
    // a double NaN whose payload is entirely below what a 32-bit float keeps stays a NaN there
    *d.instances[0].object.as_mut().unwrap().field_mut("gain").unwrap() =
        Value::Float(f64::from_bits(0x7FF0_0000_0000_0001));
    let back = ebx::read(&ebx::write(&d)?)?;
    assert!(matches!(fields_of(&back).field("gain"), Some(Value::Float(g)) if g.is_nan()));
    Ok(())
}

/// A type reference is `slot << 2 | low bits` and `slot` indexes the EFIX type list.
#[test]
fn a_type_reference_names_a_slot_of_the_efix_list() -> R {
    // EFIX order U, O, W; the table reflects W as type 0 and O as type 1
    let efix = [UNREFLECTED, OTHER, WIDE];
    let bytes = build(
        Fields {
            kind: 2 << 2 | 1,
            other: 1 << 2,
            ..plain()
        },
        efix,
    );
    let doc = ebx::read(&bytes)?;
    assert_eq!(doc.types[0].name, "Wide");
    assert_eq!(doc.types[1].name, "Other");
    let o = fields_of(&doc);
    let Some(Value::TypeRef(kind)) = o.field("kind") else {
        panic!("kind is not a type reference");
    };
    let Some(Value::TypeRef(other)) = o.field("other") else {
        panic!("other is not a type reference");
    };
    assert_eq!((kind.fixup_type, kind.encoded), (Some(2), 9));
    assert_eq!((other.fixup_type, other.encoded), (Some(1), 4));
    // slot 2 holds Wide, which the table reflects as type 0; slot 1 holds Other, type 1
    assert_eq!(doc.type_ref_target(kind), Some(0));
    assert_eq!(doc.type_ref_target(other), Some(1));
    assert_eq!(
        doc.fixup_type_target(0),
        None,
        "slot 0 names a type the table does not reflect"
    );
    assert_eq!(doc.fixup_type_target(9), None);
    assert_eq!(ebx::write(&doc)?, bytes);

    // a reference past the list is refused by the writer, not written
    let mut d = doc.clone();
    *d.instances[0].object.as_mut().unwrap().field_mut("kind").unwrap() =
        Value::TypeRef(frostbite_formats::ebx::TypeRef {
            fixup_type: Some(3),
            encoded: 3 << 2,
            ..*kind
        });
    assert!(ebx::write(&d).is_err());
    Ok(())
}

#[test]
fn a_type_reference_moves_to_the_slot_of_the_target_document() -> R {
    // the donor lists U, O, W; the target lists W, O, U: every slot differs, one type is not reflected by either
    let donor = ebx::read(&build(
        Fields {
            kind: 2 << 2 | 1,
            other: 2, // slot 0, low bits 2
            ..plain()
        },
        [UNREFLECTED, OTHER, WIDE],
    ))?;
    let mut target = ebx::read(&build(plain(), [WIDE, OTHER, UNREFLECTED]))?;
    let copied = {
        let mut c = carry::Carrier::new(&donor, &mut target);
        c.object(fields_of(&donor))?
    };
    let get = |name: &str| match copied.field(name) {
        Some(Value::TypeRef(t)) => *t,
        other => panic!("{name} is {other:?}"),
    };
    // Wide is slot 0 of the target; the unreflected type slot 2; the low bits travel unchanged
    assert_eq!((get("kind").fixup_type, get("kind").encoded), (Some(0), 1));
    assert_eq!(
        (get("other").fixup_type, get("other").encoded),
        (Some(2), 2 << 2 | 2)
    );
    // written and read again they name the same types as in the donor
    target.instances[0].object.as_mut().unwrap().fields = copied.fields;
    let back = ebx::read(&ebx::write(&target)?)?;
    let Some(Value::TypeRef(kind)) = fields_of(&back).field("kind") else {
        panic!("kind is not a type reference");
    };
    assert_eq!(back.type_ref_target(kind), Some(0));
    assert_eq!(back.types[0].name, "Wide");

    // a donor type the target's list lacks is appended to it
    let mut sparse = ebx::read(&build(plain(), [WIDE, OTHER, UNREFLECTED]))?;
    sparse.fixup_type_guids.pop();
    sparse.fixup_type_signatures.pop();
    let before = sparse.fixup_type_guids.len();
    let copied = {
        let mut c = carry::Carrier::new(&donor, &mut sparse);
        c.object(fields_of(&donor))?
    };
    assert_eq!(sparse.fixup_type_guids.len(), before + 1);
    assert_eq!(sparse.fixup_type_guids[before], GUIDS[UNREFLECTED]);
    let Some(Value::TypeRef(other)) = copied.field("other") else {
        panic!("other is not a type reference");
    };
    assert_eq!(other.fixup_type, Some(before));
    Ok(())
}
