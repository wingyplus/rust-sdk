//! Generating the Dagger Rust SDK's API bindings from an introspection schema.
//!
//! The engine hands each module its own schema; this turns that schema into the
//! `dagger::gen` module the SDK crate exposes. What comes out is written
//! against `dagger::querybuilder` and `dagger::engine`, both hand-written and
//! both outside `src/gen/` — generation replaces that directory wholesale, so
//! nothing generated may be depended on by anything that is not.
//!
//! Like everything else in this repository, this is `no_std` and built on
//! goish. It reaches for `os` where a hosted binary would reach for `std::fs`,
//! and its one dependency is the same goish rev the SDK crate and every
//! generated module pin. Nothing here comes from crates.io, so `dagger
//! generate` needs no registry access beyond the toolchain image.
//!
//! The three steps have a module each: [`schema`] decodes introspection JSON,
//! [`names`] turns schema names into Rust ones, and [`render`] writes the code.

#![no_std]

pub mod names;
pub mod render;
pub mod schema;

use goish::{error, slice, string};

/// The single file a generation run produces, relative to the output
/// directory.
pub const OUTPUT_FILE: &str = "mod.rs";

/// Turn an introspection schema into the bindings module's source.
pub fn generate(introspection: &slice<goish::byte>) -> (string, error) {
    let (parsed, err) = schema::parse(introspection);
    if err != goish::nil {
        return (string(""), err);
    }
    (render::render(&parsed), goish::nil.into())
}
