//! Attribute macros that declare a Dagger module's objects and functions.
//!
//! Rust has no runtime reflection, and unlike the Go SDK — which recovers
//! signatures by parsing the user's package at codegen time — there is nothing
//! to inspect a module's source with at build time. These macros close that gap:
//! they read the signatures at compile time and emit a static table the
//! entrypoint walks, both to tell the engine what the module serves and to
//! dispatch a call to the right function.
//!
//! ```ignore
//! #[dagger::object]
//! pub struct MyModule {
//!     /// The image every step runs in.
//!     pub image: string,
//! }
//!
//! #[dagger::object]
//! impl MyModule {
//!     /// Configure the module.
//!     #[dagger::constructor]
//!     pub fn new(#[dagger(default = "alpine:3.21")] image: string) -> Self { … }
//!
//!     /// Build the project.
//!     #[dagger::function]
//!     pub fn build(
//!         &self,
//!         #[dagger(doc = "Tag to apply")] tag: Option<string>,
//!     ) -> string { … }
//! }
//! ```
//!
//! [`macro@object`] goes on both halves of the type, because an attribute macro
//! sees only the item it is written on: the `impl` block carries the functions,
//! the `struct` carries the state that survives from one call to the next.
//!
//! Argument options live in `#[dagger(...)]` on the parameter — `default`,
//! `default_path`, `ignore`, `doc`, `deprecated` — mirroring the Go SDK's
//! `+default`, `+defaultPath`, `+ignore` pragmas. Optionality is carried by
//! `Option<T>` rather than a marker, and a parameter with a `default` is
//! optional by construction.
//!
//! A *return* may be `Option<T>` for the same reason and by the same spelling:
//! the return type is declared optional to the engine, and a `None` reaches the
//! caller as JSON null. That is a different answer from `dagger::fail` — "no
//! value" rather than "the call did not work" — which is why it is a return type
//! rather than an error.
//!
//! [`macro@check`] marks a function `dagger check` should run, the Go SDK's
//! `+check` pragma. [`macro@constructor`] marks the one that builds the object,
//! the Go SDK's `func New(...)`. [`macro@enum_type`] declares an enum the
//! module defines, which `#[dagger::object(enums(...))]` then names — the Go
//! SDK's type with string constants, the TypeScript SDK's `@enumType()`.
//!
//! What a function *is* otherwise goes in the marker attribute —
//! `#[dagger::function(generate)]` declares a generator and
//! `#[dagger::function(deprecated = "...")]` a deprecation, the way Go writes
//! `+generate` and `+deprecated` in the doc comment.
//!
//! Descriptions come from `///` comments: a method's is the function's, a
//! field's is the field's, and the one on the annotated `impl` block is the
//! object's — and the module's, since the crate's `//!` doc is not something an
//! attribute macro on an `impl` can see. Every declaration also carries the
//! file and line it was written at, read off its `proc_macro::Span`, so an
//! engine-side error can point at the source.

extern crate proc_macro;

mod parse;
#[cfg(test)]
mod tests;

use parse::{quote_str, render, split_commas, unquote, Attr, Function, SourceLoc};
use proc_macro::{Delimiter, TokenStream, TokenTree};

/// Mark a method as part of the module's API.
///
/// Only methods carrying this attribute are exposed; anything else in the
/// `impl` block stays private to the module, so helpers need no special
/// treatment.
///
/// ```ignore
/// #[dagger::object]
/// impl Build {
///     /// Compile the project.                    // becomes the description
///     #[dagger::function]
///     pub fn compile(&self, target: string) -> string {
///         self.toolchain(target)                  // calls a private helper
///     }
///
///     // No attribute: invisible to `dagger call`.
///     fn toolchain(&self, target: string) -> string { target }
/// }
/// ```
///
/// The method's `///` doc comment becomes the function's description, and its
/// name is camelCased for the API — `container_echo` is called as
/// `container-echo` on the command line.
///
/// # Failing
///
/// A function returns its value directly, or as `Result<T, string>`:
///
/// ```ignore
/// /// What the container printed.
/// #[dagger::function]
/// pub fn out(&self) -> Result<string, string> {
///     dag().container().from("alpine:3.22").with_exec(&["echo", "hi"]).stdout()
/// }
/// ```
///
/// Every client method is fallible — reaching the engine is a round trip — so
/// this is what lets `?` carry a failure out of the function rather than
/// `unwrap_or_else(|m| dagger::fail(m))` at each call. An `Err` ends the call
/// exactly as [`dagger::fail`] does: the message on stderr, a non-zero exit.
///
/// The engine is told what the function *produces* either way — failure is not
/// a kind it has — so `Result<T, string>` and `T` declare the same thing.
///
/// The error is goish's `string` or its `error`, whichever the work at hand
/// fails with: the client fails with the message itself, goish's own APIs fail
/// with an `error`, and the two cross with `map_err(errors::New)` one way and
/// [`dagger::error_message`] the other. Any other error type is a compile error
/// naming it — there is no `Display` in goish to read a message off one with.
///
/// ```ignore
/// /// The first line of a file the module carries.
/// #[dagger::function]
/// pub fn first_line(&self, path: string) -> Result<string, errors::error> {
///     let (data, err) = os::ReadFile(path);
///     if err != nil {
///         return Err(err);
///     }
///     Ok(strings::SplitN(string(data), "\n", 2)[0].clone())
/// }
/// ```
///
/// # Options
///
/// What a function *is* goes in the attribute itself — the one slot Go writes
/// its `+` pragmas into.
///
/// | Option | Effect | Go SDK equivalent |
/// | --- | --- | --- |
/// | `generate` | The function is a generator: `dagger generate` runs it and applies the `Changeset` it returns | `+generate` |
/// | `deprecated = "..."` | Marks the function deprecated, with a migration note | `+deprecated` |
///
/// ```ignore
/// /// Regenerate the checked-in fixtures.
/// #[dagger::function(generate)]
/// pub fn generate(&self, ws: Workspace) -> Changeset {
///     …
/// }
/// ```
///
/// `deprecated` is the same option a parameter carries in
/// `#[dagger(deprecated = "...")]`, one level up. It goes in the marker
/// attribute because a method has no `#[dagger(...)]` of its own:
///
/// ```ignore
/// /// Build the project. Superseded by `build`.
/// #[dagger::function(deprecated = "use build instead")]
/// pub fn compile(&self) -> string { … }
/// ```
///
/// A generator is called with nothing, so the engine holds it to a shape: it
/// must return a `Changeset`, and every argument must be one the engine can
/// leave out — an `Option<T>`, one with a `default`, or a `Workspace`, which
/// the engine supplies itself. Both halves are checked at compile time, so a
/// signature that would fail to load fails to compile instead.
///
/// On its own this attribute is inert: it returns the method untouched so the
/// path resolves and an IDE sees an ordinary function. [`macro@object`] on the
/// surrounding `impl` block is what reads it, and what strips it before `rustc`
/// gets there.
///
/// [`dagger::fail`]: ../dagger/fn.fail.html
/// [`dagger::error_message`]: ../dagger/fn.error_message.html
#[proc_macro_attribute]
pub fn function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Mark a method as a check: something `dagger check` runs.
///
/// A check validates the project — a test, a lint, a scan — and passes or fails.
/// `dagger check` discovers every check a module exposes and runs them all,
/// which is the Go SDK's `// +check` pragma and the TypeScript SDK's `@check()`
/// decorator.
///
/// ```ignore
/// #[dagger::object]
/// impl Build {
///     /// Lint the project.
///     #[dagger::check]
///     pub fn lint(&self) {
///         if !self.sources_are_formatted() {
///             ::dagger::fail(::goish::convert::string("lint failed"))
///         }
///     }
/// }
/// ```
///
/// ```console
/// $ dagger check
/// ```
///
/// A check is also an ordinary function — it is still callable as
/// `dagger call lint` — so this attribute implies [`macro@function`] and the two
/// need not both be written. It fails the same way any other function does: by
/// exiting non-zero, which [`dagger::fail`] does with a message on stderr, and
/// which returning `Err` from a `-> Result<(), string>` check does for it.
///
/// # No required arguments
///
/// `dagger check` runs a check with no arguments, so every argument must be an
/// `Option<T>` or carry a `#[dagger(default = ...)]`. A check with a required
/// argument cannot be run, and the engine deals with that by leaving it out of
/// the check tree — it simply never appears in `dagger check`. Since that is
/// silent, the macro rejects it at compile time instead, naming the argument.
///
/// [`dagger::fail`]: ../dagger/fn.fail.html
#[proc_macro_attribute]
pub fn check(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Mark an associated function as the object's constructor.
///
/// A constructor configures the module once, rather than every function
/// repeating the same arguments. Its parameters become the module's own flags,
/// so they are written *before* the function on the command line:
///
/// ```ignore
/// #[dagger::object]
/// pub struct Build {
///     /// The image every step runs in.
///     pub image: string,
/// }
///
/// #[dagger::object]
/// impl Build {
///     /// Configure the builder.
///     #[dagger::constructor]
///     pub fn new(#[dagger(default = "alpine:3.21")] image: string) -> Build {
///         Build { image }
///     }
///
///     /// The image this builder was configured with.
///     #[dagger::function]
///     pub fn base(&self) -> string {
///         self.image.clone()
///     }
/// }
/// ```
///
/// ```console
/// $ dagger call --image=rust:1.90 base
/// rust:1.90
/// ```
///
/// This is the Go SDK's `func New(...) *MyModule`. The name is free here
/// because the attribute is what marks it — `new` is only the convention — and
/// an object may declare at most one.
///
/// # Shape
///
/// A constructor takes no receiver and returns the object it builds: `Self`, or
/// the type by name, or either inside a `Result`. Its arguments carry the same
/// `#[dagger(...)]` options any other function's do.
///
/// The engine registers it under the empty name, calls it before any function
/// when the caller starts from the module, and keeps the state it built for the
/// call that follows.
///
/// # Without one
///
/// An object with no constructor is built from an empty document, which works
/// exactly when it has no fields to fill — a unit struct. A `struct` with
/// fields and no constructor fails at call time, naming the field nothing
/// supplied.
///
/// Like [`macro@function`], this attribute is inert on its own:
/// [`macro@object`] on the surrounding `impl` block is what reads it.
#[proc_macro_attribute]
pub fn constructor(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Declare the object a module serves: its state, and the functions it exposes.
///
/// Applied to *both* halves of the module's root type — the `struct` and the
/// `impl` block — because an attribute macro sees only the item it is written
/// on, and the two carry different halves of what the engine has to be told.
///
/// On the `impl` block it reads the method signatures at compile time and emits
/// the `Object` impl that `serve::<T>()` walks: the table describing the module
/// to the engine, and the dispatch that routes an incoming call. On the
/// `struct` it reads the `pub` fields and emits `ObjectState`, which encodes
/// them into the document the engine keeps between calls and decodes them back
/// into the receiver — see [State](#state) below.
///
/// ```ignore
/// #![no_std]
/// #![no_main]
///
/// use goish::{fmt, string};
///
/// #[dagger::object]
/// pub struct Build;
///
/// /// Builds and publishes images.
/// #[dagger::object]
/// impl Build {
///     /// Build an image and return its tag.
///     #[dagger::function]
///     pub fn image(
///         &self,
///         // Options go on the parameter; `doc` carries its description,
///         // since Rust has no doc comments on parameters.
///         #[dagger(default = "alpine:3.21")]
///         base: string,
///
///         #[dagger(doc = "Tag to apply; defaults to the short SHA")]
///         tag: Option<string>,
///
///         #[dagger(default = 1)]
///         jobs: int,
///
///         #[dagger(deprecated = "use `jobs` instead")]
///         parallel: Option<bool>,
///     ) -> string {
///         fmt::Sprintf!("%s:%s", base, tag.unwrap_or(string("latest")))
///     }
/// }
///
/// #[goish::main]
/// fn main() {
///     dagger::serve::<Build>()
/// }
/// ```
///
/// # State
///
/// A `pub` field of the annotated `struct` is state: the engine declares it,
/// keeps the value the module last encoded, and hands it back on the next call
/// as the receiver a function is dispatched on.
///
/// ```ignore
/// #[dagger::object]
/// pub struct Build {
///     /// The image every step runs in.       // becomes the field's description
///     pub image: string,
///     /// Superseded by `image`.
///     #[dagger(deprecated = "use image instead")]
///     pub base: Option<string>,
/// }
/// ```
///
/// Unlike a parameter, a field takes its description from its `///` comment —
/// it is state rather than an input, and Rust does have doc comments there — so
/// `deprecated` is the only `#[dagger(...)]` option it accepts.
///
/// A field must be `pub`. A private one would be state the engine never sees,
/// and so state that does not survive the call that set it; the macro says so
/// rather than dropping it the way Go drops an unexported field.
///
/// [`macro@constructor`] is what fills the fields in, and a function returning
/// `Self` hands back a reconfigured object — as its *state* rather than as an
/// ID, since the engine holds no value to mint one for.
///
/// # Descriptions
///
/// The `///` comment on the annotated `impl` block is the object's description,
/// and the module's: a Rust module's root type *is* the module, and the crate's
/// own `//!` doc belongs to the file, which an attribute macro on an `impl`
/// never sees. Write it on the `impl`, not on the `struct` — both carry this
/// attribute now, but only the block's reaches the engine; the struct's is for
/// `cargo doc`.
///
/// Each declaration also carries where it was written — file, line and column,
/// read off its `proc_macro::Span` — so an engine-side error about a function
/// or an argument points at the source rather than at a name.
///
/// # Argument options
///
/// These live in `#[dagger(...)]` on the parameter itself. The attribute is
/// removed before `rustc` sees the function, so it needs nothing in scope.
///
/// | Option | Effect | Go SDK equivalent |
/// | --- | --- | --- |
/// | `default = <literal>` | Value used when the caller omits the argument | `+default` |
/// | `default_path = "..."` | For `Directory`/`File`, load from the context directory | `+defaultPath` |
/// | `ignore = ["...", ...]` | Patterns to skip when loading a contextual argument | `+ignore` |
/// | `doc = "..."` | The argument's description | a doc comment |
/// | `deprecated = "..."` | Marks the argument deprecated, with a migration note | `+deprecated` |
///
/// `doc` is the one place this cannot mirror Go: Rust has no doc comments on
/// parameters, so the description has to be an option.
///
/// `default_path` and `ignore` apply to a `Directory` or `File` argument only —
/// the engine loads it from the module's context directory when the caller
/// leaves it out — so writing either on anything else is a compile error naming
/// the parameter.
///
/// # Optionality
///
/// An argument is optional when its type is `Option<T>`, or when it has a
/// `default` — a defaulted argument the caller may omit is optional by
/// construction. There is no `+optional` marker to write.
///
/// A `default_path` argument is a third case: it stays required in the
/// signature, since the function always receives one, but the caller may still
/// omit it because the engine supplies it. That is enough to satisfy the "no
/// required arguments" rules that checks and generators are held to.
///
/// # Checks
///
/// A method marked [`macro@check`] is exported like any other and additionally
/// flagged for `dagger check`, which runs every check a module exposes.
///
/// # Supported types
///
/// `string` (and `String`), `int`, and `bool`, plus `Option<T>` of each. A
/// function returning nothing maps to the engine's `VOID_KIND`.
///
/// Object types too: anything named as a plain type — `Directory`, `Container`,
/// `Changeset`, `Workspace` — is registered as that engine object, under the
/// last segment of the path, so `gen::Directory` and `Directory` are the same
/// declaration.
///
/// And the enums the module itself declares, which is what `enums(...)` on this
/// attribute is for:
///
/// ```ignore
/// #[dagger::enum_type]
/// pub enum TargetOs { Alpine, Debian }
///
/// #[dagger::object(enums(TargetOs))]
/// impl Build {
///     #[dagger::function]
///     pub fn image(&self, os: TargetOs) -> TargetOs { os }
/// }
/// ```
///
/// A type name in a signature is an engine object *unless* that list says
/// otherwise: the macro has no schema to look a name up in and sees one item at
/// a time, so nothing but the list connects an enum's declaration to the module
/// that serves it. See [`macro@enum_type`].
///
/// ```ignore
/// /// Build the sources in `src`.
/// #[dagger::function]
/// pub fn build(&self, #[dagger(default_path = ".")] src: Directory) -> Container {
///     // A client method's `DirectoryID` argument is a `string`, so the object
///     // goes back in as its id.
///     let id = src.to_id().unwrap_or_else(|message| ::dagger::fail(message));
///     dag().container().from("rust:1.86").with_directory("/src", id)
/// }
/// ```
///
/// An object crosses the boundary as an engine ID: an argument is rebuilt with
/// [`ObjectId::from_id`] before the function sees it, and a returned one is
/// resolved to its ID afterwards, which for a generated object is a round trip.
/// So the type has to implement `dagger::ObjectId`, which the generated
/// bindings do for every object the engine has a `loadXFromID` for — a name the
/// engine has no such loader for, or a plain misspelling, is a compile error
/// about that trait rather than a module that fails to load.
///
/// A return may be `Option<T>`, which is declared optional to the engine and
/// encoded as JSON null when it is `None`. That includes `Option<slice<T>>`:
/// the list is what is optional, so the engine is told about a nullable list
/// rather than a list of nullable elements, and a `None` is the same null any
/// other absent return is. `Option<()>` is the one refusal — a Void return is
/// the absence of a value already.
///
/// What is not supported is a list of lists, or an optional *element*
/// (`slice<Option<T>>`); both are refused by name rather than emitted.
///
/// [`ObjectId::from_id`]: ../dagger/trait.ObjectId.html#tymethod.from_id
#[proc_macro_attribute]
pub fn object(attr: TokenStream, item: TokenStream) -> TokenStream {
    match expand(attr, item.clone()) {
        Ok(generated) => {
            let mut out = strip_markers(item);
            out.extend(generated);
            out
        }
        Err(message) => compile_error(&message),
    }
}

/// Declare an enum the module defines, so a function may take and return it.
///
/// The Go SDK recovers an enum from a type with string constants and the
/// TypeScript SDK from `@enumType()`; Rust has an enum of its own, and its
/// variants — with their doc comments — are exactly what the engine wants, so
/// this reads them off the declaration.
///
/// ```ignore
/// /// The operating system a build targets.
/// #[dagger::enum_type]
/// pub enum TargetOs {
///     /// Alpine Linux.
///     Alpine,
///     /// Debian.
///     Debian,
/// }
///
/// #[dagger::object(enums(TargetOs))]
/// impl Build {
///     /// Build for one OS.
///     #[dagger::function]
///     pub fn image(&self, os: TargetOs) -> TargetOs {
///         os
///     }
/// }
/// ```
///
/// # It has to be named twice
///
/// Once here, and once in [`macro@object`]'s `enums(...)` list. Rust has no
/// runtime reflection and a macro sees one item at a time, so an attribute on
/// the enum cannot make the enum known to the `serve::<T>()` that registers the
/// module — nothing links the two but the list. Forgetting it is a compile
/// error rather than a module that misbehaves: a type named in a signature and
/// not in that list is registered as an engine *object*, and the check that it
/// is one is `dagger::ObjectId`, which no enum implements.
///
/// # Spelling
///
/// The member the engine publishes is its own SCREAMING_SNAKE_CASE of the
/// variant name — `AlpineLinux` is called as `ALPINE_LINUX` — and the engine
/// derives that itself. What crosses the call boundary is the variant name as
/// written here, in both directions, so a module never spells a member two
/// ways.
///
/// The type's `///` doc comment becomes the enum's description and each
/// variant's becomes that member's.
///
/// # What is not an enum here
///
/// A variant that carries data — `Tagged(string)` — or a discriminant is a
/// compile error naming it: an engine enum is a set of member names, so there
/// is nowhere for either to go.
#[proc_macro_attribute]
pub fn enum_type(_attr: TokenStream, item: TokenStream) -> TokenStream {
    match expand_enum(item.clone()) {
        Ok(generated) => {
            let mut out = item;
            out.extend(generated);
            out
        }
        Err(message) => compile_error(&message),
    }
}

/// Emit a `compile_error!` so a bad signature is reported at the macro, not as
/// a confusing type error in the generated code.
fn compile_error(message: &str) -> TokenStream {
    format!("compile_error!({});", quote_str(message))
        .parse()
        .expect("compile_error! is valid Rust")
}

/// Re-emit the impl block with `#[dagger::function]` and `#[dagger(...)]`
/// removed, since neither is a real attribute as far as `rustc` is concerned.
fn strip_markers(item: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = item.into_iter().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < trees.len() {
        let is_hash = matches!(&trees[i], TokenTree::Punct(p) if p.as_char() == '#');
        if is_hash {
            if let Some(TokenTree::Group(g)) = trees.get(i + 1) {
                if g.delimiter() == Delimiter::Bracket && is_dagger_attr(g.stream()) {
                    i += 2;
                    continue;
                }
            }
        }
        match &trees[i] {
            TokenTree::Group(g) => {
                let inner = strip_markers(g.stream());
                let mut rebuilt = proc_macro::Group::new(g.delimiter(), inner);
                rebuilt.set_span(g.span());
                out.push(TokenTree::Group(rebuilt));
            }
            other => out.push(other.clone()),
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// Whether an attribute's contents name this crate's markers.
fn is_dagger_attr(stream: TokenStream) -> bool {
    let mut path = String::new();
    for tree in stream {
        match tree {
            TokenTree::Ident(id) => path.push_str(&id.to_string()),
            TokenTree::Punct(p) if p.as_char() == ':' => path.push(':'),
            _ => break,
        }
    }
    matches!(
        path.as_str(),
        "dagger" | "dagger::function" | "dagger::check" | "dagger::constructor"
    )
}

/// Build the `EnumType` impl for the annotated enum.
fn expand_enum(item: TokenStream) -> Result<TokenStream, String> {
    let declared = parse::parse_enum(item)?;

    enum_impl(&declared)
        .parse()
        .map_err(|e| format!("generated code did not parse: {e}"))
}

/// Render the `EnumType` impl for one declared enum.
fn enum_impl(declared: &parse::Enum) -> String {
    let mut members = String::new();
    let mut writes = String::new();
    let mut reads = String::new();
    for variant in &declared.variants {
        members.push_str(&format!(
            "::dagger::__internal::EnumMemberDef {{ name: {name}, doc: {doc} }},",
            name = quote_str(&variant.name),
            doc = quote_str(&variant.doc),
        ));
        writes.push_str(&format!(
            "{ty}::{variant} => {name},",
            ty = declared.name,
            variant = variant.name,
            name = quote_str(&variant.name),
        ));
        // An if/else chain rather than a `match`, for the same reason the
        // function dispatch is one: the name is a goish `string`, which compares
        // against a literal with `==` but cannot be a match scrutinee.
        reads.push_str(&format!(
            "if name == {name} {{ return ::core::result::Result::Ok({ty}::{variant}); }}",
            name = quote_str(&variant.name),
            ty = declared.name,
            variant = variant.name,
        ));
    }

    format!(
        r#"
impl ::dagger::__internal::EnumType for {ty} {{
    const DEF: ::dagger::__internal::EnumDef = ::dagger::__internal::EnumDef {{
        name: {name},
        doc: {doc},
        members: &[{members}],
    }};

    fn member(&self) -> &'static str {{
        match self {{ {writes} }}
    }}

    fn from_member(
        name: &::goish::gostring::string,
    ) -> ::core::result::Result<{ty}, ::goish::gostring::string> {{
        {reads}
        ::core::result::Result::Err(
            ::goish::convert::string("not a member of {ty}: ") + name.clone(),
        )
    }}
}}
"#,
        ty = declared.name,
        name = quote_str(&declared.name),
        doc = quote_str(&declared.doc),
        members = members,
        writes = writes,
        reads = reads,
    )
}

/// The enums a module declares, as `#[dagger::object(enums(...))]` named them.
///
/// This is the whole of what tells a signature's `TargetOs` from a `Directory`:
/// the macro has no schema and no reflection, so a type name is an engine
/// object unless the module said otherwise here.
#[derive(Default)]
struct Enums {
    /// The paths as the attribute spelled them, in the order it listed them.
    paths: Vec<String>,
}

impl Enums {
    /// Whether a type named in a signature is one of them.
    ///
    /// Matched on the last path segment, the way an object's name is: how the
    /// attribute and the signature each spell the path is the user's business,
    /// and `crate::TargetOs` and `TargetOs` are one type.
    fn declares(&self, ty: &str) -> bool {
        self.paths.iter().any(|p| last_segment(p) == last_segment(ty))
    }
}

/// Read the `enums(...)` list off `#[dagger::object(...)]`.
fn enums_of(attr: TokenStream) -> Result<Enums, String> {
    let tokens: Vec<TokenTree> = attr.into_iter().collect();
    let mut enums = Enums::default();

    for part in split_commas(&tokens) {
        let key = match part.first() {
            Some(TokenTree::Ident(id)) if part.len() == 2 => id.to_string(),
            _ => {
                return Err(format!(
                    "unrecognized option `{}` on #[dagger::object]; it accepts `enums(...)`",
                    render(&part)
                ))
            }
        };
        let listed = match (&key[..], &part[1]) {
            ("enums", TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                g.stream().into_iter().collect::<Vec<TokenTree>>()
            }
            _ => {
                return Err(format!(
                    "unrecognized option `{}` on #[dagger::object]; it accepts `enums(...)`",
                    render(&part)
                ))
            }
        };

        for listed in split_commas(&listed) {
            let path = render(&listed);
            if !is_object_name(&path) {
                return Err(format!(
                    "`{path}` in `enums(...)` is not a type name; list the enums the module declares with `#[dagger::enum_type]`"
                ));
            }
            enums.paths.push(path);
        }
    }

    Ok(enums)
}

/// Options read from the marker attribute itself, `#[dagger::function(...)]`.
#[derive(Default)]
struct FunctionOptions {
    generate: bool,
    deprecated: String,
}

/// Read the options a function's marker attribute carried.
///
/// These are the flags Go writes as `+` pragmas in a doc comment — one slot,
/// several markers — rather than the per-argument `#[dagger(...)]` options.
///
/// `deprecated = "..."` is spelled here exactly as it is on a parameter, since
/// it is the same option one level up; the difference is only which attribute
/// carries it, because a method has no `#[dagger(...)]` of its own.
fn function_options_of(f: &Function) -> Result<FunctionOptions, String> {
    let mut options = FunctionOptions::default();
    for part in split_commas(&f.options) {
        match part.first() {
            Some(TokenTree::Ident(id)) if part.len() == 1 && id.to_string() == "generate" => {
                options.generate = true
            }
            Some(TokenTree::Ident(id)) if id.to_string() == "deprecated" => {
                // Skip the `=`, the way `options_of` does for a parameter.
                let value = &part[1..];
                let value = match value.first() {
                    Some(TokenTree::Punct(p)) if p.as_char() == '=' => &value[1..],
                    _ => value,
                };
                options.deprecated = literal_text(value, "deprecated")?;
            }
            _ => {
                return Err(format!(
                    "unrecognized option `{}` on `{}`; #[dagger::function(...)] accepts `generate` and `deprecated = \"...\"`",
                    render(&part),
                    f.name
                ))
            }
        }
    }
    Ok(options)
}

/// Options read from `#[dagger(...)]` on a struct field.
#[derive(Default)]
struct FieldOptions {
    deprecated: String,
}

/// Read the `#[dagger(...)]` options attached to a field.
///
/// Far fewer than a parameter's, and deliberately: a field has a `///` comment
/// of its own, so `doc` would be a second way to say the same thing, and it is
/// never an *input* to a call, so `default`, `default_path` and `ignore` have
/// nothing to apply to. Each is refused by name rather than ignored.
fn field_options_of(field: &parse::StructField) -> Result<FieldOptions, String> {
    let mut options = FieldOptions::default();
    for attr in field.attrs.iter().filter(|a| a.path == "dagger") {
        for part in split_commas(&attr.args) {
            let key = match part.first() {
                Some(TokenTree::Ident(id)) => id.to_string(),
                _ => {
                    return Err(format!(
                        "unrecognized #[dagger(...)] entry on `{}`: {}",
                        field.name,
                        render(&part)
                    ))
                }
            };
            let value = &part[1..];
            // Skip the `=`, the way `options_of` does.
            let value = match value.first() {
                Some(TokenTree::Punct(p)) if p.as_char() == '=' => &value[1..],
                _ => value,
            };

            match key.as_str() {
                "deprecated" => options.deprecated = literal_text(value, &key)?,
                "doc" => {
                    return Err(format!(
                        "`{}` is a field, so write its description as a `///` comment rather than as `doc`",
                        field.name
                    ))
                }
                other => {
                    return Err(format!(
                        "unknown #[dagger(...)] option `{other}` on the field `{}`; a field takes only `deprecated`, since it is state rather than an argument",
                        field.name
                    ))
                }
            }
        }
    }
    Ok(options)
}

/// Options read from `#[dagger(...)]` on a parameter.
#[derive(Default)]
struct Options {
    doc: String,
    default_value: String,
    default_path: String,
    ignore: Vec<String>,
    deprecated: String,
}

/// Read the `#[dagger(...)]` options attached to a parameter.
fn options_of(attrs: &[Attr]) -> Result<Options, String> {
    let mut options = Options::default();
    for attr in attrs.iter().filter(|a| a.path == "dagger") {
        for part in split_commas(&attr.args) {
            let key = match part.first() {
                Some(TokenTree::Ident(id)) => id.to_string(),
                _ => return Err(format!("unrecognized #[dagger(...)] entry: {}", render(&part))),
            };
            let value = &part[1..];
            // Skip the `=`.
            let value = match value.first() {
                Some(TokenTree::Punct(p)) if p.as_char() == '=' => &value[1..],
                _ => value,
            };

            match key.as_str() {
                "doc" => options.doc = literal_text(value, &key)?,
                "default" => options.default_value = json_literal(value, &key)?,
                "default_path" => options.default_path = literal_text(value, &key)?,
                "deprecated" => options.deprecated = literal_text(value, &key)?,
                "ignore" => options.ignore = string_list(value, &key)?,
                other => {
                    return Err(format!(
                        "unknown #[dagger(...)] option `{other}`; expected one of doc, default, default_path, ignore, deprecated"
                    ))
                }
            }
        }
    }
    Ok(options)
}

/// The text of a string-literal option.
fn literal_text(value: &[TokenTree], key: &str) -> Result<String, String> {
    match value.first() {
        Some(TokenTree::Literal(l)) => Ok(unquote(&l.to_string())),
        _ => Err(format!("`{key}` expects a string literal")),
    }
}

/// An option rendered as JSON, which is what `defaultValue` takes.
///
/// String literals are re-quoted as JSON strings; numbers and booleans pass
/// through as they are already valid JSON.
fn json_literal(value: &[TokenTree], key: &str) -> Result<String, String> {
    match value.first() {
        Some(TokenTree::Literal(l)) => {
            let raw = l.to_string();
            if raw.starts_with('"') {
                Ok(json_string(&unquote(&raw)))
            } else {
                Ok(raw)
            }
        }
        Some(TokenTree::Ident(id)) if id.to_string() == "true" || id.to_string() == "false" => {
            Ok(id.to_string())
        }
        _ => Err(format!("`{key}` expects a literal")),
    }
}

/// A `["a", "b"]` option.
fn string_list(value: &[TokenTree], key: &str) -> Result<Vec<String>, String> {
    match value.first() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Bracket => {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            split_commas(&inner)
                .iter()
                .map(|part| literal_text(part, key))
                .collect()
        }
        _ => Err(format!("`{key}` expects a list of string literals")),
    }
}

/// Escape a Rust string as a JSON string, quotes included.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Rust type mapped onto the engine's TypeDefKind, plus its optionality.
///
/// For a list, [`kind`](Kind::kind) and [`object`](Kind::object) describe the
/// *element* and [`optional`](Kind::optional) describes the list itself: the
/// engine models a list as a `TypeDef` of `LIST_KIND` wrapping an element
/// `TypeDef`, and `withOptional` applies to whichever of the two it is called
/// on. So `Option<slice<string>>` is an optional list of strings, and that is
/// the only place an `Option` may appear — see [`kind_of`].
struct Kind {
    kind: &'static str,
    /// The engine's name for an `OBJECT_KIND` or an `ENUM_KIND`; empty for a
    /// scalar. Owned rather than `&'static str` because it comes from the
    /// signature: any object the engine knows can be named, and an enum's name
    /// is the module's own, so the set is neither fixed nor this crate's.
    type_name: String,
    optional: bool,
    /// Whether the value is a list of `kind`, from a `slice<T>` or a `Vec<T>`.
    list: bool,
    /// The accessor on `Arguments` that yields this type.
    getter: &'static str,
}

impl Kind {
    /// Whether this is the engine object called `name`.
    ///
    /// The kind is part of the question rather than the name alone: an enum a
    /// module declares is named from the same namespace, so a module with an
    /// enum called `Workspace` would otherwise satisfy the rules below about
    /// the engine object of that name.
    fn is_object(&self, name: &str) -> bool {
        self.kind == "OBJECT_KIND" && self.type_name == name
    }
}

/// Strip one `Option<...>` layer, returning the type it wrapped.
fn unwrap_option(ty: &str) -> Option<&str> {
    ty.trim()
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map(|inner| inner.trim())
}

/// Strip one `slice<...>` or `Vec<...>` layer, returning the element type.
///
/// Both spellings name the same thing to the engine — a list of the element —
/// so both are mapped, the latitude `kind_of` already gives `String` and `&str`
/// alongside `string`. What the dispatch hands the function is goish's
/// `slice<T>`, so a `Vec<T>` parameter still has to become one; what accepting
/// the spelling buys is that the message its author gets is about that and not
/// about lists being unsupported.
///
/// The wrapper may be spelled any way that resolves — `slice`, `goish::slice`,
/// `alloc::vec::Vec` — so it is the last segment before the `<` that has to say
/// so, the way [`split_result`] reads `Result`.
fn unwrap_list(ty: &str) -> Option<&str> {
    let inner = ty.trim().strip_suffix('>')?;
    let open = inner.find('<')?;
    match last_segment(&inner[..open]) {
        "slice" | "Vec" => Some(inner[open + 1..].trim()),
        _ => None,
    }
}

/// The last segment of a path, so `dagger::Changeset` reads as `Changeset`.
fn last_segment(ty: &str) -> &str {
    ty.rsplit("::").next().unwrap_or(ty).trim()
}

/// Split `Result<T, E>` into the two types it names.
///
/// The path in front may be spelled any way that resolves — `Result`,
/// `core::result::Result` — so it is the last segment before the `<` that has to
/// say `Result`. The split is on the first comma at depth zero, so an ok type
/// that is itself generic (`Result<Option<string>, string>`) survives it.
///
/// `None` when the type is not a `Result` at all, which is the ordinary,
/// infallible case.
fn split_result(ty: &str) -> Option<(&str, &str)> {
    let inner = ty.trim().strip_suffix('>')?;
    let open = inner.find('<')?;
    if last_segment(&inner[..open]) != "Result" {
        return None;
    }
    let inner = &inner[open + 1..];

    let mut depth = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            // `Result<T>` with no error type never reaches here; the caller
            // reports the whole type rather than half of it.
            ',' if depth == 0 => return Some((inner[..i].trim(), inner[i + 1..].trim())),
            _ => {}
        }
    }
    Some((inner.trim(), ""))
}

/// How a function fails, if it can.
///
/// Both fallible shapes end at the same place — the message [`Object::invoke`]
/// hands back and the engine prints — so what this picks is how the error
/// spells that message.
///
/// [`Object::invoke`]: ../dagger/trait.Object.html#tymethod.invoke
enum Failure {
    /// Infallible: the function returns its value directly.
    None,
    /// `Result<T, string>`: the error already is the message. This is what
    /// every client method fails with, so it is what `?` propagates.
    Message,
    /// `Result<T, goish::error>`: goish's own APIs fail with this, and the
    /// message is `Error()` — through a helper, since that method panics on a
    /// nil error.
    GoError,
}

/// What a function returns, with the `Result` — if it wrote one — peeled off.
///
/// A function may return its value directly or fallibly, and everything that
/// looks at a return type wants the value either way: the engine is told what
/// the function *produces*, and a failure is not a kind it has. The [`Failure`]
/// is what the dispatch needs on top of that — how to turn what the call may
/// return into the message it hands back.
///
/// The error is goish's `string` or its `error`, and nothing else: there is no
/// way to get a message out of an arbitrary type — goish has no `Display`, and
/// its `error` is the interface everything that carries a message implements.
/// So anything else is refused here, rather than as a `From` error inside the
/// macro's own output.
fn return_type(f: &Function) -> Result<(&str, Failure), String> {
    let declared = f.return_ty.trim();
    let (ok, err) = match split_result(declared) {
        Some(split) => split,
        None => return Ok((declared, Failure::None)),
    };
    match last_segment(err) {
        "string" => Ok((ok, Failure::Message)),
        "error" => Ok((ok, Failure::GoError)),
        _ => Err(format!(
            "`{}` returns `{}`; a fallible function fails with the message the engine shows, so its error is goish's `string` or its `error` — write `Result<{}, string>`",
            f.name,
            declared,
            if ok.is_empty() { "T" } else { ok },
        )),
    }
}

/// Whether a type names an engine object.
///
/// There is nothing to look the name up in — the schema belongs to the engine
/// the module will run against, not to this crate — so the shape of the name is
/// the whole test: a path whose last segment is a Rust type name. `Directory`,
/// `gen::Container` and `dagger::gen::Workspace` all pass; a generic, a
/// reference or a slice does not, since none of those is a thing the engine has
/// an ID for.
fn is_object_name(ty: &str) -> bool {
    // A wrapper's own punctuation, kept out before the last segment is read: it
    // is `Vec<Directory>` that has to be rejected, and its last segment alone
    // would look like a perfectly good object.
    if ty.contains(['<', '>', '&', '[', ']', '(', ')', ',']) {
        return false;
    }
    let mut chars = last_segment(ty).chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Map a Rust type to a TypeDefKind.
///
/// Scalars are a fixed set. Everything else that looks like a type name is an
/// engine object — unless the module declared an enum of that name, which is
/// what `enums` carries, since nothing about the two spellings differs. Either
/// way it is named to the engine exactly as the last segment of the path spells
/// it.
///
/// That is as far as this can check an object: its *existence* is the engine's
/// to know, and a name it does not have is reported when the module registers.
/// What the name has to satisfy here is that a type of that name implements
/// [`ObjectId`], which the generated bindings do for every object the engine has
/// a loader for — so a misspelled one, or an enum left out of the `enums` list,
/// fails to compile naming the trait.
///
/// [`ObjectId`]: ../dagger/trait.ObjectId.html
///
/// # Lists, and how far they go
///
/// A `slice<T>` or a `Vec<T>` is a list of `T`, and `T` is anything above: a
/// list of strings, of ints, of floats, of bools, or of engine objects. **One
/// level, and no further.** The engine's model would carry more — a `TypeDef` of
/// `LIST_KIND` can wrap any other `TypeDef`, including another list — but
/// nothing else here would: [`Arguments`] decodes a JSON array of scalars or of
/// ids, and the encoders take a flat list. The Dagger schema has no nested list
/// anywhere either, so `slice<slice<T>>` is refused by name rather than
/// registered as something a call would then fail on.
///
/// `Option<slice<T>>` is an optional list: `withOptional` applies to the list
/// the same way it applies to a scalar, so the caller may leave the whole
/// argument out. `slice<Option<T>>` — a list whose *elements* may be null — is
/// refused: the engine can express it, but a null element has no Rust shape
/// here that a dispatch could hand the function, and no Dagger API returns one.
///
/// [`Arguments`]: ../dagger/struct.Arguments.html
fn kind_of(ty: &str, enums: &Enums) -> Result<Kind, String> {
    let trimmed = ty.trim();
    if let Some(inner) = unwrap_option(trimmed) {
        let mut inner = kind_of(inner, enums)?;
        // `Option<()>` is the one wrapping that says nothing: Void already *is*
        // the absence of a value, and the engine has no way to tell an optional
        // Void from a Void. It would encode as `null` either way, so the two
        // spellings would differ only in the typedef, and refusing it here is
        // clearer than declaring a distinction that does not exist.
        if inner.kind == "VOID_KIND" {
            return Err(format!(
                "`{trimmed}` is not a supported type: a Void is the absence of a value already, so write `()`"
            ));
        }
        inner.optional = true;
        return Ok(inner);
    }
    if let Some(element) = unwrap_list(trimmed) {
        let element = kind_of(element, enums)?;
        if element.list {
            return Err(format!(
                "unsupported type `{trimmed}`: a list goes one level deep, so `slice<T>` of a scalar or of an engine object — a list of lists is not something the module protocol carries"
            ));
        }
        if element.optional {
            return Err(format!(
                "unsupported type `{trimmed}`: the elements of a list are not optional, so write `Option<slice<T>>` for a list the caller may leave out"
            ));
        }
        // Every element kind has an accessor and an encoder except Void, which
        // is the absence of a value: `slice<()>` is a list of nothings.
        let getter = match element.kind {
            "STRING_KIND" => "string_list",
            "INTEGER_KIND" => "int_list",
            "FLOAT_KIND" => "float_list",
            "BOOLEAN_KIND" => "bool_list",
            "OBJECT_KIND" => "object_list",
            _ => {
                return Err(format!(
                    "unsupported type `{trimmed}`: a list is of string, int, float, bool, or an engine object"
                ))
            }
        };
        return Ok(Kind {
            kind: element.kind,
            type_name: element.type_name,
            optional: false,
            list: true,
            getter,
        });
    }
    let (kind, getter) = match trimmed {
        "string" | "String" | "&str" => ("STRING_KIND", "string"),
        "int" | "i64" | "i32" | "isize" | "usize" => ("INTEGER_KIND", "int"),
        // The 64-bit spelling only, and deliberately: there is no `f32` on the
        // wire. GraphQL's Float is a double, goish models it as Go's `float64`,
        // and the accessor and encoder either side of this arm both speak
        // `f64`. Accepting `f32` would mean a lossy cast the author never asked
        // for, or — as it did before — an `E0308` raised inside the macro's own
        // expansion, which is the worst place for an error to surface. `f32`
        // falls through to the ordinary "unsupported type" message instead.
        "float64" | "f64" => ("FLOAT_KIND", "float"),
        "bool" => ("BOOLEAN_KIND", "bool"),
        "" | "()" => ("VOID_KIND", "void"),
        other => {
            // Written as `Directory` or as `gen::Directory`; both name the same
            // object as far as the engine is concerned, and the same goes for a
            // declared enum.
            if !is_object_name(other) {
                return Err(format!(
                    "unsupported type `{other}`: a function's arguments and return are string, int, float, bool, an engine object named as a plain type — `Directory`, `Container`, `Workspace`, an enum the module declares, or a `slice<T>` of one. Other generics are not supported"
                ));
            }
            let (kind, getter) = if enums.declares(other) {
                ("ENUM_KIND", "enum_member")
            } else {
                ("OBJECT_KIND", "object")
            };
            return Ok(Kind {
                kind,
                type_name: last_segment(other).to_string(),
                optional: false,
                list: false,
                getter,
            });
        }
    };
    Ok(Kind { kind, type_name: String::new(), optional: false, list: false, getter })
}

/// `container_echo` -> `containerEcho`, matching the API's naming.
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for c in name.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Build the impl the annotated item calls for.
///
/// An object is two items — a `struct` holding its state and an `impl` holding
/// its functions — and an attribute macro sees only the one it is written on.
/// So `#[dagger::object]` goes on both and emits a different half from each:
/// `ObjectState` from the `struct`, `Object` from the `impl`.
///
/// Both halves are handed the `enums(...)` list, because a field may name a
/// module's enum as readily as an argument may. That means writing the list
/// twice for a module that has both, which is the price of the attribute
/// seeing one item at a time.
fn expand(attr: TokenStream, item: TokenStream) -> Result<TokenStream, String> {
    let enums = enums_of(attr)?;

    let generated = match parse::item_keyword(&item).as_str() {
        "struct" => state_impl(&parse::parse_struct(item)?, &enums)?,
        // `check` and `constructor` are markers of their own rather than
        // options on `function`, so a method carrying only one of them is still
        // read.
        "impl" => object_impl(
            &parse::parse_impl(item, &["function", "check", "constructor"])?,
            &enums,
        )?,
        other => {
            return Err(format!(
                "#[dagger::object] applies to a `struct` or an `impl` block, not to `{other}`"
            ))
        }
    };

    generated
        .parse()
        .map_err(|e| format!("generated code did not parse: {e}"))
}

/// Render the `ObjectState` impl for one `struct` declaration.
///
/// The three halves of a field's life, from one declaration: the [`FieldDef`]
/// the engine is told about, the read that rebuilds it out of the parent
/// document, and the write that puts it back.
///
/// [`FieldDef`]: ../dagger/struct.FieldDef.html
fn state_impl(def: &parse::StructDef, enums: &Enums) -> Result<String, String> {
    let type_name = &def.type_name;

    let mut defs = String::new();
    let mut reads = String::new();
    let mut writes = String::new();

    for field in &def.fields {
        // A private field would be state the engine never sees, and so state
        // that does not survive the call it was set in — the very thing an
        // object carrying data is for. Rather than drop it silently the way Go
        // drops an unexported field, say so.
        if !field.is_pub {
            return Err(format!(
                "`{}.{}` is private, so the engine would neither declare it nor carry it from one call to the next; make it `pub`, or move it out of the object",
                type_name, field.name
            ));
        }

        let options = field_options_of(field)?;
        let kind = kind_of(&field.ty, enums)?;
        if kind.kind == "VOID_KIND" {
            return Err(format!(
                "`{}.{}` has no type the engine can carry",
                type_name, field.name
            ));
        }
        // A field of the object's own type would need the type to contain
        // itself, which does not compile — but it reaches here as an ordinary
        // object name, so the error would be about `ObjectId` rather than about
        // the field.
        if kind.is_object(type_name) {
            return Err(format!(
                "`{}.{}` is the object's own type, which cannot be one of its fields",
                type_name, field.name
            ));
        }

        let api_name = camel_case(&field.name);

        defs.push_str(&format!(
            "::dagger::__internal::FieldDef {{ name: {name}, kind: {kind}, type_name: {type_of}, list: {list}, optional: {optional}, doc: {doc}, deprecated: {deprecated}, source: {source} }},",
            name = quote_str(&api_name),
            kind = quote_str(kind.kind),
            type_of = quote_str(&kind.type_name),
            list = kind.list,
            optional = kind.optional,
            doc = quote_str(&field.doc),
            deprecated = quote_str(&options.deprecated),
            source = source_map_def(&field.source),
        ));

        reads.push_str(&format!(
            "{name}: {value},",
            name = field.name,
            value = read_value("state", &api_name, &field.ty, &kind),
        ));

        writes.push_str(&format!(
            "__state.put({name}, {value});",
            name = quote_str(&api_name),
            value = write_value(&field.name, &kind)?,
        ));
    }

    // The two `allow`s are both about the unit case, which every scaffolded
    // module starts in: with no fields there is nothing to read the state for
    // and nothing to write to the builder, and a warning in either would land
    // in the user's build rather than here.
    Ok(format!(
        r#"
impl ::dagger::__internal::ObjectState for {type_name} {{
    fn fields() -> &'static [::dagger::__internal::FieldDef] {{
        const FIELDS: &[::dagger::__internal::FieldDef] = &[{defs}];
        FIELDS
    }}

    #[allow(unused_variables)]
    fn from_state(
        state: &::dagger::__internal::State,
    ) -> ::core::result::Result<{type_name}, ::goish::gostring::string> {{
        ::core::result::Result::Ok({type_name} {{ {reads} }})
    }}

    #[allow(unused_mut)]
    fn to_state(&self) -> ::core::result::Result<::goish::gostring::string, ::goish::gostring::string> {{
        let mut __state = ::dagger::__internal::StateWriter::new();
        {writes}
        ::core::result::Result::Ok(__state.finish())
    }}
}}
"#,
        type_name = type_name,
        defs = defs,
        reads = reads,
        writes = writes,
    ))
}

/// Render the `Object` impl for one parsed block, given the enums it declares.
///
/// Split out of [`expand`] so the crate's own tests can reach it: everything
/// above it speaks in `TokenTree`, which a test binary may not touch, and
/// everything below it speaks in `String`.
fn object_impl(block: &parse::ImplBlock, enums: &Enums) -> Result<String, String> {
    let type_name = &block.type_name;

    let mut defs = String::new();
    let mut arms = String::new();
    let mut constructor: Option<&Function> = None;

    for f in &block.functions {
        if f.has_marker("constructor") {
            if f.has_marker("function") || f.has_marker("check") {
                return Err(format!(
                    "`{}` is both a constructor and a function; a constructor is reached as the module's own arguments rather than by name, so it can only be one",
                    f.name
                ));
            }
            if let Some(first) = constructor {
                return Err(format!(
                    "`{}` declares a second constructor; `{}` is already this object's, and the engine registers exactly one",
                    f.name, first.name
                ));
            }
            constructor = Some(f);
            continue;
        }
        defs.push_str(&function_def(f, enums)?);
        arms.push_str(&dispatch_arm(type_name, f, enums)?);
    }

    let enum_defs = enum_defs(enums);
    let constructor = match constructor {
        Some(f) => constructor_impl(type_name, f, enums)?,
        None => String::new(),
    };

    Ok(format!(
        r#"
impl ::dagger::__internal::Object for {type_name} {{
    const NAME: &'static str = {name_literal};

    const DOC: &'static str = {doc_literal};

    const SOURCE: ::dagger::__internal::SourceMapDef = {source};

    fn functions() -> &'static [::dagger::__internal::FunctionDef] {{
        const FUNCTIONS: &[::dagger::__internal::FunctionDef] = &[{defs}];
        FUNCTIONS
    }}
{enum_defs}{constructor}
    #[allow(unused_variables)]
    fn invoke(
        self,
        name: &::goish::gostring::string,
        args: &::dagger::__internal::Arguments,
    ) -> ::core::result::Result<::goish::gostring::string, ::goish::gostring::string> {{
        {arms}
        ::core::result::Result::Err(
            ::goish::convert::string("no such function: ") + name.clone(),
        )
    }}
}}
"#,
        type_name = type_name,
        name_literal = quote_str(type_name),
        doc_literal = quote_str(&block.doc),
        source = source_map_def(&block.source),
        defs = defs,
        enum_defs = enum_defs,
        constructor = constructor,
        arms = arms,
    ))
}

/// Render the two `Object` methods a declared constructor overrides.
///
/// The engine knows a constructor by having no name at all, which is why the
/// `FunctionDef` here is built with an empty one rather than with the Rust
/// function's: `new` is a Rust convention, and the API has no such function.
fn constructor_impl(type_name: &str, f: &Function, enums: &Enums) -> Result<String, String> {
    if f.takes_self {
        return Err(format!(
            "`{}` is a constructor, so it builds the object rather than being called on one; drop the `self` parameter",
            f.name
        ));
    }

    let (returns, failure) = return_type(f)?;
    let ret = kind_of(returns, enums)?;
    if !ret.is_object(type_name) || ret.optional || ret.list {
        return Err(format!(
            "`{}` is a constructor, so it must return `{}`, but it returns `{}`",
            f.name,
            type_name,
            if f.return_ty.is_empty() { "()" } else { &f.return_ty }
        ));
    }

    let options = function_options_of(f)?;
    if options.generate {
        return Err(format!(
            "`{}` is a constructor, so `dagger generate` has nothing to run it for",
            f.name
        ));
    }

    let (bindings, call) = call_expr(&format!("{type_name}::"), f, enums)?;

    Ok(format!(
        r#"
    fn constructor() -> ::core::option::Option<&'static ::dagger::__internal::FunctionDef> {{
        const CONSTRUCTOR: ::dagger::__internal::FunctionDef = {def}
        ::core::option::Option::Some(&CONSTRUCTOR)
    }}

    #[allow(unused_variables)]
    fn construct(
        args: &::dagger::__internal::Arguments,
    ) -> ::core::result::Result<::goish::gostring::string, ::goish::gostring::string> {{
        {bindings}
        ::dagger::__internal::ObjectState::to_state(&{call})
    }}
"#,
        // The rendered def ends in a trailing comma, which is what a table
        // entry wants and a `const` initializer does not; a `;` after it is the
        // one place that matters.
        def = function_def_named(f, "", &options, enums)?.trim_end_matches(',').to_string() + ";",
        bindings = bindings,
        call = wrap_failure(call, &failure),
    ))
}

/// Render one `SourceMapDef`.
///
/// A location the parser had no span for becomes `UNKNOWN` rather than a
/// literal with a zero line, so `register` can tell "no source map" from "line
/// zero" without inspecting the numbers.
fn source_map_def(loc: &SourceLoc) -> String {
    if loc.file.is_empty() {
        return "::dagger::__internal::SourceMapDef::UNKNOWN".to_string();
    }
    format!(
        "::dagger::__internal::SourceMapDef {{ file: {file}, line: {line}, column: {column} }}",
        file = quote_str(&loc.file),
        line = loc.line,
        column = loc.column,
    )
}

/// Render `Object::enums`, the list `register` walks to declare them.
///
/// Each enum contributes the `EnumDef` its own attribute emitted, rather than
/// anything read here: this side knows a path, and what the members are is
/// `#[dagger::enum_type]`'s to say. Empty when the module declares none, so
/// what it emits is what it emitted before enums existed and the trait's own
/// default stands.
fn enum_defs(enums: &Enums) -> String {
    if enums.paths.is_empty() {
        return String::new();
    }
    let listed = enums
        .paths
        .iter()
        .map(|path| format!("<{path} as ::dagger::__internal::EnumType>::DEF,"))
        .collect::<String>();
    format!(
        r#"
    fn enums() -> &'static [::dagger::__internal::EnumDef] {{
        const ENUMS: &[::dagger::__internal::EnumDef] = &[{listed}];
        ENUMS
    }}
"#
    )
}

/// Whether a return satisfies what `dagger generate` applies.
///
/// A generator hands back the changes it made, so the return is the engine's
/// `Changeset` and neither an optional one nor a list of them: "maybe some
/// changes" and "several sets of changes" are not shapes generation can apply,
/// and now that `Option<T>` and `slice<T>` returns are otherwise supported,
/// either would be declared to the engine rather than refused. `is_object`
/// rather than a name comparison, so a module's own enum called `Changeset`
/// cannot satisfy it either.
///
/// A function of its own so the rule can be tested: whether a method *is* a
/// generator is read off the tokens in `#[dagger::function(generate)]`, and
/// tokens cannot be built outside a macro expansion.
fn is_generator_return(ret: &Kind) -> bool {
    ret.is_object("Changeset") && !ret.optional && !ret.list
}

/// Render one `FunctionDef`.
fn function_def(f: &Function, enums: &Enums) -> Result<String, String> {
    let function_options = function_options_of(f)?;
    function_def_with(f, &function_options, enums)
}

/// Render one `FunctionDef` from options already read.
///
/// The split is what makes the marker attribute's options testable: reading
/// them means walking a `TokenTree`, which a test binary may not do, but the
/// text they turn into is ordinary `String` work.
fn function_def_with(
    f: &Function,
    function_options: &FunctionOptions,
    enums: &Enums,
) -> Result<String, String> {
    function_def_named(f, &camel_case(&f.name), function_options, enums)
}

/// Render one `FunctionDef` under a given API name.
///
/// The name is a parameter because a constructor has none: the engine spells
/// "this function builds the object" as the empty name, so everything else
/// about it — its arguments, their defaults, its source map — is built exactly
/// as any other function's is.
fn function_def_named(
    f: &Function,
    api_name: &str,
    function_options: &FunctionOptions,
    enums: &Enums,
) -> Result<String, String> {
    let mut args = String::new();
    for param in &f.params {
        let options = options_of(&param.attrs)?;
        let kind = kind_of(&param.ty, enums)?;

        // The engine takes these on a contextual argument only — "can only set
        // default path for Object, not STRING_KIND" — and it says so at module
        // load, which is a bad place to learn about a typo. The two types it
        // means are known here, so the message can name the parameter instead.
        if !options.default_path.is_empty() || !options.ignore.is_empty() {
            let which = if options.default_path.is_empty() { "ignore" } else { "default_path" };
            // A list of them is no good either: the engine loads one contextual
            // value per argument, so `slice<Directory>` has nowhere to put it.
            if kind.list || (!kind.is_object("Directory") && !kind.is_object("File")) {
                return Err(format!(
                    "`{which}` on `{}` applies only to Directory and File arguments, and `{}` is `{}`",
                    param.name, param.name, param.ty
                ));
            }
        }
        // A defaulted argument is optional whether or not it is an Option<T>:
        // the caller may leave it out and get the default.
        let optional = kind.optional || !options.default_value.is_empty();

        // A `default_path` argument is not *optional* — the typedef says what
        // the signature says — but the caller can still leave it out, because
        // the engine loads it from the context directory. So it satisfies the
        // two "callable with nothing" rules below without being declared
        // optional to the engine.
        let contextual = !options.default_path.is_empty();

        // `dagger check` runs a check with no arguments. The engine's answer to
        // a check that cannot be run is to leave it out of the check tree, so it
        // would just never appear — catch it here, where the message can say why.
        if f.has_marker("check") && !optional && !contextual {
            return Err(format!(
                "`{}` is a check, so it is run with no arguments, but `{}` is required; give it a `#[dagger(default = ...)]` or make it an `Option<T>`",
                f.name, param.name
            ));
        }

        // The same holds for a generator, which the engine runs with nothing to
        // pass it: it refuses to load a module whose generator has a required
        // argument. A Workspace is the exception — the engine supplies that one
        // itself. Checking here turns a module that fails to load into a
        // signature that fails to compile, naming the argument.
        if function_options.generate && !optional && !contextual && !kind.is_object("Workspace") {
            return Err(format!(
                "`{}` is a generate function, so it must be callable with no arguments, but `{}` is required; make it an Option<T> or give it a `default`",
                f.name, param.name
            ));
        }
        let ignore = options
            .ignore
            .iter()
            .map(|p| quote_str(p))
            .collect::<Vec<_>>()
            .join(", ");

        args.push_str(&format!(
            "::dagger::__internal::ArgDef {{ name: {name}, kind: {kind}, type_name: {type_name}, list: {list}, optional: {optional}, doc: {doc}, default_value: {default}, default_path: {path}, ignore: &[{ignore}], deprecated: {deprecated}, source: {source} }},",
            name = quote_str(&camel_case(&param.name)),
            kind = quote_str(kind.kind),
            type_name = quote_str(&kind.type_name),
            list = kind.list,
            optional = optional,
            doc = quote_str(&options.doc),
            default = quote_str(&options.default_value),
            path = quote_str(&options.default_path),
            ignore = ignore,
            deprecated = quote_str(&options.deprecated),
            source = source_map_def(&param.source),
        ));
    }

    // The engine is told what the function produces, so a fallible one is
    // declared by the type inside its `Result`: failure is not a kind.
    let (returns, _failure) = return_type(f)?;
    let ret = kind_of(returns, enums)?;

    // The other half of the generator contract: `dagger generate` applies what
    // the function returns, so a generator returns the changes it made —
    // directly, or as `Result<Changeset, string>`.
    if function_options.generate && !is_generator_return(&ret) {
        return Err(format!(
            "`{}` is a generate function, so it must return `Changeset`, but it returns `{}`",
            f.name,
            if f.return_ty.is_empty() { "()" } else { &f.return_ty }
        ));
    }

    Ok(format!(
        "::dagger::__internal::FunctionDef {{ name: {name}, doc: {doc}, return_kind: {ret}, return_type_name: {ret_type_name}, return_list: {ret_list}, return_optional: {ret_optional}, is_check: {is_check}, generator: {generator}, deprecated: {deprecated}, source: {source}, args: &[{args}] }},",
        name = quote_str(api_name),
        doc = quote_str(&f.doc),
        ret = quote_str(ret.kind),
        ret_type_name = quote_str(&ret.type_name),
        ret_list = ret.list,
        ret_optional = ret.optional,
        is_check = f.has_marker("check"),
        generator = function_options.generate,
        deprecated = quote_str(&function_options.deprecated),
        source = source_map_def(&f.source),
        args = args,
    ))
}

/// The expression that reads one value out of a carrier.
///
/// The same code for an argument of a call and for a field of the parent
/// object: `State` derefs to `Arguments`, so the two carry one accessor set and
/// what differs between them is the name of the local they are reached through.
///
/// An object arrives as its ID and an enum as one of its member names, so
/// neither is used as it lands: both are rebuilt through the trait that knows
/// how. A list of objects arrives as a list of IDs and is rebuilt element by
/// element, which is `from_ids` — the same trait, once per element. Naming the
/// type the way the declaration does means it resolves in the user's scope,
/// whatever they imported it from.
fn read_value(carrier: &str, api_name: &str, ty: &str, kind: &Kind) -> String {
    let accessor = if kind.optional {
        format!("{}_opt", kind.getter)
    } else {
        kind.getter.to_string()
    };
    let read = format!(
        "{carrier}.{accessor}({name})?",
        carrier = carrier,
        accessor = accessor,
        name = quote_str(api_name),
    );
    let ty = unwrap_option(ty).unwrap_or(ty.trim());
    match kind.kind {
        "OBJECT_KIND" => {
            // Either way this has to name a *function*, not a call, so that an
            // optional can hand it to `.map(…)`. `from_ids` is a free function
            // and takes a turbofish; the trait method cannot, so it stays
            // qualified.
            let rebuild = match unwrap_list(ty) {
                Some(element) => format!("::dagger::__internal::from_ids::<{element}>"),
                None => format!("<{ty} as ::dagger::ObjectId>::from_id"),
            };
            if kind.optional {
                format!("{read}.map({rebuild})")
            } else {
                format!("{rebuild}({read})")
            }
        }
        // `from_member` is fallible where `from_id` is not, so an optional one
        // cannot be a `map`: that would leave a `Result` inside the `Option` for
        // the function to receive.
        "ENUM_KIND" => {
            let from_member = format!("<{ty} as ::dagger::__internal::EnumType>::from_member");
            if kind.optional {
                format!(
                    "match {read} {{ ::core::option::Option::Some(member) => ::core::option::Option::Some({from_member}(&member)?), ::core::option::Option::None => ::core::option::Option::None }}"
                )
            } else {
                format!("{from_member}(&{read})?")
            }
        }
        _ => read,
    }
}

/// The expression that encodes one field of `self` back into the state.
///
/// Reaches for the same `encode_*` functions the dispatch encodes a return
/// with, so a field and a return of one type make the same round trip — and
/// refuses the same kinds, since [`list_encoder`] is the one that decides what
/// a list of each element kind becomes.
///
/// What varies is only how the value is held: a string, an object, an enum or a
/// list is borrowed, a number or a flag is copied, and an `Option` is matched —
/// which puts a reference in hand either way, hence the deref on the copied
/// kinds.
fn write_value(name: &str, kind: &Kind) -> Result<String, String> {
    if kind.list {
        return Ok(if kind.optional {
            let encode = list_encoder(kind.kind, "*value")?;
            optional_write(name, &encode)
        } else {
            list_encoder(kind.kind, &format!("self.{name}"))?
        });
    }

    let (encoder, borrow, deref, question) = match kind.kind {
        "STRING_KIND" => ("::dagger::__internal::encode_string", "&", "", ""),
        "INTEGER_KIND" => ("::dagger::__internal::encode_int", "", "*", ""),
        "FLOAT_KIND" => ("::dagger::__internal::encode_float", "", "*", ""),
        "BOOLEAN_KIND" => ("::dagger::__internal::encode_bool", "", "*", ""),
        // Fallible, unlike the others: reading a generated object's id runs the
        // chain it was built from.
        "OBJECT_KIND" => ("::dagger::__internal::encode_object", "&", "", "?"),
        // An enum goes back as the member's name, which the value already
        // carries — nothing to fetch, so nothing to fail.
        "ENUM_KIND" => ("::dagger::__internal::encode_enum", "&", "", ""),
        other => return Err(format!("cannot encode a field of kind {other}")),
    };
    Ok(if kind.optional {
        optional_write(name, &format!("{encoder}({deref}value){question}"))
    } else {
        format!("{encoder}({borrow}self.{name}){question}")
    })
}

/// Wrap a field's encoder in the match an `Option` needs.
fn optional_write(name: &str, encode: &str) -> String {
    format!(
        "match &self.{name} {{ ::core::option::Option::Some(value) => {encode}, ::core::option::Option::None => ::dagger::__internal::encode_null() }}"
    )
}

/// The bindings that read a function's arguments, and the call that uses them.
fn call_expr(receiver: &str, f: &Function, enums: &Enums) -> Result<(String, String), String> {
    let mut bindings = String::new();
    let mut call_args = Vec::new();

    for param in &f.params {
        let kind = kind_of(&param.ty, enums)?;
        let value = read_value("args", &camel_case(&param.name), &param.ty, &kind);
        bindings.push_str(&format!("let {binding} = {value};", binding = param.name));
        call_args.push(param.name.clone());
    }

    Ok((
        bindings,
        format!("{receiver}{fname}({args})", fname = f.name, args = call_args.join(", ")),
    ))
}

/// Carry a fallible call's failure out as the message the engine shows.
///
/// The parentheses are what make either safe to interpolate: `?` binds tighter
/// than the `&` an encoder takes, so `&(call)?` borrows the value rather than
/// the `Result`.
fn wrap_failure(call: String, failure: &Failure) -> String {
    match failure {
        Failure::None => call,
        Failure::Message => format!("({call})?"),
        Failure::GoError => format!("({call}).map_err(::dagger::__internal::error_message)?"),
    }
}

/// How a returned list of `kind` is encoded.
///
/// One encoder per element kind, mirroring the scalars: the JSON a function
/// hands back is an array of what the element encoder would have written on its
/// own. The object one is fallible for the same reason `encode_object` is —
/// reading an object's id runs the chain it was built from — and it is fallible
/// once per element.
fn list_encoder(kind: &str, call: &str) -> Result<String, String> {
    Ok(match kind {
        "STRING_KIND" => format!("::dagger::__internal::encode_string_list(&{call})"),
        "INTEGER_KIND" => format!("::dagger::__internal::encode_int_list(&{call})"),
        "FLOAT_KIND" => format!("::dagger::__internal::encode_float_list(&{call})"),
        "BOOLEAN_KIND" => format!("::dagger::__internal::encode_bool_list(&{call})"),
        "OBJECT_KIND" => format!("::dagger::__internal::encode_object_list(&{call})?"),
        other => return Err(format!("cannot encode a returned list of kind {other}")),
    })
}

/// Render the `match` arm that calls one function and encodes its result.
fn dispatch_arm(type_name: &str, f: &Function, enums: &Enums) -> Result<String, String> {
    // The receiver is the object the engine's `parent` decoded to, taken by
    // value: a builder that consumes `self` is as ordinary a signature as one
    // that borrows it, and the receiver belongs to this one call either way.
    let receiver = if f.takes_self {
        "self.".to_string()
    } else {
        format!("{type_name}::")
    };
    let (bindings, call) = call_expr(&receiver, f, enums)?;
    let (returns, failure) = return_type(f)?;
    let ret = kind_of(returns, enums)?;

    // `invoke` returns `Result<string, string>`, so a function that failed with
    // a message needs nothing but `?` on the way past, and one that failed with
    // a goish `error` needs its message read off it first.
    let call = wrap_failure(call, &failure);

    // An `Option<T>` is encoded by the kind inside it, or as JSON null — the one
    // encoding the engine reads back as "no value", and what `withOptional` on
    // the return typedef promises it may get. `__value` cannot collide with
    // anything: `camel_case` never produces a leading underscore, so no name in
    // the user's signature reaches it.
    //
    // The `None` arm is why this is a `match` rather than a `map`: an
    // `Option<Directory>` that is `None` has no object to resolve an id for, so
    // it costs no round trip and cannot fail.
    //
    // A list composes rather than competing: `list_encoder` takes the
    // expression to encode, so an `Option<slice<T>>` hands it `__value` and
    // keeps the null. The list is tested *inside* this half deliberately — as a
    // guard in front of the whole match, the way the infallible half spells it,
    // it would take an optional list before the `Option` was ever unwrapped and
    // encode a `slice` that is still inside one.
    let encode = if ret.optional {
        let some = if ret.list {
            list_encoder(ret.kind, "__value")?
        } else {
            match ret.kind {
                "STRING_KIND" => "::dagger::__internal::encode_string(&__value)".to_string(),
                "INTEGER_KIND" => "::dagger::__internal::encode_int(__value)".to_string(),
                "FLOAT_KIND" => "::dagger::__internal::encode_float(__value)".to_string(),
                "BOOLEAN_KIND" => "::dagger::__internal::encode_bool(__value)".to_string(),
                // The object's own type goes back as its state here too: the
                // two halves of an `Option<Self>` are the same two answers as
                // any other optional return, and only the `Some` one has a
                // document to write.
                "OBJECT_KIND" if ret.is_object(type_name) => {
                    "::dagger::__internal::encode_state(&__value)?".to_string()
                }
                "OBJECT_KIND" => "::dagger::__internal::encode_object(&__value)?".to_string(),
                // The member's name is already in the value, so nothing is
                // fetched and nothing can fail — the `Some` arm needs no `?`.
                "ENUM_KIND" => "::dagger::__internal::encode_enum(&__value)".to_string(),
                other => {
                    return Err(format!("cannot encode an optional return of kind {other}"))
                }
            }
        };
        format!(
            "match {call} {{ ::core::option::Option::Some(__value) => {some}, ::core::option::Option::None => ::dagger::__internal::encode_null() }}"
        )
    } else {
        match ret.kind {
            // A list is encoded by its element kind, one encoder each, so this
            // is a guard in front of the scalar arms rather than four more of
            // them.
            _ if ret.list => list_encoder(ret.kind, &call)?,
            "VOID_KIND" => format!("{{ {call}; ::dagger::__internal::encode_void() }}"),
            "STRING_KIND" => format!("::dagger::__internal::encode_string(&{call})"),
            "INTEGER_KIND" => format!("::dagger::__internal::encode_int({call})"),
            "FLOAT_KIND" => format!("::dagger::__internal::encode_float({call})"),
            "BOOLEAN_KIND" => format!("::dagger::__internal::encode_bool({call})"),
            // The object's own type goes back as its state rather than as an
            // id: the engine holds no value to mint one for, so what it keeps
            // is the document the fields encode to. This is what lets a builder
            // return `Self` and the caller go on chaining from it.
            "OBJECT_KIND" if ret.is_object(type_name) => {
                format!("::dagger::__internal::encode_state(&{call})?")
            }
            // Fallible, unlike the others: reading a generated object's id runs
            // the chain the function built.
            "OBJECT_KIND" => format!("::dagger::__internal::encode_object(&{call})?"),
            // An enum goes back as the member's name, which the value already
            // carries — nothing to fetch, so nothing to fail.
            "ENUM_KIND" => format!("::dagger::__internal::encode_enum(&{call})"),
            other => return Err(format!("cannot encode a return of kind {other}")),
        }
    };

    // An if/else chain rather than a `match`: the name is a goish `string`,
    // which compares against a literal with `==` but cannot be a match scrutinee.
    Ok(format!(
        "if name == {name} {{ {bindings} return ::core::result::Result::Ok({encode}); }}",
        name = quote_str(&camel_case(&f.name)),
        bindings = bindings,
        encode = encode,
    ))
}
