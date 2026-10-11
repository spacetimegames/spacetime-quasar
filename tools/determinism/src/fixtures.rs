//! Scenario `data-fixtures` (ADR 0008, ADR 0010): the `st-data` golden
//! fixtures as one data set.
//!
//! The four `ship_design` fixture files of `st-data` (versions 1, 2 and 3,
//! and the version-3 file that the version-1 file migrates to) are loaded
//! through the migration chain, and their content hashes go into one
//! [data-set hash](st_data::data_set_hash). The content hash of a file is the
//! hash of the value it migrated to, so a platform that parses a float
//! differently, migrates differently or encodes differently hashes
//! differently. The scenario has a single checkpoint, at tick 0: FNV-1a 64 of
//! the 32 bytes of the data-set hash (format version 1, no length prefix),
//! so the comparison needs nothing but the existing line format.
//!
//! The files are `crates/st-data/tests/data/ship_design/v<N>/<case>.ron`
//! (ADR 0010), read with `include_str!`: no file system access at run time.
//! The schema below mirrors the one in `crates/st-data/tests/common`, which
//! is test code and cannot be reached from here; the unit tests pin the
//! content hashes to the golden values of `st-data`'s own tests, so the two
//! cannot drift apart unnoticed.

use serde::{Deserialize, Serialize};
use st_data::{
    DataSetEntry, DataSetHash, Diagnostic, Genesis, Loader, MigrationError, Schema, Validate,
    Validator, data_set_hash,
};
use st_sim::{DumpLine, StateHasher};

/// The scenario name in the reference file.
pub const NAME: &str = "data-fixtures";

mod v1 {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ShipDesign {
        pub name: String,
        pub mass: f32,
    }

    impl Schema for ShipDesign {
        const KIND: &'static str = "ship_design";
        const VERSION: u32 = 1;
        type Previous = Genesis;
        fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
            match previous {}
        }
    }
}

mod v2 {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ShipDesign {
        pub name: String,
        pub mass: f32,
        pub armor: f32,
    }

    impl Schema for ShipDesign {
        const KIND: &'static str = "ship_design";
        const VERSION: u32 = 2;
        type Previous = v1::ShipDesign;
        fn migrate(previous: v1::ShipDesign) -> Result<Self, MigrationError> {
            Ok(Self {
                name: previous.name,
                mass: previous.mass,
                armor: 0.0,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Section {
    Cargo { capacity: u32 },
    Engine(f32),
    Core,
}

/// The current version (3) of the fixture kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShipDesign {
    name: String,
    mass: f32,
    #[serde(default)]
    armor: f32,
    #[serde(default)]
    sections: Vec<Section>,
    #[serde(default)]
    tags: Vec<String>,
}

impl Schema for ShipDesign {
    const KIND: &'static str = "ship_design";
    const VERSION: u32 = 3;
    type Previous = v2::ShipDesign;
    fn migrate(previous: v2::ShipDesign) -> Result<Self, MigrationError> {
        Ok(Self {
            name: previous.name,
            mass: previous.mass,
            armor: previous.armor,
            sections: Vec::new(),
            tags: Vec::new(),
        })
    }
}

impl Validate for ShipDesign {
    fn validate(&self, v: &mut Validator) {
        v.field("mass", |v| {
            if self.mass <= 0.0 {
                v.error("ship_design.mass_positive", "mass must be positive");
            }
        });
        v.field("sections", |v| {
            for (i, section) in self.sections.iter().enumerate() {
                v.index(i, |v| {
                    if let Section::Cargo { capacity } = section {
                        v.variant("Cargo", |v| {
                            v.field("capacity", |v| {
                                if *capacity > 1000 {
                                    v.error("ship_design.capacity", "capacity above 1000");
                                }
                            });
                        });
                    }
                });
            }
        });
    }
}

/// The fixture files: the source name the file is loaded under (its path
/// below `crates/st-data/tests/data/`) and its text.
const FILES: [(&str, &str); 4] = [
    (
        "ship_design/v1/shuttle.ron",
        include_str!("../../../crates/st-data/tests/data/ship_design/v1/shuttle.ron"),
    ),
    (
        "ship_design/v2/shuttle.ron",
        include_str!("../../../crates/st-data/tests/data/ship_design/v2/shuttle.ron"),
    ),
    (
        "ship_design/v3/minimal.ron",
        include_str!("../../../crates/st-data/tests/data/ship_design/v3/minimal.ron"),
    ),
    (
        "ship_design/v3/shuttle.ron",
        include_str!("../../../crates/st-data/tests/data/ship_design/v3/shuttle.ron"),
    ),
];

/// A fixture failed to load, or the data set could not be hashed.
#[derive(Debug)]
pub struct FixtureError(pub String);

impl core::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for FixtureError {}

fn describe(source: &str, diagnostics: &[Diagnostic]) -> FixtureError {
    let lines: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
    FixtureError(format!("fixture {source}: {}", lines.join("; ")))
}

/// What loading one fixture file gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    /// The source name.
    pub source: &'static str,
    /// The version the file is written at.
    pub file_version: u32,
    /// The content hash of the migrated value, as `b3:<hex>`.
    pub content_hash: String,
}

/// Loads every fixture through the migration chain and returns what each
/// gave, in source-name order, and the hash of the whole set.
///
/// # Errors
///
/// A [`FixtureError`] if a file fails to load or the set cannot be hashed.
pub fn load() -> Result<(Vec<Fixture>, DataSetHash), FixtureError> {
    let loader = Loader::new();
    let mut fixtures = Vec::new();
    let mut entries = Vec::new();
    for (source, text) in FILES {
        let loaded = loader
            .load::<ShipDesign>(source, text.as_bytes())
            .map_err(|d| describe(source, &d))?;
        fixtures.push(Fixture {
            source,
            file_version: loaded.file_version,
            content_hash: loaded.hash.to_string(),
        });
        entries.push(DataSetEntry {
            kind: ShipDesign::KIND.to_owned(),
            source: source.to_owned(),
            hash: loaded.hash,
        });
    }
    let set = data_set_hash(&entries)
        .map_err(|e| FixtureError(format!("data-set hash: {}", e.message)))?;
    Ok((fixtures, set))
}

/// The hash of tick 0 and the bytes it folds (the 32 bytes of the data-set
/// hash), as a dump line.
///
/// # Errors
///
/// As [`load`].
pub fn checkpoint() -> Result<(u64, DumpLine), FixtureError> {
    let (_, set) = load()?;
    let mut h = StateHasher::new();
    h.bytes(set.as_bytes());
    let line = DumpLine {
        section: "DSET",
        key: None,
        bytes: set.as_bytes().to_vec(),
    };
    Ok((h.finish(), line))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The golden content hashes of `crates/st-data/tests/golden.rs`.
    const GOLDEN_SHUTTLE_V3: &str =
        "b3:a9513a06f037cb8e9cb2bdba9a75a003aa6c305c7a09e82cea4af2295e507fc2";
    const GOLDEN_MINIMAL_V3: &str =
        "b3:3c04e17b3c33f5bba49b9665910e899992b8b6b85b9f6135374d47fee1ded088";
    const GOLDEN_SHUTTLE_V2: &str =
        "b3:68629c42d34ab81c474f21d69ec03c38f1bd0e1468cdcea870d30340e2399936";

    fn by_source<'a>(fixtures: &'a [Fixture], source: &str) -> &'a Fixture {
        fixtures.iter().find(|f| f.source == source).unwrap()
    }

    #[test]
    fn the_fixtures_match_the_st_data_golden_hashes() {
        let (fixtures, _) = load().unwrap();
        let v3 = by_source(&fixtures, "ship_design/v3/shuttle.ron");
        assert_eq!(
            (v3.file_version, v3.content_hash.as_str()),
            (3, GOLDEN_SHUTTLE_V3)
        );
        let min = by_source(&fixtures, "ship_design/v3/minimal.ron");
        assert_eq!(
            (min.file_version, min.content_hash.as_str()),
            (3, GOLDEN_MINIMAL_V3)
        );
        let v2 = by_source(&fixtures, "ship_design/v2/shuttle.ron");
        assert_eq!(
            (v2.file_version, v2.content_hash.as_str()),
            (2, GOLDEN_SHUTTLE_V2)
        );
    }

    #[test]
    fn an_old_file_hashes_as_the_value_it_migrates_to() {
        let (fixtures, _) = load().unwrap();
        let v1 = by_source(&fixtures, "ship_design/v1/shuttle.ron");
        assert_eq!(v1.file_version, 1);
        assert_eq!(v1.content_hash, GOLDEN_MINIMAL_V3);
    }

    #[test]
    fn the_checkpoint_is_fnv_of_the_data_set_hash() {
        let (hash, line) = checkpoint().unwrap();
        assert_eq!(line.bytes.len(), 32);
        let mut h = StateHasher::new();
        h.bytes(&line.bytes);
        assert_eq!(h.finish(), hash);
        assert_eq!(line.to_string().len(), "DSET - ".len() + 64);
    }
}
