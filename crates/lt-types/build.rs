// Build scripts report failure by panicking, which is idiomatic and cannot
// propagate a Result; the crate-wide panic-safety lints do not apply here.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let schema_path = Path::new(&manifest_dir).join("../../build/linear-schema-definition.graphql");

    println!("cargo:rerun-if-changed={}", schema_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Make the snapshot available to `#[cynic::schema("linear")]` and the
    // `QueryFragment` derives, which read it from `$OUT_DIR/cynic-schemas`.
    cynic_codegen::register_schema("linear")
        .from_sdl_file(&schema_path)
        .expect("registering Linear schema with cynic")
        .as_default()
        .expect("setting cynic default schema");
}
