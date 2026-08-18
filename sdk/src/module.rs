//! What a module declares, and how a call reaches it.
//!
//! The tables here are what `#[dagger::object]` emits. They are `const`, so the
//! names and kinds are `&'static str` — that is core, not std, and costs no
//! allocation. Everything that carries a *value* at runtime is a goish type.
//!
//! [`serve`] is the entry point a module's `main` calls: it answers the
//! engine's pending call, either by describing what this module serves
//! ([`register`]) or by running one of its functions.
//!
//! # Why this talks to the engine through [`querybuilder`]
//!
//! Registration is a client of the API like any other — `typeDef`, `function`
//! and `module` are ordinary schema fields — so it is written against
//! [`Chain`] and [`Leaf`] rather than by pasting GraphQL together. What it
//! cannot use is [`gen`](crate::gen): `src/gen` is a placeholder until a
//! module's first `dagger generate`, and the engine has to *load* a module —
//! which runs the registration below — before generation can enumerate
//! anything. So the few fields needed here are named as string literals, the
//! way the generated bindings name them, and [`ArgValueFields`] is the one
//! hand-written stand-in for a `Fields` namespace codegen would otherwise
//! emit.
//!
//! [`querybuilder`]: crate::querybuilder

use goish::encoding::json;
use goish::{int, nil, os, slice, strconv, string};

use crate::engine::{self, Session, Transport};
use crate::querybuilder::{arg_list, arg_string, Args, Chain, Fields, FromJson, Leaf, ListField};
use crate::{fail, json_string};

/// Where a declaration was written, so an engine-side error about it can point
/// at the source rather than at a name.
///
/// The file is what `proc_macro::Span::file()` reports, which for a module
/// cargo builds from its own root is already the path the engine wants —
/// `src/main.rs`, relative to the module's source directory. Line and column
/// are one-indexed, as the engine's are.
///
/// [`UNKNOWN`](SourceMapDef::UNKNOWN) is the absent case: a declaration the
/// macro had no span for registers with no source map at all, rather than with
/// one pointing at line zero of nothing.
pub struct SourceMapDef {
    /// Path of the source file, relative to the module's source root.
    pub file: &'static str,
    /// One-indexed line.
    pub line: int,
    /// One-indexed column.
    pub column: int,
}

impl SourceMapDef {
    /// No location known: nothing is sent to the engine for this declaration.
    pub const UNKNOWN: SourceMapDef = SourceMapDef {
        file: "",
        line: 0,
        column: 0,
    };

    /// Whether there is a location to send.
    fn is_known(&self) -> bool {
        !self.file.is_empty()
    }
}

/// One argument of an exported function.
pub struct ArgDef {
    /// API name, camelCased from the Rust parameter.
    pub name: &'static str,
    /// The engine's TypeDefKind: `STRING_KIND`, `INTEGER_KIND`, `FLOAT_KIND`,
    /// `BOOLEAN_KIND`, `OBJECT_KIND`.
    pub kind: &'static str,
    /// For `OBJECT_KIND`, the engine's name for the object — `Directory`,
    /// `Workspace`. Empty for every other kind.
    pub object: &'static str,
    /// Whether the caller may leave it out — `Option<T>`, or anything with a default.
    pub optional: bool,
    /// From `#[dagger(doc = "...")]`. Rust has no doc comments on parameters.
    pub doc: &'static str,
    /// From `#[dagger(default = ...)]`, already JSON-encoded. Empty when unset.
    pub default_value: &'static str,
    /// From `#[dagger(default_path = "...")]`. Empty when unset.
    pub default_path: &'static str,
    /// From `#[dagger(ignore = [...])]`.
    pub ignore: &'static [&'static str],
    /// From `#[dagger(deprecated = "...")]`. Empty when unset.
    pub deprecated: &'static str,
    /// Where the parameter was written.
    pub source: SourceMapDef,
}

/// One exported function.
pub struct FunctionDef {
    /// API name, camelCased from the Rust method.
    pub name: &'static str,
    /// The method's `///` doc comment.
    pub doc: &'static str,
    /// The engine's TypeDefKind for the return value.
    pub return_kind: &'static str,
    /// For an `OBJECT_KIND` return, the engine's name for the object —
    /// `Changeset`, `Container`. Empty for every other kind.
    pub return_object: &'static str,
    /// From `#[dagger::check]`: `dagger check` runs this function.
    pub is_check: bool,
    /// From `#[dagger::function(generate)]`: this function is a generator, so
    /// `dagger generate` runs it and applies the `Changeset` it returns.
    pub generator: bool,
    /// From `#[dagger::function(deprecated = "...")]`. Empty when unset.
    ///
    /// The same option an argument carries in `#[dagger(deprecated = "...")]`,
    /// one level up: the engine takes a reason on either.
    pub deprecated: &'static str,
    /// Where the method was written.
    pub source: SourceMapDef,
    pub args: &'static [ArgDef],
}

/// A module's root object, as declared by `#[dagger::object]`.
pub trait Object {
    /// The object name the engine knows this module by.
    const NAME: &'static str;

    /// The `///` doc comment on the annotated `impl` block.
    ///
    /// It describes both the object and the module: a Rust module's root type
    /// is the module, and the crate's own `//!` doc is out of reach of an
    /// attribute macro on an `impl`. Empty when the block carried none.
    const DOC: &'static str;

    /// Where the annotated `impl` block's type name was written.
    const SOURCE: SourceMapDef;

    /// Everything the module exposes.
    fn functions() -> &'static [FunctionDef];

    /// Call one function by API name and return its JSON-encoded result.
    ///
    /// The name stays a goish `string`: the generated dispatch compares it with
    /// `==` against literals rather than `match`, which would need a `&str`.
    fn invoke(name: &string, args: &Arguments) -> Result<string, string>;
}

/// The arguments the engine supplied for this call.
///
/// Holds `inputArgs` as the `(name, value)` pairs [`serve`] selected; the
/// accessors below are what the generated dispatch calls, one per supported
/// type.
pub struct Arguments {
    entries: slice<(string, string)>,
}

impl Arguments {
    /// Wrap the `inputArgs` of `currentFunctionCall`, as `(name, value)` pairs.
    ///
    /// The value is still encoded: the engine types the field as `JSON`, so
    /// what arrives is a JSON document *as text*, which
    /// [`lookup`](Arguments::lookup) decodes once an accessor asks for it.
    pub fn new(entries: slice<(string, string)>) -> Arguments {
        Arguments { entries }
    }

    /// Find an argument's decoded value.
    fn lookup(&self, name: &str) -> Option<json::Value> {
        let mut i: goish::int = 0;
        while i < self.entries.Len() {
            let (found, encoded) = &self.entries[i];
            i += 1;
            if found != name {
                continue;
            }
            // An absent optional arrives as the JSON text `null` rather than
            // being left out of the list, so this decodes to a null value and
            // the `_opt` accessors below turn that into `None`.
            let mut decoded = json::Value::Null;
            let err = json::Unmarshal(&goish::bytes(encoded.clone()), &mut decoded);
            if err != nil {
                return None;
            }
            return Some(decoded);
        }
        None
    }

    fn missing(name: &str) -> string {
        string("missing required argument: ") + name
    }

    fn wrong_type(name: &str, expected: &str) -> string {
        string("argument ") + name + " is not " + expected
    }

    /// A required string argument.
    pub fn string(&self, name: &str) -> Result<string, string> {
        match self.string_opt(name)? {
            Some(value) => Ok(value),
            None => Err(Arguments::missing(name)),
        }
    }

    /// An optional string argument.
    pub fn string_opt(&self, name: &str) -> Result<Option<string>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsString() {
                Some(s) => Ok(Some(s.clone())),
                None => Err(Arguments::wrong_type(name, "a string")),
            },
        }
    }

    /// A required integer argument.
    pub fn int(&self, name: &str) -> Result<goish::int, string> {
        match self.int_opt(name)? {
            Some(value) => Ok(value),
            None => Err(Arguments::missing(name)),
        }
    }

    /// An optional integer argument.
    pub fn int_opt(&self, name: &str) -> Result<Option<goish::int>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsNumber() {
                Some(n) => Ok(Some(n as goish::int)),
                None => Err(Arguments::wrong_type(name, "an integer")),
            },
        }
    }

    /// A required float argument.
    pub fn float(&self, name: &str) -> Result<goish::float64, string> {
        match self.float_opt(name)? {
            Some(value) => Ok(value),
            None => Err(Arguments::missing(name)),
        }
    }

    /// An optional float argument.
    ///
    /// The same JSON number [`int`](Arguments::int) reads, kept as it arrived:
    /// the engine's Float is a double, and goish decodes every JSON number to
    /// one, so this is the accessor that does not narrow.
    pub fn float_opt(&self, name: &str) -> Result<Option<goish::float64>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsNumber() {
                Some(n) => Ok(Some(n)),
                None => Err(Arguments::wrong_type(name, "a number")),
            },
        }
    }

    /// A required object argument, as the engine's ID for it.
    ///
    /// An object arrives as its ID — the same opaque string the engine hands
    /// back for one — so this yields the ID and the generated dispatch wraps it
    /// with [`crate::ObjectId::from_id`].
    pub fn object(&self, name: &str) -> Result<string, string> {
        match self.object_opt(name)? {
            Some(value) => Ok(value),
            None => Err(Arguments::missing(name)),
        }
    }

    /// An optional object argument, as the engine's ID for it.
    pub fn object_opt(&self, name: &str) -> Result<Option<string>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsString() {
                Some(s) => Ok(Some(s.clone())),
                None => Err(Arguments::wrong_type(name, "an object id")),
            },
        }
    }

    /// A required boolean argument.
    pub fn bool(&self, name: &str) -> Result<bool, string> {
        match self.bool_opt(name)? {
            Some(value) => Ok(value),
            None => Err(Arguments::missing(name)),
        }
    }

    /// An optional boolean argument.
    pub fn bool_opt(&self, name: &str) -> Result<Option<bool>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsBool() {
                Some(b) => Ok(Some(b)),
                None => Err(Arguments::wrong_type(name, "a boolean")),
            },
        }
    }
}

/// JSON-encode a string result.
pub fn encode_string(value: &string) -> string {
    crate::json_string(value)
}

/// JSON-encode an integer result.
pub fn encode_int(value: goish::int) -> string {
    goish::fmt::Sprintf!("%d", value)
}

/// JSON-encode a float result.
///
/// `'g'` with a precision of -1 is Go's "shortest text that parses back to this
/// value", which is what keeps a whole number out of exponent form — the same
/// formatting [`ToArg for float64`](crate::querybuilder::ToArg) renders an
/// outgoing argument with, so a value makes the same round trip in both
/// directions.
pub fn encode_float(value: goish::float64) -> string {
    goish::strconv::FormatFloat(value, b'g', -1, 64)
}

/// JSON-encode a boolean result.
pub fn encode_bool(value: bool) -> string {
    if value {
        string("true")
    } else {
        string("false")
    }
}

/// JSON-encode a function that returns nothing.
pub fn encode_void() -> string {
    string("null")
}

/// JSON-encode an object result as the engine's ID for it.
///
/// An object crosses the boundary as its ID in both directions, so returning
/// one is returning an ID. Fallible because obtaining that ID is a round trip
/// for a generated object: what the function returns is a chain nothing has
/// sent yet, and asking for its ID is what runs it.
pub fn encode_object<T: crate::ObjectId>(value: &T) -> Result<string, string> {
    Ok(crate::json_string(&value.to_id()?))
}

/// The message a goish [`error`](goish::error) carries, for a function that
/// returned one.
///
/// A function may fail with a `string` — the message itself, which is what
/// every client method fails with — or with goish's `error`, which is what
/// goish's own APIs hand back. Both end as the one thing the engine prints, and
/// this is where the second becomes the first.
///
/// It is a function rather than `err.Error()` inline in the dispatch because
/// that method panics on a nil error, the way calling a method on a nil
/// receiver does in Go. A module that returned `Err(nil)` did mean to fail, so
/// a vague message serves its caller better than a panic in the dispatch does.
pub fn error_message(err: goish::error) -> string {
    if err == nil {
        return string("the function failed, but its error was nil");
    }
    err.Error()
}

/// The `Void` scalar: a field that returns nothing.
///
/// It arrives as JSON null, so decoding accepts whatever it is handed — the
/// value carries no information, only that the call completed. Codegen emits
/// its own `Void` for the same reason; this one is here because [`serve`] runs
/// before `src/gen` is real.
struct Void;

impl FromJson for Void {
    fn from_json(_value: &json::Value) -> Result<Void, string> {
        Ok(Void)
    }
}

/// The fields of `FunctionCallArgValue`, for the selection [`serve`] makes.
///
/// The one `Fields` namespace written by hand rather than generated; see the
/// module docs for why registration cannot reach for [`crate::gen`].
struct ArgValueFields;

impl Fields for ArgValueFields {
    fn new() -> ArgValueFields {
        ArgValueFields
    }
}

impl ArgValueFields {
    /// The argument's API name.
    fn name(&self) -> Leaf<string> {
        Leaf::new("name")
    }

    /// Its value, as the JSON document the engine encoded it into.
    fn value(&self) -> Leaf<string> {
        Leaf::new("value")
    }
}

/// Serve this module: answer the engine's pending function call.
///
/// `T` is the module's root object, declared with `#[dagger::object]`. The macro
/// emits the [`Object`] impl this walks — the function table for registration,
/// and the dispatch for an invocation.
pub fn serve<T: Object>() -> ! {
    let session = match Session::from_env() {
        Some(s) => s,
        None => fail(string(
            "no engine session in the environment; a module binary is started by the engine, not run directly",
        )),
    };

    // The whole call in one round trip. `inputArgs` is a list of objects, so it
    // is a nested selection rather than a leaf.
    let call = engine::fetch(
        &session,
        &Chain::root().field("currentFunctionCall", string("")),
        &(
            Leaf::<string>::new("parentName"),
            Leaf::<string>::new("name"),
            ListField::<ArgValueFields>::new("inputArgs").select(|a| (a.name(), a.value())),
        ),
    );
    let (parent_name, name, input_args) = match call {
        Ok(call) => call,
        Err(message) => fail(message),
    };

    // An empty parentName is the engine asking what this module serves; anything
    // else is a call against that object.
    let result = if parent_name.Len() == 0 {
        register::<T>(&session)
    } else {
        T::invoke(&name, &Arguments::new(input_args))
    };

    match result.and_then(|value| return_value(&session, &value)) {
        Ok(()) => os::Exit(0),
        Err(message) => fail(message),
    }
}

/// Describe the module to the engine and return the description's ID.
///
/// Build a `TypeDef` for the root object, hang every declared function off it,
/// attach it to a `Module`, and hand back the module's ID.
fn register<T: Object>(transport: &dyn Transport) -> Result<string, string> {
    let mut args = Args::new();
    args.put("name", arg_string(T::NAME));
    // A description is a string like any other, so it is quoted; `kind` in
    // `build_type_def` is the one thing here that is not.
    if !T::DOC.is_empty() {
        args.put("description", arg_string(T::DOC));
    }
    if T::SOURCE.is_known() {
        let source_map = build_source_map(transport, &T::SOURCE)?;
        args.put("sourceMap", arg_string(source_map));
    }
    let mut object = Chain::root()
        .field("typeDef", string(""))
        .field("withObject", args.finish());

    // One `withFunction` per declared function, chained. The response nests
    // exactly as the chain does, and `Chain::decode` walks it back the same
    // way, so repeating a field name costs nothing here.
    for def in T::functions() {
        let function_id = build_function(transport, def)?;
        let mut args = Args::new();
        args.put("function", arg_string(function_id));
        object = object.field("withFunction", args.finish());
    }
    let type_def_id = fetch_id(transport, &object)?;

    let mut args = Args::new();
    args.put("object", arg_string(type_def_id));
    let mut module = Chain::root().field("module", string(""));
    // The root object's doc is the module's description too. Rust has nothing
    // else to offer: the crate's `//!` doc belongs to the file, and an
    // attribute macro on an `impl` block cannot see it.
    if !T::DOC.is_empty() {
        let mut description = Args::new();
        description.put("description", arg_string(T::DOC));
        module = module.field("withDescription", description.finish());
    }
    let module = module.field("withObject", args.finish());
    let module_id = fetch_id(transport, &module)?;

    // Both paths hand back "a JSON document", so the ID is JSON-encoded here to
    // match what dispatch returns from encode_*. returnValue then embeds it as a
    // GraphQL string exactly once.
    Ok(json_string(&module_id))
}

/// Build one `Function` and return its ID.
fn build_function(transport: &dyn Transport, def: &FunctionDef) -> Result<string, string> {
    let return_type = build_type_def(transport, def.return_kind, def.return_object, false)?;

    let mut args = Args::new();
    args.put("name", arg_string(def.name));
    args.put("returnType", arg_string(return_type));
    let mut function = Chain::root().field("function", args.finish());

    // What makes `dagger generate` run this function. It takes no arguments:
    // the engine reads the flag off the function, then enforces the rest of the
    // contract — a `Changeset` return, and no required arguments — when the
    // module loads.
    if def.generator {
        function = function.field("withGenerator", string(""));
    }

    if !def.doc.is_empty() {
        let mut args = Args::new();
        args.put("description", arg_string(def.doc));
        function = function.field("withDescription", args.finish());
    }

    // The engine takes a deprecation on a function as readily as on one of its
    // arguments; the argument's goes in `withArg` below, the function's is a
    // field of its own.
    if !def.deprecated.is_empty() {
        let mut args = Args::new();
        args.put("reason", arg_string(def.deprecated));
        function = function.field("withDeprecated", args.finish());
    }

    if def.source.is_known() {
        let source_map = build_source_map(transport, &def.source)?;
        let mut args = Args::new();
        args.put("sourceMap", arg_string(source_map));
        function = function.field("withSourceMap", args.finish());
    }

    // Flags the function for `dagger check`. It takes no arguments — the whole
    // effect is the flag.
    if def.is_check {
        function = function.field("withCheck", string(""));
    }

    for arg in def.args {
        let arg_type = build_type_def(transport, arg.kind, arg.object, arg.optional)?;
        let mut args = Args::new();
        args.put("name", arg_string(arg.name));
        args.put("typeDef", arg_string(arg_type));
        if !arg.doc.is_empty() {
            args.put("description", arg_string(arg.doc));
        }
        if !arg.default_value.is_empty() {
            // defaultValue is a JSON scalar, so the already-encoded JSON is
            // embedded as a GraphQL string.
            args.put("defaultValue", arg_string(arg.default_value));
        }
        if !arg.default_path.is_empty() {
            args.put("defaultPath", arg_string(arg.default_path));
        }
        if !arg.ignore.is_empty() {
            args.put("ignore", arg_list(arg.ignore));
        }
        if !arg.deprecated.is_empty() {
            args.put("deprecated", arg_string(arg.deprecated));
        }
        if arg.source.is_known() {
            let source_map = build_source_map(transport, &arg.source)?;
            args.put("sourceMap", arg_string(source_map));
        }
        function = function.field("withArg", args.finish());
    }

    fetch_id(transport, &function)
}

/// Build one `SourceMap` and return its ID.
///
/// `line` and `column` are Int arguments, so they are spliced as bare decimal
/// literals rather than through [`arg_string`] — the same distinction
/// [`build_type_def`] makes for `kind`. They come from a `proc_macro::Span`,
/// never from user text.
fn build_source_map(transport: &dyn Transport, def: &SourceMapDef) -> Result<string, string> {
    let mut args = Args::new();
    args.put("filename", arg_string(def.file));
    args.put("line", strconv::Itoa(def.line));
    args.put("column", strconv::Itoa(def.column));

    fetch_id(transport, &Chain::root().field("sourceMap", args.finish()))
}

/// Build a `TypeDef` of one kind and return its ID.
///
/// `object` names the engine object for `OBJECT_KIND` and is empty otherwise;
/// an object is described by name rather than by kind, since the kind alone
/// would not say which object it is.
fn build_type_def(
    transport: &dyn Transport,
    kind: &'static str,
    object: &'static str,
    optional: bool,
) -> Result<string, string> {
    let mut args = Args::new();
    let mut type_def = Chain::root().field("typeDef", string(""));

    if object.is_empty() {
        // `kind` is a GraphQL enum literal, so it is spliced unquoted rather
        // than through `arg_string`. It only ever comes from the macro's fixed
        // set, never from user text. An object's name is a string argument, so
        // it goes through the usual quoting.
        args.put("kind", string(kind));
        type_def = type_def.field("withKind", args.finish());
    } else {
        args.put("name", arg_string(object));
        type_def = type_def.field("withObject", args.finish());
    }

    if optional {
        let mut args = Args::new();
        args.put("optional", string("true"));
        type_def = type_def.field("withOptional", args.finish());
    }

    fetch_id(transport, &type_def)
}

/// Send `chain` and read the `id` of the object it builds.
///
/// Every builder above ends the same way: the chain is lazy until an ID is
/// asked for, and asking is what runs it.
fn fetch_id(transport: &dyn Transport, chain: &Chain) -> Result<string, string> {
    engine::fetch(transport, chain, &Leaf::<string>::new("id"))
}

/// Hand a JSON-encoded result back to the engine.
fn return_value(transport: &dyn Transport, value: &string) -> Result<(), string> {
    let mut args = Args::new();
    args.put("value", arg_string(value.clone()));
    engine::fetch(
        transport,
        &Chain::root().field("currentFunctionCall", string("")),
        &Leaf::<Void>::with_args("returnValue", args.finish()),
    )?;
    Ok(())
}
