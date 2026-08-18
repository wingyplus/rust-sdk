//! What a module declares, and how a call reaches it.
//!
//! The tables here are what `#[dagger::object]` emits. They are `const`, so the
//! names and kinds are `&'static str` — that is core, not std, and costs no
//! allocation. Everything that carries a *value* at runtime is a goish type.
//!
//! [`serve`] is the entry point a module's `main` calls: it answers the
//! engine's pending call, by describing what this module serves
//! ([`register`]), by building the object its constructor configures, or by
//! running one of its functions.
//!
//! # How the object survives between calls
//!
//! A module object has no ID — the engine holds no value to mint one for — so
//! it crosses the boundary as *itself*: the JSON document its fields encode to.
//! The engine keeps that document and hands it back as
//! `currentFunctionCall.parent`, which [`State`] decodes and
//! [`ObjectState::from_state`] turns into the receiver a function is dispatched
//! on. A function that returns the object writes the document again, so a chain
//! of calls is one object passed along rather than one rebuilt from nothing each
//! time.
//!
//! That is why [`Object`] takes its receiver by value and why [`Object::invoke`]
//! is not an associated function any more: there *is* a receiver now, and it
//! belongs to the one call it was decoded for.
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
use goish::{append, int, make, nil, os, slice, strconv, string};

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
    /// `BOOLEAN_KIND`, `OBJECT_KIND`, `ENUM_KIND`.
    pub kind: &'static str,
    /// For `OBJECT_KIND` and `ENUM_KIND`, the engine's name for the type —
    /// `Directory`, `Workspace`, or an enum the module declares. Empty for
    /// every other kind, since the kind alone says what those are.
    pub type_name: &'static str,
    /// Whether the argument is a *list* of `kind` — `slice<T>` or `Vec<T>` in
    /// the signature. The kind and type name above describe the element, so a
    /// list is one bool rather than a second copy of both.
    pub list: bool,
    /// Whether the caller may leave it out — `Option<T>`, or anything with a default.
    ///
    /// Of the list, when [`list`](ArgDef::list) is set: the engine's
    /// `withOptional` applies to whichever `TypeDef` it is called on, and
    /// `Option<slice<T>>` makes the list optional, not its elements.
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
    /// For an `OBJECT_KIND` or `ENUM_KIND` return, the engine's name for the
    /// type — `Changeset`, `Container`, or an enum the module declares. Empty
    /// for every other kind.
    pub return_type_name: &'static str,
    /// Whether the function returns a *list* of `return_kind`.
    pub return_list: bool,
    /// Whether the function may return no value at all — `Option<T>`.
    ///
    /// The engine takes this on the return `TypeDef` the same way it takes it on
    /// an argument's, so what declares it is one `withOptional`. What the
    /// dispatch then has to agree on is the encoding: a `None` is
    /// [`encode_null`], and a return declared optional that encoded something
    /// else would fail the call rather than the build.
    ///
    /// Independent of `return_list`: the two compose as `Option<slice<T>>`,
    /// where [`build_type_def`] wraps the element in a list and marks *that*
    /// optional — a nullable list, not a list of nullable elements.
    pub return_optional: bool,
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

/// One field of an object's state.
///
/// Read off a `pub` field of the annotated `struct`, and registered with
/// `withField` so the engine can both hand the value back to a later call and
/// answer for it directly — `dagger call --image=alpine image` never reaches
/// the module. The shape mirrors [`ArgDef`], minus everything about being
/// *supplied*: a field has no default, no context path and no ignore list,
/// because it is state rather than an input.
pub struct FieldDef {
    /// API name, camelCased from the Rust field.
    pub name: &'static str,
    /// The engine's TypeDefKind for the field's type.
    pub kind: &'static str,
    /// For `OBJECT_KIND` and `ENUM_KIND`, the engine's name for the type. Empty
    /// for every other kind.
    pub type_name: &'static str,
    /// Whether the field is a *list* of `kind`, the way [`ArgDef::list`] is.
    pub list: bool,
    /// Whether the field may be absent — `Option<T>`.
    pub optional: bool,
    /// The field's `///` doc comment.
    pub doc: &'static str,
    /// From `#[dagger(deprecated = "...")]`. Empty when unset.
    pub deprecated: &'static str,
    /// Where the field was written.
    pub source: SourceMapDef,
}

/// One member of an enum a module declares.
pub struct EnumMemberDef {
    /// The member name, spelled as the Rust variant is.
    ///
    /// This is the name the engine records as the member's *original* name, and
    /// it is the text that crosses the call boundary in both directions — the
    /// engine hands a module its own spelling and expects it back. The name a
    /// caller writes is the engine's own SCREAMING_SNAKE_CASE of it, which the
    /// engine derives and this side never has to.
    pub name: &'static str,
    /// The variant's `///` doc comment.
    pub doc: &'static str,
}

/// An enum a module declares, as `#[dagger::enum_type]` read it.
pub struct EnumDef {
    /// The engine's name for the enum, from the Rust type name.
    pub name: &'static str,
    /// The type's `///` doc comment.
    pub doc: &'static str,
    pub members: &'static [EnumMemberDef],
}

/// An enum type a module declares, as `#[dagger::enum_type]` emits it.
///
/// An enum crosses the call boundary as one member's name: [`member`] writes
/// that name, and [`from_member`] reads it back into a variant. Both use the
/// Rust spelling — see [`EnumMemberDef::name`].
///
/// [`member`]: EnumType::member
/// [`from_member`]: EnumType::from_member
pub trait EnumType: Sized {
    /// What `register` tells the engine about this enum.
    const DEF: EnumDef;

    /// The member name of this value.
    fn member(&self) -> &'static str;

    /// The value a member name stands for.
    ///
    /// Fails when the name is not one of this enum's members. The engine checks
    /// an incoming argument against the members the module declared, so that is
    /// a module and an engine disagreeing rather than a caller's mistake — but
    /// the dispatch has to be able to say so either way.
    fn from_member(name: &string) -> Result<Self, string>;
}

/// The data an object carries from one call to the next, as declared by
/// `#[dagger::object]` on the `struct`.
///
/// A module object crosses the boundary as *itself* rather than as an ID: the
/// engine keeps the JSON document the module last handed back and returns it as
/// `parent` on the next call. So the receiver is built rather than named, and
/// the two halves of that round trip are [`from_state`](ObjectState::from_state)
/// and [`to_state`](ObjectState::to_state).
///
/// # Why this is a second attribute rather than part of [`Object`]
///
/// A Rust type's data and its behaviour are two items — a `struct` and an
/// `impl` — and an attribute macro sees only the one it is written on. The
/// `impl` block has no way to learn the field list, so `#[dagger::object]` goes
/// on both: on the `struct` it emits this trait, on the `impl` it emits
/// [`Object`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` declares no state",
    label = "no `#[dagger::object]` on this type's `struct`",
    note = "an object is a `struct` and an `impl`, and `#[dagger::object]` goes on both: the `impl` declares the functions, the `struct` declares the fields that survive from one call to the next"
)]
pub trait ObjectState: Sized {
    /// The object's `pub` fields, in declaration order.
    fn fields() -> &'static [FieldDef];

    /// Rebuild the receiver from the state the engine sent.
    ///
    /// Fails when a declared field is absent or holds the wrong type, naming
    /// it. A type with no fields ignores the state and cannot fail.
    fn from_state(state: &State) -> Result<Self, string>;

    /// Encode the state back out, as the JSON document the engine stores.
    ///
    /// Fallible because a field may itself be an engine object, and reading an
    /// object's ID is a round trip.
    fn to_state(&self) -> Result<string, string>;
}

/// A module's root object, as declared by `#[dagger::object]` on its `impl`
/// block.
pub trait Object: ObjectState {
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

    /// The enums the module declares, from `#[dagger::object(enums(...))]`.
    ///
    /// Defaulted because most modules declare none, and because an `Object`
    /// impl written before enums existed — by hand, or by an older macro
    /// vendored alongside — still says everything it meant to.
    fn enums() -> &'static [EnumDef] {
        &[]
    }

    /// The `#[dagger::constructor]`, if the object declared one.
    ///
    /// It is registered under the empty name, which is how the engine spells a
    /// constructor: its arguments become the module's own flags, so
    /// `dagger call --image=alpine build` configures the object once and then
    /// calls a function on it.
    ///
    /// Defaulted for the same reason [`enums`](Object::enums) is: most modules
    /// declare none.
    fn constructor() -> Option<&'static FunctionDef> {
        None
    }

    /// Run the constructor and encode the object it built.
    ///
    /// The default is what an object with no constructor does: the engine asked
    /// for a value and there is nothing to configure, so the state is whatever
    /// an empty document decodes to — which for a type with no fields is the
    /// type itself, and for one with fields is an error naming the first field
    /// nothing supplied.
    fn construct(_args: &Arguments) -> Result<string, string> {
        Self::from_state(&State::empty())?.to_state()
    }

    /// Call one function by API name and return its JSON-encoded result.
    ///
    /// The receiver is the object the engine's `parent` decoded to, so a
    /// function reads the state a constructor or an earlier call put there. It
    /// is taken by value: a builder that consumes `self` and returns a
    /// reconfigured object is as ordinary a signature as one that borrows it,
    /// and the receiver belongs to this one call either way.
    ///
    /// The name stays a goish `string`: the generated dispatch compares it with
    /// `==` against literals rather than `match`, which would need a `&str`.
    fn invoke(self, name: &string, args: &Arguments) -> Result<string, string>;
}

/// The arguments the engine supplied for this call.
///
/// Holds `inputArgs` as the `(name, value)` pairs [`serve`] selected; the
/// accessors below are what the generated dispatch calls, one per supported
/// type.
pub struct Arguments {
    entries: slice<(string, string)>,
    /// The noun a failure names.
    ///
    /// The same accessors serve an object's fields — see [`State`] — and the
    /// only thing that differs there is what to call the thing that was missing
    /// or of the wrong type. Carrying the word is what lets the decoding rules
    /// be written once.
    what: &'static str,
}

impl Arguments {
    /// Wrap the `inputArgs` of `currentFunctionCall`, as `(name, value)` pairs.
    ///
    /// The value is still encoded: the engine types the field as `JSON`, so
    /// what arrives is a JSON document *as text*, which
    /// [`lookup`](Arguments::lookup) decodes once an accessor asks for it.
    pub fn new(entries: slice<(string, string)>) -> Arguments {
        Arguments {
            entries,
            what: "argument",
        }
    }

    /// The same, over an object's fields rather than a call's arguments.
    ///
    /// Everything below is identical for the two; what this changes is the word
    /// a failure uses, so a module told about a missing *field* is not sent
    /// looking for an argument nobody asked it for. [`State::decode`] is what
    /// builds one.
    fn fields(entries: slice<(string, string)>) -> Arguments {
        Arguments {
            entries,
            what: "field",
        }
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

    /// The three messages, each naming what this carrier calls its values.
    ///
    /// They start from an empty `string` rather than from `self.what`, because
    /// goish's `string(…)` conversion takes a `&'static str` and the borrow
    /// here is not one as far as the signature is concerned.
    fn missing(&self, name: &str) -> string {
        string("missing required ") + self.what + ": " + name
    }

    fn wrong_type(&self, name: &str, expected: &str) -> string {
        string("") + self.what + " " + name + " is not " + expected
    }

    /// The same, for one element of a list.
    ///
    /// The index is the whole point: an argument's name says which argument
    /// went wrong, and for a list that leaves the caller comparing their own
    /// values against a message that fits any of them. `querybuilder` names the
    /// index on the way in for the same reason.
    fn wrong_element(&self, name: &str, index: int, expected: &str) -> string {
        string("") + self.what + " " + name + "[" + strconv::Itoa(index) + "] is not " + expected
    }

    /// An optional list argument, decoded element by element.
    ///
    /// The shape every list accessor below is written in terms of: `element`
    /// decodes one JSON value, and yields `None` for a value of the wrong type,
    /// which becomes the message naming its index. A list that is absent, and
    /// one that arrived as JSON null, are both `None`; an *empty* list is a
    /// list, so it is `Some` of nothing rather than absent.
    fn list_opt<T>(
        &self,
        name: &str,
        expected: &str,
        element: impl Fn(&json::Value) -> Option<T>,
    ) -> Result<Option<slice<T>>, string> {
        let value = match self.lookup(name) {
            None => return Ok(None),
            Some(value) if value.IsNull() => return Ok(None),
            Some(value) => value,
        };
        let items = match value.AsArray() {
            Some(items) => items.clone(),
            None => return Err(self.wrong_type(name, "a list")),
        };

        let mut out = make!([]T, 0, items.Len());
        let mut i: int = 0;
        while i < items.Len() {
            match element(&items[i]) {
                Some(decoded) => out = append!(out, decoded),
                None => return Err(self.wrong_element(name, i, expected)),
            }
            i += 1;
        }
        Ok(Some(out))
    }

    /// A required string argument.
    pub fn string(&self, name: &str) -> Result<string, string> {
        match self.string_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional string argument.
    pub fn string_opt(&self, name: &str) -> Result<Option<string>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsString() {
                Some(s) => Ok(Some(s.clone())),
                None => Err(self.wrong_type(name, "a string")),
            },
        }
    }

    /// A required integer argument.
    pub fn int(&self, name: &str) -> Result<goish::int, string> {
        match self.int_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional integer argument.
    pub fn int_opt(&self, name: &str) -> Result<Option<goish::int>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsNumber() {
                Some(n) => Ok(Some(n as goish::int)),
                None => Err(self.wrong_type(name, "an integer")),
            },
        }
    }

    /// A required float argument.
    pub fn float(&self, name: &str) -> Result<goish::float64, string> {
        match self.float_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
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
                None => Err(self.wrong_type(name, "a number")),
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
            None => Err(self.missing(name)),
        }
    }

    /// An optional object argument, as the engine's ID for it.
    pub fn object_opt(&self, name: &str) -> Result<Option<string>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsString() {
                Some(s) => Ok(Some(s.clone())),
                None => Err(self.wrong_type(name, "an object id")),
            },
        }
    }

    /// A required enum argument, as the name of the member the caller chose.
    ///
    /// An enum arrives as a member name rather than as an ID: the engine
    /// resolves whatever the caller wrote — the schema's SCREAMING_SNAKE_CASE
    /// spelling — back to the name the module registered the member under, so
    /// what lands here is the Rust variant's own name and the generated
    /// dispatch turns it into the variant with [`EnumType::from_member`].
    pub fn enum_member(&self, name: &str) -> Result<string, string> {
        match self.enum_member_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional enum argument, as the name of the member the caller chose.
    pub fn enum_member_opt(&self, name: &str) -> Result<Option<string>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsString() {
                Some(s) => Ok(Some(s.clone())),
                None => Err(self.wrong_type(name, "an enum member")),
            },
        }
    }

    /// A required boolean argument.
    pub fn bool(&self, name: &str) -> Result<bool, string> {
        match self.bool_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional boolean argument.
    pub fn bool_opt(&self, name: &str) -> Result<Option<bool>, string> {
        match self.lookup(name) {
            None => Ok(None),
            Some(value) if value.IsNull() => Ok(None),
            Some(value) => match value.AsBool() {
                Some(b) => Ok(Some(b)),
                None => Err(self.wrong_type(name, "a boolean")),
            },
        }
    }

    /// A required list of strings.
    pub fn string_list(&self, name: &str) -> Result<slice<string>, string> {
        match self.string_list_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional list of strings.
    pub fn string_list_opt(&self, name: &str) -> Result<Option<slice<string>>, string> {
        self.list_opt(name, "a string", |value| value.AsString().cloned())
    }

    /// A required list of integers.
    pub fn int_list(&self, name: &str) -> Result<slice<int>, string> {
        match self.int_list_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional list of integers.
    pub fn int_list_opt(&self, name: &str) -> Result<Option<slice<int>>, string> {
        self.list_opt(name, "an integer", |value| value.AsNumber().map(|n| n as int))
    }

    /// A required list of floats.
    pub fn float_list(&self, name: &str) -> Result<slice<goish::float64>, string> {
        match self.float_list_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional list of floats.
    ///
    /// The same JSON numbers [`int_list`](Arguments::int_list) reads, kept as
    /// they arrived: this is the list accessor that does not narrow, the way
    /// [`float_opt`](Arguments::float_opt) is the scalar one.
    pub fn float_list_opt(&self, name: &str) -> Result<Option<slice<goish::float64>>, string> {
        self.list_opt(name, "a number", |value| value.AsNumber())
    }

    /// A required list of booleans.
    pub fn bool_list(&self, name: &str) -> Result<slice<bool>, string> {
        match self.bool_list_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional list of booleans.
    pub fn bool_list_opt(&self, name: &str) -> Result<Option<slice<bool>>, string> {
        self.list_opt(name, "a boolean", |value| value.AsBool())
    }

    /// A required list of objects, as the engine's IDs for them.
    ///
    /// The list counterpart of [`object`](Arguments::object): each element
    /// arrives as an ID, and the generated dispatch rebuilds the whole list
    /// with [`crate::from_ids`].
    pub fn object_list(&self, name: &str) -> Result<slice<string>, string> {
        match self.object_list_opt(name)? {
            Some(value) => Ok(value),
            None => Err(self.missing(name)),
        }
    }

    /// An optional list of objects, as the engine's IDs for them.
    pub fn object_list_opt(&self, name: &str) -> Result<Option<slice<string>>, string> {
        self.list_opt(name, "an object id", |value| value.AsString().cloned())
    }
}

/// The state the object being called on arrived with.
///
/// The engine keeps the JSON document a module object was last encoded to and
/// hands it back as `currentFunctionCall.parent`; this is that document,
/// flattened into its fields. A top-level call carries an empty object, which
/// is the engine's own answer to "the function is on the module itself".
///
/// # Why it is an [`Arguments`]
///
/// A field and an argument are the same thing to a decoder: a name, and a JSON
/// value under it. Every rule past finding the name is shared — an absent value
/// and a JSON `null` both read as missing, a wrong type is an error naming what
/// was expected, an element error names its index — and so is the set of types,
/// down to lists of objects and enum members. Writing that twice is how the two
/// drift apart, and the drift that matters is silent: an accessor and the
/// encoder on the other side of the boundary have to agree, or a value changes
/// on the way through rather than failing.
///
/// So this holds an `Arguments` over the parent's fields and derefs to it, and
/// the generated [`ObjectState::from_state`] reads one accessor per declared
/// field. The cost is re-encoding each field's value as text at decode time,
/// which is what [`Arguments`] stores; that is a few small allocations per
/// call against one copy of the decoding rules.
pub struct State {
    values: Arguments,
}

impl State {
    /// No state at all: every field reads as absent.
    pub fn empty() -> State {
        State {
            values: Arguments::fields(make!([](string, string), 0, 0)),
        }
    }

    /// Decode the `parent` document the engine sent.
    ///
    /// An empty document is the empty state rather than a syntax error: a
    /// module that has never encoded itself has nothing to send back, and the
    /// distinction is not one a field accessor could act on anyway. So is a
    /// document that is not an object, which is what `null` arrives as.
    pub fn decode(document: &string) -> Result<State, string> {
        if document.Len() == 0 {
            return Ok(State::empty());
        }
        let mut parsed = json::Value::Null;
        let err = json::Unmarshal(&goish::bytes(document.clone()), &mut parsed);
        if err != nil {
            return Err(string("decoding the parent object: ") + err.Error());
        }
        let fields = match parsed.AsObject() {
            Some(fields) => fields,
            None => return Ok(State::empty()),
        };

        let mut entries = make!([](string, string), 0, 0);
        let names = fields.Keys();
        let mut i: int = 0;
        while i < names.Len() {
            let name = names[i].clone();
            i += 1;
            let (value, ok) = fields.GetRef(name.clone());
            let value = match (value, ok) {
                (Some(value), true) => value,
                _ => continue,
            };
            let (encoded, err) = json::Marshal(value);
            if err != nil {
                return Err(string("re-encoding the parent field ") + name + ": " + err.Error());
            }
            entries = append!(entries, (name, string(encoded)));
        }
        Ok(State {
            values: Arguments::fields(entries),
        })
    }
}

impl core::ops::Deref for State {
    type Target = Arguments;

    fn deref(&self) -> &Arguments {
        &self.values
    }
}

/// Builds the JSON document an object's state is stored as.
///
/// One `put` per declared field, in declaration order, each handed a value the
/// `encode_*` functions below already rendered — the same ones the dispatch
/// encodes a return with, so a field and a return of the same type make the
/// same round trip.
pub struct StateWriter {
    out: string,
    empty: bool,
}

impl Default for StateWriter {
    fn default() -> StateWriter {
        StateWriter::new()
    }
}

impl StateWriter {
    /// An object with no fields yet.
    pub fn new() -> StateWriter {
        StateWriter {
            out: string("{"),
            empty: true,
        }
    }

    /// Append one field, whose value is already JSON.
    ///
    /// The name is `&'static str` because that is what a [`FieldDef`] carries
    /// and what the macro emits — a field name comes from the source, never
    /// from the wire.
    pub fn put(&mut self, name: &'static str, encoded: string) {
        if !self.empty {
            self.out += ",";
        }
        self.empty = false;
        self.out += crate::json_string(&string(name));
        self.out += ":";
        self.out += encoded;
    }

    /// The finished document.
    pub fn finish(self) -> string {
        self.out + "}"
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

/// JSON-encode an enum result as the name of its member.
///
/// The same text an enum argument arrives as, going the other way: the engine
/// matches it against the members the module registered and hands the caller
/// the schema's own spelling of it. Infallible, unlike [`encode_object`] — the
/// name is a `&'static str` the macro wrote, not something to go and fetch.
pub fn encode_enum<T: EnumType>(value: &T) -> string {
    crate::json_string(&string(value.member()))
}

/// JSON-encode a function that returns nothing.
pub fn encode_void() -> string {
    string("null")
}

/// JSON-encode the absent half of an `Option<T>`.
///
/// The same three characters [`encode_void`] writes, and deliberately a separate
/// function: a Void return says the function produces no value *ever*, and this
/// one says a function that does produce values did not this time. The engine is
/// told which is which by the return `TypeDef` — `VOID_KIND` against a kind
/// carrying `withOptional` — and only the declaration distinguishes them, since
/// the wire form cannot.
///
/// A field the object does not carry is written the same way rather than being
/// left out of the document, matching how the engine hands one in: an accessor
/// reads the two as the same thing, so the round trip is closed either way, and
/// writing the key keeps the encoded object shaped like the type that declared
/// it.
///
/// Infallible, unlike [`encode_object`]: there is no object to resolve an ID
/// for, which is the whole reason an `Option<Directory>` that is `None` costs no
/// round trip.
pub fn encode_null() -> string {
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

/// JSON-encode a module object as its state.
///
/// The other kind of object return, and the one that is not an ID:
/// [`encode_object`] resolves an *engine* object, which the engine already
/// holds and can name, while a module's own object exists only as the fields it
/// carries. So this is the document [`ObjectState::to_state`] wrote, handed
/// straight back — the engine keeps it and returns it as `parent` when the
/// caller goes on chaining.
pub fn encode_state<T: ObjectState>(value: &T) -> Result<string, string> {
    value.to_state()
}

/// JSON-encode a list, given an encoder for one element.
///
/// A returned list is the elements' own encoding inside `[…]`, so this is the
/// three infallible list encoders below; the object one has its own loop,
/// because resolving an id is a round trip and so cannot be written this way.
fn encode_list<T>(values: &slice<T>, element: impl Fn(&T) -> string) -> string {
    let mut out = string("[");
    let mut i: int = 0;
    while i < values.Len() {
        if i > 0 {
            out = out + ",";
        }
        out = out + element(&values[i]);
        i += 1;
    }
    out + "]"
}

/// JSON-encode a list of strings.
pub fn encode_string_list(values: &slice<string>) -> string {
    encode_list(values, encode_string)
}

/// JSON-encode a list of integers.
pub fn encode_int_list(values: &slice<int>) -> string {
    encode_list(values, |value| encode_int(*value))
}

/// JSON-encode a list of floats.
pub fn encode_float_list(values: &slice<goish::float64>) -> string {
    encode_list(values, |value| encode_float(*value))
}

/// JSON-encode a list of booleans.
pub fn encode_bool_list(values: &slice<bool>) -> string {
    encode_list(values, |value| encode_bool(*value))
}

/// JSON-encode a list of objects as the engine's IDs for them.
///
/// Fallible for the reason [`encode_object`] is, once per element: each object
/// is a chain nothing has sent yet, and asking for its ID is what runs it. So a
/// list of *n* objects a function built is *n* round trips, and the first that
/// fails is the message the caller gets.
pub fn encode_object_list<T: crate::ObjectId>(values: &slice<T>) -> Result<string, string> {
    let mut out = string("[");
    let mut i: int = 0;
    while i < values.Len() {
        if i > 0 {
            out = out + ",";
        }
        out = out + encode_object(&values[i])?;
        i += 1;
    }
    Ok(out + "]")
}

/// Rebuild a list of objects from the IDs they arrived as.
///
/// The list counterpart of [`ObjectId::from_id`](crate::ObjectId::from_id), and
/// what the generated dispatch calls for a `slice<Directory>` argument: the
/// engine sends a list of IDs, and each becomes a client object that carries
/// its own connection. Infallible, like the single-object case — nothing is
/// sent until a field is asked for.
pub fn from_ids<T: crate::ObjectId>(ids: slice<string>) -> slice<T> {
    let mut out = make!([]T, 0, ids.Len());
    let mut i: int = 0;
    while i < ids.Len() {
        out = append!(out, T::from_id(ids[i].clone()));
        i += 1;
    }
    out
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
    // is a nested selection rather than a leaf; `parent` is a JSON scalar, which
    // arrives as the document's own text the way an argument's value does.
    let call = engine::fetch(
        &session,
        &Chain::root().field("currentFunctionCall", string("")),
        &(
            Leaf::<string>::new("parentName"),
            Leaf::<string>::new("name"),
            Leaf::<string>::new("parent"),
            ListField::<ArgValueFields>::new("inputArgs").select(|a| (a.name(), a.value())),
        ),
    );
    let (parent_name, name, parent, input_args) = match call {
        Ok(call) => call,
        Err(message) => fail(message),
    };

    // Three calls share one entry point, told apart by which of the two names is
    // empty. An empty parentName is the engine asking what this module serves.
    // An empty function name on a named parent is the constructor: the engine
    // spells it that way because it is the object's own function rather than one
    // of the object's. Anything else is a call against a parent the engine has
    // the state for.
    let result = if parent_name.Len() == 0 {
        register::<T>(&session)
    } else if name.Len() == 0 {
        T::construct(&Arguments::new(input_args))
    } else {
        State::decode(&parent)
            .and_then(|state| T::from_state(&state))
            .and_then(|receiver| receiver.invoke(&name, &Arguments::new(input_args)))
    };

    match result.and_then(|value| return_value(&session, &value)) {
        Ok(()) => os::Exit(0),
        Err(message) => fail(message),
    }
}

/// Describe the module to the engine and return the description's ID.
///
/// Build a `TypeDef` for the root object, hang its fields, its functions and
/// its constructor off it, attach it to a `Module` alongside every enum the
/// module declares, and hand back the module's ID.
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

    // One `withField` per `pub` field of the struct. A field is not a function:
    // the engine answers for it out of the state the module last handed back,
    // so `dagger call --image=alpine image` never starts a module process.
    for def in T::fields() {
        let field_type = build_type_def(transport, def.kind, def.type_name, def.list, def.optional)?;
        let mut args = Args::new();
        args.put("name", arg_string(def.name));
        args.put("typeDef", arg_string(field_type));
        if !def.doc.is_empty() {
            args.put("description", arg_string(def.doc));
        }
        if !def.deprecated.is_empty() {
            args.put("deprecated", arg_string(def.deprecated));
        }
        if def.source.is_known() {
            let source_map = build_source_map(transport, &def.source)?;
            args.put("sourceMap", arg_string(source_map));
        }
        object = object.field("withField", args.finish());
    }

    // One `withFunction` per declared function, chained. The response nests
    // exactly as the chain does, and `Chain::decode` walks it back the same
    // way, so repeating a field name costs nothing here.
    for def in T::functions() {
        let function_id = build_function(transport, def)?;
        let mut args = Args::new();
        args.put("function", arg_string(function_id));
        object = object.field("withFunction", args.finish());
    }

    // The constructor, if the object declared one. It is an ordinary `Function`
    // — built by the same builder, with the same arguments and source map — hung
    // off the type def by a field of its own rather than by name, which is why
    // `build_function` is handed a `FunctionDef` whose name is empty.
    if let Some(def) = T::constructor() {
        let function_id = build_function(transport, def)?;
        let mut args = Args::new();
        args.put("function", arg_string(function_id));
        object = object.field("withConstructor", args.finish());
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
    let mut module = module.field("withObject", args.finish());

    // An enum is declared to the module rather than to the object: a signature
    // only ever *references* one by name, so this is the one place its members
    // are written down. `Module.withEnum` takes the TypeDefID of an enum
    // TypeDef, the way `withObject` takes an object's.
    for def in T::enums() {
        let enum_id = build_enum(transport, def)?;
        let mut args = Args::new();
        args.put("enum", arg_string(enum_id));
        module = module.field("withEnum", args.finish());
    }
    let module_id = fetch_id(transport, &module)?;

    // Both paths hand back "a JSON document", so the ID is JSON-encoded here to
    // match what dispatch returns from encode_*. returnValue then embeds it as a
    // GraphQL string exactly once.
    Ok(json_string(&module_id))
}

/// Build one enum `TypeDef`, members and all, and return its ID.
///
/// `withEnumMember` takes the member's name and nothing else. The engine keeps
/// what it is given as the member's *original* name — what it hands the module
/// for an argument, and what it expects back for a return — and derives the
/// name a caller writes from it. It also accepts a `value`, for an SDK whose
/// members carry a string distinct from their identifier; a Rust variant has no
/// such thing, so leaving it unset keeps the two spellings the module deals in
/// down to one.
fn build_enum(transport: &dyn Transport, def: &EnumDef) -> Result<string, string> {
    let mut args = Args::new();
    args.put("name", arg_string(def.name));
    if !def.doc.is_empty() {
        args.put("description", arg_string(def.doc));
    }
    let mut type_def = Chain::root()
        .field("typeDef", string(""))
        .field("withEnum", args.finish());

    for member in def.members {
        let mut args = Args::new();
        args.put("name", arg_string(member.name));
        if !member.doc.is_empty() {
            args.put("description", arg_string(member.doc));
        }
        type_def = type_def.field("withEnumMember", args.finish());
    }

    fetch_id(transport, &type_def)
}

/// Build one `Function` and return its ID.
fn build_function(transport: &dyn Transport, def: &FunctionDef) -> Result<string, string> {
    let return_type =
        build_type_def(
            transport,
            def.return_kind,
            def.return_type_name,
            def.return_list,
            def.return_optional,
        )?;

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
        let arg_type = build_type_def(transport, arg.kind, arg.type_name, arg.list, arg.optional)?;
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
/// `type_name` names the engine type for `OBJECT_KIND` and `ENUM_KIND` and is
/// empty otherwise: those two are described by name rather than by kind, since
/// the kind alone would not say *which* object or enum it is. An enum is named
/// here as a reference — the members are declared once, by [`build_enum`].
///
/// A list is a `TypeDef` *wrapping* another one, so `list` makes `kind` and
/// `type_name` describe the element: the element is built and resolved to an ID
/// first, and `withListOf` takes that ID. `optional` then applies to the list
/// rather than to the element, which is what `Option<slice<T>>` means.
fn build_type_def(
    transport: &dyn Transport,
    kind: &'static str,
    type_name: &'static str,
    list: bool,
    optional: bool,
) -> Result<string, string> {
    let mut args = Args::new();
    let mut type_def = Chain::root().field("typeDef", string(""));

    if type_name.is_empty() {
        // `kind` is a GraphQL enum literal, so it is spliced unquoted rather
        // than through `arg_string`. It only ever comes from the macro's fixed
        // set, never from user text. A named type's name is a string argument,
        // so it goes through the usual quoting.
        args.put("kind", string(kind));
        type_def = type_def.field("withKind", args.finish());
    } else if kind == "ENUM_KIND" {
        args.put("name", arg_string(type_name));
        type_def = type_def.field("withEnum", args.finish());
    } else {
        args.put("name", arg_string(type_name));
        type_def = type_def.field("withObject", args.finish());
    }

    if list {
        // The one place a builder here needs an ID before it is finished: the
        // element is an argument to `withListOf`, so the chain above has to be
        // sent and resolved rather than extended.
        let element = fetch_id(transport, &type_def)?;
        let mut args = Args::new();
        args.put("elementType", arg_string(element));
        type_def = Chain::root().field("typeDef", string("")).field("withListOf", args.finish());
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
