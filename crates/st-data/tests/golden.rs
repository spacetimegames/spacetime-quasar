//! Golden content hashes, canonical-encoding bytes and data-set hashes.
//!
//! These pin the canonical encoding v1: a change to any recorded value means
//! the encoding (or a schema, without a version bump) changed. They run on
//! every CI target.

mod common;

use common::{
    Catalog, MINIMAL_V3, SHUTTLE_V1, SHUTTLE_V2, SHUTTLE_V3, Section, ShipDesign, design,
    load_design, v1, v2,
};
use serde::Serialize;
use st_data::{
    Code, ContentHash, DataSetEntry, Loader, canonical_bytes, content_hash, data_set_hash,
};

const GOLDEN_SHUTTLE_V3: &str =
    "b3:a9513a06f037cb8e9cb2bdba9a75a003aa6c305c7a09e82cea4af2295e507fc2";
const GOLDEN_MINIMAL_V3: &str =
    "b3:3c04e17b3c33f5bba49b9665910e899992b8b6b85b9f6135374d47fee1ded088";
const GOLDEN_SHUTTLE_V2: &str =
    "b3:68629c42d34ab81c474f21d69ec03c38f1bd0e1468cdcea870d30340e2399936";

#[test]
fn golden_content_hashes() {
    let shuttle = load_design("assets/ships/shuttle.ron", SHUTTLE_V3);
    assert_eq!(shuttle.hash.to_string(), GOLDEN_SHUTTLE_V3);
    let minimal = load_design("assets/ships/minimal.ron", MINIMAL_V3);
    assert_eq!(minimal.hash.to_string(), GOLDEN_MINIMAL_V3);
    let v2 = load_design("designs/shuttle_v2.ron", SHUTTLE_V2);
    assert_eq!(v2.hash.to_string(), GOLDEN_SHUTTLE_V2);
}

#[test]
fn an_old_file_hashes_as_the_value_it_migrates_to() {
    let v1 = load_design("designs/shuttle_v1.ron", SHUTTLE_V1);
    assert_eq!(v1.file_version, 1);
    let minimal = load_design("designs/minimal.ron", MINIMAL_V3);
    assert_eq!(v1.value, minimal.value);
    assert_eq!(v1.hash, minimal.hash);

    let v2 = load_design("designs/shuttle_v2.ron", SHUTTLE_V2);
    assert_eq!(v2.file_version, 2);
    assert_eq!(v2.value.armor, 3.0);
    assert!(v2.value.sections.is_empty());
}

#[test]
fn the_hash_ignores_syntax() {
    let reference = load_design("a.ron", SHUTTLE_V3).hash;

    // Field order, comments, trailing commas, struct names, `implicit_some`
    // attributes and an unusual layout do not matter.
    let variants = [
        r#"(data:(tags:["civilian","small"],sections:[Cargo(capacity:40),Engine(0.5),Core],armor:3.0,mass:12.5,name:"Shuttle"),version:3,kind:"ship_design")"#,
        r#"#![enable(implicit_some)]
           /* leading /* nested */ comment */
           Envelope(kind: "ship_design", version: 3, // note
             data: ShipDesign(name: "Shuttle", mass: 12.50, armor: 3, sections: [Cargo(capacity: 40,), Engine(5e-1), Core,],
               tags: ["civilian", "small",],),)"#,
    ];
    for text in variants {
        assert_eq!(load_design("b.ron", text).hash, reference, "{text}");
    }

    // An omitted field versus its default written out.
    let omitted = design(r#"(name: "A", mass: 1.0)"#);
    let written = design(r#"(name: "A", mass: 1.0, armor: 0.0, sections: [], tags: [])"#);
    assert_eq!(
        load_design("c.ron", &omitted).hash,
        load_design("d.ron", &written).hash
    );
}

#[test]
fn the_hash_follows_the_value() {
    let base = load_design("a.ron", &design(r#"(name: "A", mass: 1.0)"#)).hash;
    let other = load_design("a.ron", &design(r#"(name: "A", mass: 1.5)"#)).hash;
    assert_ne!(base, other);
    // `-0.0` is kept as written: it is a different value from `0.0`.
    let zero = load_design("a.ron", &design(r#"(name: "A", mass: 1.0, armor: 0.0)"#)).hash;
    let negative_zero =
        load_design("a.ron", &design(r#"(name: "A", mass: 1.0, armor: -0.0)"#)).hash;
    assert_ne!(zero, negative_zero);
}

#[test]
fn content_hash_text_form_round_trips() {
    let hash = load_design("a.ron", SHUTTLE_V3).hash;
    let text = hash.to_string();
    assert!(text.starts_with("b3:"));
    assert_eq!(text.len(), 3 + 64);
    assert_eq!(text.parse::<ContentHash>().unwrap(), hash);
    assert!(text.to_uppercase().parse::<ContentHash>().is_err());
    assert!(text[..text.len() - 1].parse::<ContentHash>().is_err());
}

#[test]
fn content_hash_of_a_value_matches_the_hash_of_its_file() {
    let loaded = load_design("a.ron", SHUTTLE_V3);
    assert_eq!(content_hash(&loaded.value).unwrap(), loaded.hash);
    let v1 = v1::ShipDesign {
        name: "Shuttle".into(),
        mass: 12.5,
    };
    let v2 = v2::ShipDesign {
        name: "Shuttle".into(),
        mass: 12.5,
        armor: 0.0,
    };
    // The version is part of the hash input.
    assert_ne!(
        content_hash(&v1).unwrap().to_string(),
        content_hash(&v2).unwrap().to_string()
    );
}

// ---------------------------------------------------------------------------
// The canonical encoding, byte for byte.

#[derive(Serialize)]
struct AB {
    b: u8,
    a: u8,
}

#[derive(Serialize)]
struct BA {
    a: u8,
    b: u8,
}

#[derive(Serialize)]
enum E {
    Unit,
    New(u16),
    Tuple(u8, u8),
    Struct { y: bool, x: bool },
}

#[test]
fn encoding_bytes_of_scalars_and_containers() {
    let len = |n: u64| n.to_le_bytes().to_vec();
    let cat = |parts: &[&[u8]]| parts.concat();

    assert_eq!(canonical_bytes(&true).unwrap(), [1]);
    assert_eq!(canonical_bytes(&-2i16).unwrap(), [0xfe, 0xff]);
    assert_eq!(canonical_bytes(&0x0102_0304u32).unwrap(), [4, 3, 2, 1]);
    assert_eq!(
        canonical_bytes(&1.0f32).unwrap(),
        0x3f80_0000u32.to_le_bytes()
    );
    assert_eq!(
        canonical_bytes(&-0.0f64).unwrap(),
        0x8000_0000_0000_0000u64.to_le_bytes()
    );
    assert_eq!(canonical_bytes(&'é').unwrap(), 0xe9u32.to_le_bytes());
    assert_eq!(
        canonical_bytes("é").unwrap(),
        cat(&[&len(2), "é".as_bytes()])
    );
    assert_eq!(canonical_bytes(&()).unwrap(), Vec::<u8>::new());
    assert_eq!(canonical_bytes(&None::<u8>).unwrap(), [0]);
    assert_eq!(canonical_bytes(&Some(7u8)).unwrap(), [1, 7]);
    assert_eq!(
        canonical_bytes(&vec![1u8, 2]).unwrap(),
        cat(&[&len(2), &[1, 2]])
    );
    assert_eq!(canonical_bytes(&(1u8, 2u8)).unwrap(), [1, 2]);

    // Fields are identified by name and sorted by it, whatever the Rust order.
    let expected = cat(&[&len(2), &len(1), b"a", &[1], &len(1), b"b", &[2]]);
    assert_eq!(canonical_bytes(&AB { b: 2, a: 1 }).unwrap(), expected);
    assert_eq!(canonical_bytes(&BA { a: 1, b: 2 }).unwrap(), expected);

    // Variants are identified by name.
    assert_eq!(canonical_bytes(&E::Unit).unwrap(), cat(&[&len(4), b"Unit"]));
    assert_eq!(
        canonical_bytes(&E::New(0x0102)).unwrap(),
        cat(&[&len(3), b"New", &[2, 1]])
    );
    assert_eq!(
        canonical_bytes(&E::Tuple(1, 2)).unwrap(),
        cat(&[&len(5), b"Tuple", &[1, 2]])
    );
    assert_eq!(
        canonical_bytes(&E::Struct { y: true, x: false }).unwrap(),
        cat(&[
            &len(6),
            b"Struct",
            &len(2),
            &len(1),
            b"x",
            &[0],
            &len(1),
            b"y",
            &[1]
        ])
    );
}

#[test]
fn map_entries_are_sorted_by_the_bytes_of_the_encoded_key() {
    use std::collections::BTreeMap;
    let mut map = BTreeMap::new();
    map.insert(1u16, 0xaau8);
    map.insert(256u16, 0xbbu8);
    // 256 encodes as [0, 1] and sorts before 1, which encodes as [1, 0]:
    // the order is that of the bytes, not of the keys.
    let bytes = canonical_bytes(&map).unwrap();
    let mut expected = 2u64.to_le_bytes().to_vec();
    expected.extend_from_slice(&[0, 1, 0xbb, 1, 0, 0xaa]);
    assert_eq!(bytes, expected);
}

#[test]
fn unique_map_hashes_like_a_btree_map_and_ignores_file_order() {
    let a = r#"(kind: "catalog", version: 1, data: (prices: {"b": 2, "a": 1}))"#;
    let b = r#"(kind: "catalog", version: 1, data: (prices: {"a": 1, "b": 2}))"#;
    let loader = Loader::new();
    let ha = loader.load::<Catalog>("a.ron", a.as_bytes()).unwrap().hash;
    let hb = loader.load::<Catalog>("b.ron", b.as_bytes()).unwrap().hash;
    assert_eq!(ha, hb);
}

#[test]
fn the_encoder_rejects_what_it_cannot_encode() {
    let nan = canonical_bytes(&f32::NAN).unwrap_err();
    assert_eq!(nan.code, Code::NonFinite);
    assert_eq!(
        canonical_bytes(&f64::INFINITY).unwrap_err().code,
        Code::NonFinite
    );
    assert_eq!(
        canonical_bytes(&f32::NEG_INFINITY).unwrap_err().code,
        Code::NonFinite
    );
    assert_eq!(canonical_bytes(&1u128).unwrap_err().code, Code::Type);
    assert_eq!(canonical_bytes(&-1i128).unwrap_err().code, Code::Type);

    // The path of the offending value is reported.
    #[derive(Serialize)]
    struct Inner {
        values: Vec<f32>,
    }
    #[derive(Serialize)]
    struct Outer {
        inner: Inner,
    }
    let e = canonical_bytes(&Outer {
        inner: Inner {
            values: vec![1.0, f32::NAN],
        },
    })
    .unwrap_err();
    assert_eq!(e.path.to_string(), "inner.values[1]");

    // Skipped fields would be invisible to the hash.
    #[derive(Serialize)]
    struct Skips {
        #[serde(skip_serializing_if = "Option::is_none")]
        maybe: Option<u8>,
    }
    assert_eq!(
        canonical_bytes(&Skips { maybe: None }).unwrap_err().code,
        Code::Type
    );
}

#[test]
fn a_map_with_equal_encoded_keys_is_an_error() {
    struct Dup;
    impl Serialize for Dup {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut m = s.serialize_map(Some(2))?;
            m.serialize_entry("k", &1u8)?;
            m.serialize_entry("k", &2u8)?;
            m.end()
        }
    }
    let e = canonical_bytes(&Dup).unwrap_err();
    assert_eq!(e.code, Code::DuplicateKey);
    assert_eq!(e.path.to_string(), "k");
}

#[test]
fn sections_survive_the_chain() {
    let shuttle = load_design("a.ron", SHUTTLE_V3);
    assert_eq!(
        shuttle.value,
        ShipDesign {
            name: "Shuttle".into(),
            mass: 12.5,
            armor: 3.0,
            sections: vec![
                Section::Cargo { capacity: 40 },
                Section::Engine(0.5),
                Section::Core
            ],
            tags: vec!["civilian".into(), "small".into()],
        }
    );
}

// ---------------------------------------------------------------------------
// The data-set hash.

fn entry(kind: &str, source: &str, hash: &ContentHash) -> DataSetEntry {
    DataSetEntry {
        kind: kind.into(),
        source: source.into(),
        hash: *hash,
    }
}

const GOLDEN_DATA_SET: &str = "b3:ded16101f0fc8b882c5a75a36ceb8184d9773c36b2a30914bd503ff8341a8ec7";

#[test]
fn golden_data_set_hash() {
    let shuttle = load_design("assets/ships/shuttle.ron", SHUTTLE_V3).hash;
    let minimal = load_design("assets/ships/minimal.ron", MINIMAL_V3).hash;
    let set = [
        entry("ship_design", "assets/ships/shuttle.ron", &shuttle),
        entry("ship_design", "assets/ships/minimal.ron", &minimal),
    ];
    let hash = data_set_hash(&set).unwrap();
    assert_eq!(hash.to_string(), GOLDEN_DATA_SET);
    // The order of the input does not matter.
    let reversed = [set[1].clone(), set[0].clone()];
    assert_eq!(data_set_hash(&reversed).unwrap(), hash);
    // Any change to a member changes the hash.
    let changed = [
        entry("ship_design", "assets/ships/shuttle.ron", &minimal),
        set[1].clone(),
    ];
    assert_ne!(data_set_hash(&changed).unwrap(), hash);
    // So does the source name and the kind.
    let renamed = [
        entry("ship_design", "assets/ships/shuttle2.ron", &shuttle),
        set[1].clone(),
    ];
    assert_ne!(data_set_hash(&renamed).unwrap(), hash);
    let rekinded = [
        entry("other", "assets/ships/shuttle.ron", &shuttle),
        set[1].clone(),
    ];
    assert_ne!(data_set_hash(&rekinded).unwrap(), hash);
}

#[test]
fn data_set_order_is_bytewise_on_the_utf8_source_names() {
    let h = ContentHash::from_bytes([7; 32]);
    let sources = ["z.ron", "a/b.ron", "a.ron", "B.ron", "é.ron", "a-b.ron"];
    let mut expected: Vec<&str> = sources.to_vec();
    // Plain byte order: uppercase before lowercase, `-` (2D) and `.` (2E)
    // before `/` (2F), and non-ASCII (lead byte >= C0) after all ASCII.
    expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(
        expected,
        ["B.ron", "a-b.ron", "a.ron", "a/b.ron", "z.ron", "é.ron"]
    );

    let forward: Vec<_> = expected.iter().map(|s| entry("k", s, &h)).collect();
    let mut backward = forward.clone();
    backward.reverse();
    assert_eq!(
        data_set_hash(&forward).unwrap(),
        data_set_hash(&backward).unwrap()
    );
    // A different permutation of the same set gives the same hash.
    let shuffled: Vec<_> = sources.iter().map(|s| entry("k", s, &h)).collect();
    assert_eq!(
        data_set_hash(&shuffled).unwrap(),
        data_set_hash(&forward).unwrap()
    );
}

#[test]
fn data_set_edge_cases() {
    let h = ContentHash::from_bytes([1; 32]);
    let empty = data_set_hash(&[]).unwrap();
    let one = data_set_hash(&[entry("k", "a.ron", &h)]).unwrap();
    assert_ne!(empty, one);
    let dup = data_set_hash(&[entry("k", "a.ron", &h), entry("k", "a.ron", &h)]).unwrap_err();
    assert_eq!(dup.code, Code::DuplicateKey);
    assert!(empty.to_string().starts_with("b3:"));
}

/// Pins the byte-wise order with a preimage built by hand and hashed with
/// `blake3` directly, so a case-folding, locale or Unicode-aware sort fails.
#[test]
fn data_set_hash_matches_a_hand_built_bytewise_preimage() {
    // Byte-wise: `B` (42) < `a` (61); then `a-b.ron` (2D) < `a.ron` (2E) <
    // `a/b.ron` (2F); `é` (C3 A9) last. A case-folding sort would put
    // `B.ron` after the `a*` names; a locale sort would put `é.ron` after `a`.
    let ordered = ["B.ron", "a-b.ron", "a.ron", "a/b.ron", "é.ron"];
    let hash_of = |i: usize| ContentHash::from_bytes([u8::try_from(i).unwrap() + 1; 32]);

    let mut preimage = Vec::new();
    preimage.extend_from_slice(&5u64.to_le_bytes());
    for (i, source) in ordered.iter().enumerate() {
        for text in ["k", source] {
            preimage.extend_from_slice(&(text.len() as u64).to_le_bytes());
            preimage.extend_from_slice(text.as_bytes());
        }
        preimage.extend_from_slice(&32u64.to_le_bytes());
        preimage.extend_from_slice(hash_of(i).as_bytes());
    }
    let mut hasher = blake3::Hasher::new_derive_key("st-data 2026-10-11 data set hash v1");
    hasher.update(&preimage);
    let expected = *hasher.finalize().as_bytes();

    // Supplied in a scrambled order.
    let scrambled = [4usize, 0, 3, 1, 2];
    let entries: Vec<_> = scrambled
        .iter()
        .map(|&i| entry("k", ordered[i], &hash_of(i)))
        .collect();
    assert_eq!(data_set_hash(&entries).unwrap().as_bytes(), &expected);
}
