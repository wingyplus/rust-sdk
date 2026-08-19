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
//! 1. Ask for `currentFunctionCall { parentName name parent inputArgs }`.
//! 2. When `parentName` is empty this is the *registration* call: build a
//!    `Module` describing what the module serves and return its ID.
//! 3. When the function `name` is empty this is the *constructor*: build the
//!    object from the arguments and return its state.
//! 4. Otherwise it is an invocation: decode `parent` into the receiver and
//!    dispatch to the named function.
//! 5. Either way, hand the result back through `returnValue`.
//!
//! # Status
//!
//! The protocol above runs end to end: [`serve`] registers what the module
//! serves — its fields, its functions and their arguments, which of them are
//! checks, and which are generators — and dispatches an incoming call.
//!
//! The root object carries state. `#[dagger::object]` goes on the `struct` as
//! well as on the `impl`: on the `struct` it reads the `pub` fields and emits
//! the `ObjectState` half, which encodes them into the document the engine
//! keeps and decodes them back into the receiver on the next call. A
//! `#[dagger::constructor]` configures the object once — its arguments are the
//! module's own flags — and a function returning `Self` hands back a
//! reconfigured one, so `dagger call --image=alpine with-tag --tag=v1 publish`
//! is three calls against one object rather than three copies of the same
//! arguments.
//!
//! The other direction now exists too. [`gen`] is generated from the engine's
//! own schema and is a full typed client: `dag().container()
//! .from("alpine").with_exec(&["echo", "hi"]).stdout()?` reaches a live engine.
//! `dag()` takes no argument because it opens the session the engine put in
//! this process's environment — see [`engine::default_transport`] — so a
//! module's function can reach the API without being handed anything.
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
//! A signature may be list-typed too: a `slice<T>` argument arrives as a JSON
//! array — of scalars, or of the IDs a `slice<Directory>` is rebuilt from — and
//! a returned one is encoded the same way. One level deep, and an `Option`
//! wraps the list rather than its elements.
//!
//! What is still missing is the way back *in*: an argument of a client method
//! that the schema types as `DirectoryID` is a `string` here, so handing an
//! object to one goes through [`ObjectId::to_id`] rather than passing the
//! object itself. See the repository README.

#![no_std]

// goish is built on `alloc` and so is everything here: `string` and `slice`
// allocate, and the generated bindings hold their engine connection in an
// `alloc::sync::Arc<dyn Transport>`.
extern crate alloc;

// The three public modules carry their own `//!` docs. An outer `///` here as
// well would be merged in front of them, and rustdoc resolves a merged doc's
// links in the scope of its *first* fragment — the crate root — so every link
// an inner doc makes to its own module's items would break.
pub mod engine;
pub mod gen;
pub mod querybuilder;

mod module;
mod objects;

pub use module::serve;
pub use objects::{Changeset, ObjectId, Workspace};

/// Declare a module's root object, its state, its enums, and the functions it
/// serves.
pub use dagger_macros::{check, constructor, enum_type, function, object};

use goish::{bytes, os, string};

/// Write a message to stderr and exit non-zero.
///
/// The engine surfaces a module's stderr, so this is what a user sees when a
/// module fails to serve. It is also how a module gives up inside a function:
/// every generated client method is fallible, and
/// `unwrap_or_else(|m| dagger::fail(m))` is the spelling for a failure the
/// function has no better answer to than stopping.
pub fn fail(message: string) -> ! {
    let stderr = os::Stderr();
    let _ = stderr.Write(bytes(string("dagger: ") + message + "\n"));
    os::Exit(2)
}

/// What the attribute macros expand into. Not a public API.
///
/// `#[dagger::object]` and its companions emit code into the *user's* crate, so
/// everything that code names has to be reachable as `dagger::…` even though a
/// module never writes any of it by hand. Keeping it behind one hidden module
/// says which half of the crate that is: what a module author writes is the
/// short list above, and anything here can change with the macro that emits it.
#[doc(hidden)]
pub mod __private {
    pub use crate::module::{
        encode_bool, encode_bool_list, encode_enum, encode_float, encode_float_list, encode_int,
        encode_int_list, encode_null, encode_object, encode_object_list, encode_state,
        encode_string, encode_string_list, encode_void, error_message, from_ids, ArgDef, Arguments,
        EnumDef, EnumMemberDef, EnumType, FieldDef, FunctionDef, Object, ObjectState, SourceMapDef,
        State, StateWriter,
    };
}
