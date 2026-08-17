//! Dagger client library for Rust modules.
//!
//! This crate is `no_std` and built on [goish], which supplies the Go standard
//! library — `net/http`, `encoding/json`, `encoding/base64` — that the engine
//! session protocol needs, without libc or a garbage collector. A module built
//! against it links to a single static binary.
//!
//! It is vendored into each module as `dagger/` by `dagger generate`,
//! together with [`gen`], the API bindings generated from that module's own
//! schema. Modules never regenerate it themselves.
//!
//! [goish]: https://github.com/cogentica-ai/goish
//!
//! # The module protocol
//!
//! The engine starts a module binary once per call, with the session's port and
//! token in the environment, and expects it to talk GraphQL back over loopback:
//!
//! 1. Ask for `currentFunctionCall { parentName }`.
//! 2. When `parentName` is empty this is the *registration* call: build a
//!    `Module` describing what the module serves and return its ID.
//! 3. Otherwise it is an invocation: dispatch to the named function.
//! 4. Either way, hand the result back through `returnValue`.
//!
//! # Status
//!
//! The protocol above runs end to end: [`serve`] registers what the module
//! serves — its functions and their arguments, which of them are checks, and
//! which are generators — and dispatches an incoming call.
//!
//! The other direction now exists too. [`gen`] is generated from the engine's
//! own schema and is a full typed client: `dag().container()
//! .from("alpine").with_exec(&["echo", "hi"]).stdout()?` reaches a live engine.
//! `dag()` takes no argument because it opens the session the engine put in
//! this process's environment — see [`default_transport`] — so a module's
//! function can reach the API without being handed anything.
//!
//! What is not yet wired is the seam between the two: not *reaching* the API,
//! but passing its objects across the call boundary. A generated object holds
//! an `Arc<dyn Transport>`, and the dispatch
//! [`Object::invoke`](module::Object::invoke) emits carries only [`Arguments`],
//! while [`ObjectId`] — how an object crosses that boundary — still wraps a
//! bare ID with no transport behind it. So function signatures remain limited
//! to scalars and the two types in [`objects`]: a function can call `dag()`,
//! but it cannot yet take a `Container` or return one. See the repository
//! README.

#![no_std]

// goish is built on `alloc` and so is everything here: `string` and `slice`
// allocate, and the generated bindings hold their engine connection in an
// `alloc::sync::Arc<dyn Transport>`.
extern crate alloc;

/// API bindings generated from the module's schema.
///
/// The checked-in contents are a placeholder; `dagger generate` replaces this
/// whole module with bindings derived from the engine's introspection schema.
pub mod gen;

/// Talking to the Dagger Engine: the session, and sending a selection over it.
pub mod engine;

/// What this module declares, and how an incoming call reaches it.
pub mod module;

/// Multi-field selection: the query language the other two exchange.
///
/// Belongs to neither side — [`engine`] carries what it builds, [`gen`] is
/// written against it, and it depends on neither. Unlike [`gen`] it is
/// hand-written and stays put; see the module docs.
pub mod querybuilder;

mod objects;

pub use engine::{
    default_transport, fetch, field, field_string, Session, Transport, SESSION_PORT_ENV,
    SESSION_TOKEN_ENV,
};
pub use module::{
    encode_bool, encode_int, encode_object, encode_string, encode_void, serve, ArgDef, Arguments,
    FunctionDef, Object,
};
pub use objects::{Changeset, ObjectId, Workspace};
pub use querybuilder::{
    arg_list, arg_string, Args, Chain, Field, Fields, FromJson, Leaf, ListField, OptField, Sel, Sub,
    SubList, SubOpt, ToArg,
};

/// Declare a module's root object and the functions it serves.
pub use dagger_macros::{check, function, object};

use goish::encoding::json;
use goish::{bytes, nil, os, string};

/// Render a Rust-side string as a quoted, escaped literal.
///
/// Used for both the GraphQL arguments and the JSON request body, which share
/// escaping rules. Going through `json::Marshal` rather than hand-rolling the
/// escapes keeps module IDs — opaque, engine-chosen text — safe to embed. It is
/// public for the same reason [`field`] is: a query written by hand has to
/// quote its arguments, and this is what the rest of the crate quotes with.
pub fn json_string(value: &string) -> string {
    let (encoded, err) = json::Marshal(&json::Value::String(value.clone()));
    if err != nil {
        fail(string("encoding a query argument: ") + err.Error());
    }
    string(encoded)
}

/// Write a message to stderr and exit non-zero.
///
/// The engine surfaces a module's stderr, so this is what a user sees when a
/// module fails to serve.
pub fn fail(message: string) -> ! {
    let stderr = os::Stderr();
    let _ = stderr.Write(bytes(string("dagger: ") + message + "\n"));
    os::Exit(2)
}
