//! Shader parameter keys ([`frostbite_formats::paramdb`]): the name hash, the 16-byte layout and the table of names.
//!
//! The expected values are measured, not derived: each row of `MEASURED` is a parameter asset of the skate. install
//! (build 20260929) with the key the stock documents tie to it (an `AppearanceExpressionParamInfo` whose `Value.Parameter`
//! points at the asset), read by an independent EBX reader in a census of all 328,015 stock payloads. tests/real_data_fields.rs
//! repeats the check over the whole install.

#![allow(clippy::unwrap_used)]

use frostbite_formats::depot;
use frostbite_formats::names::GuidNames;
use frostbite_formats::paramdb::{KeyUse, LOW48, NAME_HASH_SEED, ParamKey, ParamNames, name_hash};

/// (asset name as the bundle manifests state it, key hash, type key, count, identity, name hash = identity << 16 | hash >> 48)
const MEASURED: [(&str, u64, u32, u16, u16, u32); 10] = [
    (
        "common/shadercomponents/parameters/base/param_base_c",
        0x79a3_4861_0bb2_3445,
        0x14c5_fc30,
        1,
        0x99e8,
        0x99e8_79a3,
    ),
    (
        "common/shadercomponents/parameters/base/param_base_ny",
        0xaf17_ab37_3f4c_d4ca,
        0x14c5_fc30,
        1,
        0xd6f7,
        0xd6f7_af17,
    ),
    (
        "common/shadercomponents/parameters/character/clothing/graphic/graphic0/param_graphic0_enum",
        0xbddf_41e0_469b_5631,
        0x8e4a_817e,
        1,
        0xab74,
        0xab74_bddf,
    ),
    (
        "common/shadercomponents/parameters/character/clothing/graphic/graphic0/param_graphic0_scale",
        0xda14_b611_faaf_a109,
        0x2bb6_5c2d,
        1,
        0x1bdc,
        0x1bdc_da14,
    ),
    (
        "common/shadercomponents/parameters/character/clothing/graphic/graphic0/param_graphic0_tint",
        0xd98b_4359_6948_5311,
        0x885e_ff59,
        1,
        0xab7d,
        0xab7d_d98b,
    ),
    (
        "common/shadercomponents/parameters/character/clothing/graphic/param_usegraphic",
        0xa5f9_a6b8_0982_79ad,
        0x36ed_3444,
        1,
        0x825e,
        0x825e_a5f9,
    ),
    (
        "common/shadercomponents/parameters/character/clothing/pattern/param_pattern_c",
        0xd086_3882_22d8_5590,
        0x14c5_fc30,
        1,
        0x3bb7,
        0x3bb7_d086,
    ),
    (
        "common/shadercomponents/parameters/character/face/param_facebase_c",
        0x51e0_cac4_e89f_42e6,
        0x14c5_fc30,
        1,
        0xb82a,
        0xb82a_51e0,
    ),
    (
        "common/shadercomponents/parameters/regions/region/param_region_pattern_a",
        0x0252_afbe_59b5_8d97,
        0x2bb6_5c2d,
        1,
        0x1fb0,
        0x1fb0_0252,
    ),
    (
        "common/shadercomponents/parameters/regions/region2/param_region_color_2a",
        0xfca7_9ddc_3252_49ce,
        0x885e_ff59,
        1,
        0x1f99,
        0x1f99_fca7,
    ),
];

fn key(m: &(&str, u64, u32, u16, u16, u32)) -> ParamKey {
    ParamKey {
        hash: m.1,
        type_key: m.2,
        count: m.3,
        identity: m.4,
    }
}

#[test]
fn the_name_hash_of_an_asset_name_is_the_name_hash_of_its_key() {
    assert_eq!(NAME_HASH_SEED, 0x6875_ca4d);
    for m in &MEASURED {
        assert_eq!(name_hash(m.0.as_bytes()), m.5, "{}", m.0);
        assert_eq!(key(m).name_hash(), m.5, "{}", m.0);
        assert!(key(m).is_named(m.0.as_bytes()), "{}", m.0);
    }
    // another parameter's name does not hash to the key
    assert!(!key(&MEASURED[0]).is_named(MEASURED[1].0.as_bytes()));
    assert!(!key(&MEASURED[0]).is_named(b"param_base_c"));
}

#[test]
fn the_name_hash_ignores_ascii_case_and_a_last_byte_xors_into_the_low_byte() {
    let (path, _, _, _, _, expected) = MEASURED[9];
    assert_eq!(name_hash(path.to_ascii_uppercase().as_bytes()), expected);
    assert_eq!(
        name_hash(b"Common/ShaderComponents/Parameters/Regions/Region2/Param_Region_Color_2A"),
        expected
    );
    // two names that differ in their last byte only: the hashes differ by the XOR of the bytes, in the low byte
    let a = name_hash(path.as_bytes());
    let b = name_hash(path.replace("color_2a", "color_2b").as_bytes());
    assert_eq!(a ^ b, u32::from(b'a' ^ b'b'));
    // the empty name hashes to the seed
    assert_eq!(name_hash(b""), NAME_HASH_SEED);
}

#[test]
fn a_key_is_rebuilt_from_its_name_hash_and_its_other_48_bits() {
    for m in &MEASURED {
        let k = key(m);
        assert_eq!(ParamKey::from_parts(m.5, k.low48(), m.2, m.3), k, "{}", m.0);
        assert_eq!(k.low48(), m.1 & LOW48);
    }
}

#[test]
fn a_key_reads_and_writes_its_16_stored_bytes() {
    // the first measured key as depot records and ParamDbKeySerialized GUIDs store it
    let bytes = [
        0x45, 0x34, 0xb2, 0x0b, 0x61, 0x48, 0xa3, 0x79, 0x30, 0xfc, 0xc5, 0x14, 0x01, 0x00, 0xe8, 0x99,
    ];
    let k = ParamKey::from_bytes(&bytes).unwrap();
    assert_eq!(k, key(&MEASURED[0]));
    assert_eq!(k.to_bytes(), bytes);
    for m in &MEASURED {
        assert_eq!(ParamKey::from_bytes(&key(m).to_bytes()).unwrap(), key(m));
    }
}

#[test]
fn a_depot_parameter_gives_its_key() {
    // a block of one float parameter: the count word, the 16-byte key, the value count 1 and the value
    let k = key(&MEASURED[3]);
    let mut block = 1u32.to_le_bytes().to_vec();
    block.extend(k.to_bytes());
    block.extend(1u32.to_le_bytes());
    block.extend(0.5f32.to_le_bytes());
    let params = depot::params(&block).unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].key(), k);
    assert_eq!(params[0].slot(), 0xda14);
}

#[test]
fn names_are_kept_only_when_they_hash_to_the_key() {
    let mut files = GuidNames::new();
    for (i, m) in MEASURED.iter().enumerate() {
        files.insert([u8::try_from(i).unwrap(); 16], m.0);
    }
    files.insert(
        [0xee; 16],
        "common/shadercomponents/parameters/base/param_base_c_alias",
    );
    let use_of = |i: usize, guid: u8| KeyUse {
        key: key(&MEASURED[i]),
        asset: Some([guid; 16]),
        alias: None,
    };
    let mut names = ParamNames::new();
    let uses = [
        use_of(0, 0),
        use_of(1, 1),
        use_of(0, 0),
        // the asset of key 2 is named by another asset's name: refused
        use_of(2, 3),
        // the asset of key 4 is a GUID the table does not name
        use_of(4, 0xaa),
        // a name that does not hash to the key
        use_of(0, 0xee),
        KeyUse {
            key: key(&MEASURED[5]),
            asset: None,
            alias: Some("UseGraphic".to_string()),
        },
    ];
    let added = names.add(&uses, &files);
    assert_eq!(
        (added.added, added.repeated, added.unnamed, added.refused),
        (2, 1, 1, 2)
    );
    assert_eq!(names.len(), 2);
    assert_eq!(names.get(&key(&MEASURED[0])), Some(MEASURED[0].0));
    assert_eq!(names.by_hash(MEASURED[1].1), Some(MEASURED[1].0));
    assert_eq!(names.get(&key(&MEASURED[2])), None);
}
