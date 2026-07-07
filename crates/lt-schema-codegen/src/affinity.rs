//! Maps each fragment field's Rust base type ([`crate::selection_model`]) to
//! its SQLite column affinity.
//!
//! Keyed on the Rust type, not the GraphQL type: this is what lets `Priority`
//! (GraphQL `Float`, backed by `u8` -- `lt-types/src/scalars.rs`) resolve to
//! `Integer` rather than `Real`.
//!
//! Fails closed (build-time panic, per this crate's build-script convention)
//! on any type name the table does not list.

use std::fmt;

/// A SQLite column affinity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Text,
    Real,
    Integer,
}

impl fmt::Display for Affinity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Affinity::Text => "TEXT",
            Affinity::Real => "REAL",
            Affinity::Integer => "INTEGER",
        })
    }
}

/// The affinity for a fragment field's Rust base type name (the last path
/// segment, e.g. `Id` for `cynic::Id`).
///
/// Panics naming the offending type if `rust_base_type` is not in the table:
/// an unmapped scalar must not silently pick a default affinity.
pub fn affinity(rust_base_type: &str) -> Affinity {
    match rust_base_type {
        "f64" => Affinity::Real,
        // The custom-scalar hop: `Priority` is `u8`-backed, not its GraphQL
        // `Float`.
        "i64" | "Priority" => Affinity::Integer,
        // `String`; `Id` is `cynic::Id`; `DateTime` is RFC3339 text, per
        // `scalars.rs::to_rfc3339_millis`.
        "String" | "Id" | "DateTime" => Affinity::Text,
        other => panic!("no SQLite affinity mapped for Rust base type `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Affinity, affinity};

    #[test]
    fn affinity_maps_every_named_scalar() {
        let cases = [
            ("String", Affinity::Text, "TEXT"),
            ("f64", Affinity::Real, "REAL"),
            ("i64", Affinity::Integer, "INTEGER"),
            ("Id", Affinity::Text, "TEXT"),
            ("DateTime", Affinity::Text, "TEXT"),
            ("Priority", Affinity::Integer, "INTEGER"),
        ];
        for (rust_base_type, expected, rendered) in cases {
            let got = affinity(rust_base_type);
            assert_eq!(got, expected, "{rust_base_type}");
            assert_eq!(got.to_string(), rendered, "{rust_base_type}");
        }
    }

    #[test]
    #[should_panic(expected = "no SQLite affinity mapped for Rust base type `Widget`")]
    fn affinity_panics_on_unmapped_type() {
        affinity("Widget");
    }
}
