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
//! impl MyModule {
//!     /// Build the project.
//!     #[dagger::function]
//!     pub fn build(
//!         &self,
//!         #[dagger(default = "alpine:3.21")] image: string,
//!         #[dagger(doc = "Tag to apply")] tag: Option<string>,
//!     ) -> string { … }
//! }
//! ```
//!
//! Argument options live in `#[dagger(...)]` on the parameter — `default`,
//! `default_path`, `ignore`, `doc`, `deprecated` — mirroring the Go SDK's
//! `+default`, `+defaultPath`, `+ignore` pragmas. Optionality is carried by
//! `Option<T>` rather than a marker, and a parameter with a `default` is
//! optional by construction.
//!
//! [`macro@check`] marks a function `dagger check` should run, the Go SDK's
//! `+check` pragma.
//!
//! What a function *is* otherwise goes in the marker attribute — `#[dagger::function(generate)]`
//! declares a generator, the way Go writes `+generate` in the doc comment.

extern crate proc_macro;

mod parse;
#[cfg(test)]
mod tests;

use parse::{quote_str, render, split_commas, unquote, Attr, Function};
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
/// # Options
///
/// What a function *is* goes in the attribute itself — the one slot Go writes
/// its `+` pragmas into.
///
/// | Option | Effect | Go SDK equivalent |
/// | --- | --- | --- |
/// | `generate` | The function is a generator: `dagger generate` runs it and applies the `Changeset` it returns | `+generate` |
///
/// ```ignore
/// /// Regenerate the checked-in fixtures.
/// #[dagger::function(generate)]
/// pub fn generate(&self, ws: Workspace) -> Changeset {
///     …
/// }
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
/// exiting non-zero, which [`dagger::fail`] does with a message on stderr.
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

/// Declare the object a module serves, and the functions it exposes.
///
/// Applied to the `impl` block of the module's root type. It reads the method
/// signatures at compile time and emits the `Object` impl that `serve::<T>()`
/// walks — the table describing the module to the engine, and the dispatch that
/// routes an incoming call.
///
/// ```ignore
/// #![no_std]
/// #![no_main]
///
/// use goish::{fmt, string};
///
/// pub struct Build;
///
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
/// What is not supported is a list of anything, in either direction, and an
/// optional return.
///
/// [`ObjectId::from_id`]: ../dagger/trait.ObjectId.html#tymethod.from_id
#[proc_macro_attribute]
pub fn object(_attr: TokenStream, item: TokenStream) -> TokenStream {
    match expand(item.clone()) {
        Ok(generated) => {
            let mut out = strip_markers(item);
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
    matches!(path.as_str(), "dagger" | "dagger::function" | "dagger::check")
}

/// Options read from the marker attribute itself, `#[dagger::function(...)]`.
#[derive(Default)]
struct FunctionOptions {
    generate: bool,
}

/// Read the options a function's marker attribute carried.
///
/// These are the flags Go writes as `+` pragmas in a doc comment — one slot,
/// several markers — rather than the per-argument `#[dagger(...)]` options.
fn function_options_of(f: &Function) -> Result<FunctionOptions, String> {
    let mut options = FunctionOptions::default();
    for part in split_commas(&f.options) {
        match part.first() {
            Some(TokenTree::Ident(id)) if part.len() == 1 && id.to_string() == "generate" => {
                options.generate = true
            }
            _ => {
                return Err(format!(
                    "unrecognized option `{}` on `{}`; #[dagger::function(...)] accepts `generate`",
                    render(&part),
                    f.name
                ))
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
struct Kind {
    kind: &'static str,
    /// The engine's name for an `OBJECT_KIND`; empty for a scalar. Owned rather
    /// than `&'static str` because it comes from the signature: any object the
    /// engine knows can be named, so the set is the schema's, not this crate's.
    object: String,
    optional: bool,
    /// The accessor on `Arguments` that yields this type.
    getter: &'static str,
}

/// Strip one `Option<...>` layer, returning the type it wrapped.
fn unwrap_option(ty: &str) -> Option<&str> {
    ty.trim()
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
        .map(|inner| inner.trim())
}

/// The last segment of a path, so `dagger::Changeset` reads as `Changeset`.
fn last_segment(ty: &str) -> &str {
    ty.rsplit("::").next().unwrap_or(ty).trim()
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
/// Scalars are a fixed set; everything else that looks like a type name is an
/// engine object, named to the engine exactly as the last segment of the path
/// spells it. That is as far as this can check: the object's *existence* is the
/// engine's to know, and a name it does not have is reported when the module
/// registers. What the name has to satisfy here is that a type of that name
/// implements [`ObjectId`], which the generated bindings do for every object
/// the engine has a loader for — so a misspelled or unsupported one fails to
/// compile, naming the trait.
///
/// [`ObjectId`]: ../dagger/trait.ObjectId.html
fn kind_of(ty: &str) -> Result<Kind, String> {
    let trimmed = ty.trim();
    if let Some(inner) = unwrap_option(trimmed) {
        let mut inner = kind_of(inner)?;
        inner.optional = true;
        return Ok(inner);
    }
    let (kind, getter) = match trimmed {
        "string" | "String" | "&str" => ("STRING_KIND", "string"),
        "int" | "i64" | "i32" | "isize" | "usize" => ("INTEGER_KIND", "int"),
        "bool" => ("BOOLEAN_KIND", "bool"),
        "" | "()" => ("VOID_KIND", "void"),
        other => {
            // Written as `Directory` or as `gen::Directory`; both name the same
            // object as far as the engine is concerned.
            if !is_object_name(other) {
                return Err(format!(
                    "unsupported type `{other}`: a function's arguments and return are string, int, bool, or an engine object named as a plain type — `Directory`, `Container`, `Workspace`. Lists and other generics are not supported yet"
                ));
            }
            return Ok(Kind {
                kind: "OBJECT_KIND",
                object: last_segment(other).to_string(),
                optional: false,
                getter: "object",
            });
        }
    };
    Ok(Kind { kind, object: String::new(), optional: false, getter })
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

/// Build the `Object` impl for the annotated block.
fn expand(item: TokenStream) -> Result<TokenStream, String> {
    // `check` is a marker of its own rather than an option on `function`, so a
    // method carrying only `#[dagger::check]` is still exported.
    let (type_name, functions) = parse::parse_impl(item, &["function", "check"])?;

    let mut defs = String::new();
    let mut arms = String::new();

    for f in &functions {
        defs.push_str(&function_def(f)?);
        arms.push_str(&dispatch_arm(&type_name, f)?);
    }

    let generated = format!(
        r#"
impl ::dagger::Object for {type_name} {{
    const NAME: &'static str = {name_literal};

    fn functions() -> &'static [::dagger::FunctionDef] {{
        const FUNCTIONS: &[::dagger::FunctionDef] = &[{defs}];
        FUNCTIONS
    }}

    fn invoke(
        name: &::goish::gostring::string,
        args: &::dagger::Arguments,
    ) -> ::core::result::Result<::goish::gostring::string, ::goish::gostring::string> {{
        {arms}
        ::core::result::Result::Err(
            ::goish::convert::string("no such function: ") + name.clone(),
        )
    }}
}}
"#,
        type_name = type_name,
        name_literal = quote_str(&type_name),
        defs = defs,
        arms = arms,
    );

    generated
        .parse()
        .map_err(|e| format!("generated code did not parse: {e}"))
}

/// Render one `FunctionDef`.
fn function_def(f: &Function) -> Result<String, String> {
    let function_options = function_options_of(f)?;
    let mut args = String::new();
    for param in &f.params {
        let options = options_of(&param.attrs)?;
        let kind = kind_of(&param.ty)?;

        // The engine takes these on a contextual argument only — "can only set
        // default path for Object, not STRING_KIND" — and it says so at module
        // load, which is a bad place to learn about a typo. The two types it
        // means are known here, so the message can name the parameter instead.
        if !options.default_path.is_empty() || !options.ignore.is_empty() {
            let which = if options.default_path.is_empty() { "ignore" } else { "default_path" };
            if kind.object != "Directory" && kind.object != "File" {
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
        if function_options.generate && !optional && !contextual && kind.object != "Workspace" {
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
            "::dagger::ArgDef {{ name: {name}, kind: {kind}, object: {object}, optional: {optional}, doc: {doc}, default_value: {default}, default_path: {path}, ignore: &[{ignore}], deprecated: {deprecated} }},",
            name = quote_str(&camel_case(&param.name)),
            kind = quote_str(kind.kind),
            object = quote_str(&kind.object),
            optional = optional,
            doc = quote_str(&options.doc),
            default = quote_str(&options.default_value),
            path = quote_str(&options.default_path),
            ignore = ignore,
            deprecated = quote_str(&options.deprecated),
        ));
    }

    let ret = kind_of(&f.return_ty)?;

    // The other half of the generator contract: `dagger generate` applies what
    // the function returns, so a generator returns the changes it made.
    if function_options.generate && (ret.object != "Changeset" || ret.optional) {
        return Err(format!(
            "`{}` is a generate function, so it must return `Changeset`, but it returns `{}`",
            f.name,
            if f.return_ty.is_empty() { "()" } else { &f.return_ty }
        ));
    }

    Ok(format!(
        "::dagger::FunctionDef {{ name: {name}, doc: {doc}, return_kind: {ret}, return_object: {ret_object}, is_check: {is_check}, generator: {generator}, args: &[{args}] }},",
        name = quote_str(&camel_case(&f.name)),
        doc = quote_str(&f.doc),
        ret = quote_str(ret.kind),
        ret_object = quote_str(&ret.object),
        is_check = f.has_marker("check"),
        generator = function_options.generate,
        args = args,
    ))
}

/// Render the `match` arm that calls one function and encodes its result.
fn dispatch_arm(type_name: &str, f: &Function) -> Result<String, String> {
    let mut bindings = String::new();
    let mut call_args = Vec::new();

    for param in &f.params {
        let kind = kind_of(&param.ty)?;
        let accessor = if kind.optional {
            format!("{}_opt", kind.getter)
        } else {
            kind.getter.to_string()
        };
        let read = format!(
            "args.{accessor}({name})?",
            accessor = accessor,
            name = quote_str(&camel_case(&param.name)),
        );
        // An object arrives as its ID, so it is rebuilt through the trait
        // rather than used directly. Naming the type the way the parameter does
        // means it resolves in the user's scope, whatever they imported it from.
        let value = if kind.object.is_empty() {
            read
        } else {
            let ty = unwrap_option(&param.ty).unwrap_or(param.ty.trim());
            let from_id = format!("<{ty} as ::dagger::ObjectId>::from_id");
            if kind.optional {
                format!("{read}.map({from_id})")
            } else {
                format!("{from_id}({read})")
            }
        };
        bindings.push_str(&format!("let {binding} = {value};", binding = param.name));
        call_args.push(param.name.clone());
    }

    let receiver = if f.takes_self {
        format!("{type_name}.")
    } else {
        format!("{type_name}::")
    };
    // A unit struct is its own value, so `MyModule.method(..)` is how a `&self`
    // method is reached without the engine having constructed anything.
    let call = format!("{receiver}{fname}({args})", fname = f.name, args = call_args.join(", "));
    let ret = kind_of(&f.return_ty)?;

    // A returned `Option<T>` would have to be declared optional to the engine,
    // which `FunctionDef` has no room for yet, and every encoder below takes the
    // bare value. Say so rather than emitting code that fails to typecheck
    // somewhere inside the macro's output.
    if ret.optional {
        return Err(format!(
            "`{}` returns `{}`; an optional return is not supported yet, so return `{}` and fail with `dagger::fail` instead",
            f.name,
            f.return_ty,
            unwrap_option(&f.return_ty).unwrap_or("T")
        ));
    }

    let encode = match ret.kind {
        "VOID_KIND" => format!("{{ {call}; ::dagger::encode_void() }}"),
        "STRING_KIND" => format!("::dagger::encode_string(&{call})"),
        "INTEGER_KIND" => format!("::dagger::encode_int({call})"),
        "BOOLEAN_KIND" => format!("::dagger::encode_bool({call})"),
        // Fallible, unlike the others: reading a generated object's id runs the
        // chain the function built.
        "OBJECT_KIND" => format!("::dagger::encode_object(&{call})?"),
        other => return Err(format!("cannot encode a return of kind {other}")),
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
