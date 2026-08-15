//! API bindings generated from a module's introspection schema.
//!
//! **This file is a placeholder.** `dagger generate` replaces the whole
//! `src/gen/` directory with bindings produced by `sdk/codegen` from the
//! engine's schema, so nothing written here survives into a real module. It
//! exists so that `sdk/` compiles on its own while the SDK is being developed.
//!
//! The generated form will mirror the Go SDK's `dagger.gen.go`: one type per
//! GraphQL object, each method appending a selection to a lazily-built query
//! that is only sent when a leaf value is requested.

/// The schema version these bindings were generated from.
///
/// Replaced by the real engine version at generation time.
pub const ENGINE_VERSION: &str = "(placeholder — run `dagger generate`)";
