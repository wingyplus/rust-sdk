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
//! The two meet at [`ObjectId`], which every generated object with a loader
//! implements: a function may take a `Directory` and return a `Container`, and
//! both cross the boundary as engine IDs, rebuilt on the way in and resolved on
//! the way out. So the signature a module declares is not limited to scalars
//! any more.
//!
//! A function may fail, too: returning `Result<T, string>` — or
//! `Result<T, error>`, goish's own error type — declares to the engine exactly
//! what returning `T` declares, and an `Err` reaches the caller as the message
//! [`fail`] would have written. That is what lets `?` carry a client failure out
//! of a function, every generated method being fallible.
//!
//! What is still missing is the way back *in*: an argument of a client method
//! that the schema types as `DirectoryID` is a `string` here, so handing an
//! object to one goes through [`ObjectId::to_id`] rather than passing the
//! object itself. Lists, in either direction, are not supported either. See the
//! repository README.

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
    encode_bool, encode_int, encode_object, encode_string, encode_void, error_message, serve,
    ArgDef, Arguments, FunctionDef, Object,
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
