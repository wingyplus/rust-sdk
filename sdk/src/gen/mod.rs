//! API bindings generated from a module's introspection schema.
//!
//! **This file is a placeholder.** `dagger generate` replaces the whole
//! `src/gen/` directory with bindings produced by `sdk/codegen` from the
//! engine's schema, so nothing written here survives into a real module. It
//! exists so that `sdk/` compiles on its own while the SDK is being developed,
//! and so that a freshly scaffolded module builds before its first `generate`.
//!
//! What replaces it is roughly 20,000 lines: one type per GraphQL object,
//! holding the [`Transport`](crate::engine::Transport) it was reached through
//! and a lazily-built [`Chain`](crate::querybuilder::Chain), plus a zero-sized
//! `XFields` namespace per object for multi-field
//! [`fetch`](crate::querybuilder). To see it, run `sdk/codegen` over an
//! engine's schema — `sdk/README.md` has the recipe.

/// The schema version these bindings were generated from.
///
/// Replaced by the real engine version at generation time.
pub const ENGINE_VERSION: &str = "(placeholder — run `dagger generate`)";
