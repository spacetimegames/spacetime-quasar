//! The `ship_design` kind from ADR 0010's examples, in three versions, used
//! by the integration tests. Real schema types live in the crates that own
//! their meaning; this one only exists to exercise the chain.
#![allow(dead_code, reason = "each test file uses a part of this module")]

use serde::{Deserialize, Serialize};
use st_data::{Genesis, MigrationError, Schema, UniqueMap, Validate, Validator};

pub mod v1 {
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

pub mod v2 {
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
pub enum Section {
    Cargo { capacity: u32 },
    Engine(f32),
    Core,
}

/// The current version (3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShipDesign {
    pub name: String,
    pub mass: f32,
    #[serde(default)]
    pub armor: f32,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub tags: Vec<String>,
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

/// A kind with a map, to exercise `UniqueMap`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub prices: UniqueMap<String, u32>,
}

impl Schema for Catalog {
    const KIND: &'static str = "catalog";
    const VERSION: u32 = 1;
    type Previous = Genesis;
    fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
        match previous {}
    }
}

impl Validate for Catalog {
    fn validate(&self, _: &mut Validator) {}
}

/// Loads a `ship_design` file, panicking with the diagnostics on failure.
pub fn load_design(source: &str, text: &str) -> st_data::Loaded<ShipDesign> {
    match st_data::Loader::new().load::<ShipDesign>(source, text.as_bytes()) {
        Ok(loaded) => loaded,
        Err(diagnostics) => {
            let lines: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
            panic!("expected {source} to load:\n{}", lines.join("\n"));
        }
    }
}

/// Loads a `ship_design` file that must fail, returning the diagnostics.
pub fn load_err(text: &str) -> Vec<st_data::Diagnostic> {
    match st_data::Loader::new().load::<ShipDesign>("designs/t.ron", text.as_bytes()) {
        Ok(_) => panic!("expected the file to fail:\n{text}"),
        Err(diagnostics) => diagnostics,
    }
}

/// An envelope around `data` for the current `ship_design` version.
pub fn design(data: &str) -> String {
    format!("(\n    kind: \"ship_design\",\n    version: 3,\n    data: {data},\n)")
}

// Fixtures per version, as in ADR 0010 `tests/data/<kind>/v<N>/<case>.ron`. v1 and v3 are real
// files read with `include_str!` (the Nix source filter keeps `.ron`); the rest are still strings.

/// `ship_design/v1/shuttle.ron`.
pub const SHUTTLE_V1: &str = include_str!("../data/ship_design/v1/shuttle.ron");

/// `ship_design/v2/shuttle.ron`.
pub const SHUTTLE_V2: &str = r#"// Version 2: adds armor.
(
    kind: "ship_design",
    version: 2,
    data: (
        name: "Shuttle",
        mass: 12.5,
        armor: 3.0,
    ),
)
"#;

/// `ship_design/v3/shuttle.ron`.
pub const SHUTTLE_V3: &str = include_str!("../data/ship_design/v3/shuttle.ron");

/// `ship_design/v3/minimal.ron`.
pub const MINIMAL_V3: &str = r#"// The same value that v1/shuttle.ron migrates to: armor 0.0, no sections, no tags.
(
    kind: "ship_design",
    version: 3,
    data: (
        name: "Shuttle",
        mass: 12.5,
        armor: 0.0,
        sections: [],
        tags: [],
    ),
)
"#;
