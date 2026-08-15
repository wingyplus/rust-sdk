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

extern crate proc_macro;

mod parse;

use parse::{quote_str, render, split_commas, unquote, Attr, Function};
use proc_macro::{Delimiter, TokenStream, TokenTree};

/// Mark a method as part of the module's API. Consumed by [`macro@object`].
///
/// On its own this is inert: it passes the method through untouched so that the
/// attribute resolves and an IDE sees a normal function. `#[dagger::object]` on
/// the surrounding `impl` block is what reads it.
#[proc_macro_attribute]
pub fn function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Declare the object a module serves, and the functions it exposes.
///
/// Applied to an `impl` block. Every method inside carrying
/// `#[dagger::function]` becomes part of the module's API; everything else is
/// left alone.
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
    path == "dagger" || path == "dagger::function" || path == "dagger_sdk::function"
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
    optional: bool,
    /// The accessor on `Arguments` that yields this type.
    getter: &'static str,
}

/// Map a Rust type to a TypeDefKind.
///
/// Only scalars for now. Object types (`Container`, `Directory`, …) need the
/// generated bindings, which are still a placeholder, so they are rejected here
/// with a message that says so rather than failing later as a type error.
fn kind_of(ty: &str) -> Result<Kind, String> {
    let trimmed = ty.trim();
    if let Some(inner) = trimmed
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
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
            return Err(format!(
                "unsupported type `{other}`: the Rust SDK currently exposes only string, int and bool. Object types such as Container need the generated bindings, which are not implemented yet"
            ))
        }
    };
    Ok(Kind { kind, optional: false, getter })
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
    let (type_name, functions) = parse::parse_impl(item, "function")?;

    let mut defs = String::new();
    let mut arms = String::new();

    for f in &functions {
        defs.push_str(&function_def(f)?);
        arms.push_str(&dispatch_arm(&type_name, f)?);
    }

    let generated = format!(
        r#"
impl ::dagger_sdk::Object for {type_name} {{
    const NAME: &'static str = {name_literal};

    fn functions() -> &'static [::dagger_sdk::FunctionDef] {{
        const FUNCTIONS: &[::dagger_sdk::FunctionDef] = &[{defs}];
        FUNCTIONS
    }}

    fn invoke(
        name: &::goish::gostring::string,
        args: &::dagger_sdk::Arguments,
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
    let mut args = String::new();
    for param in &f.params {
        let options = options_of(&param.attrs)?;
        let kind = kind_of(&param.ty)?;
        // A defaulted argument is optional whether or not it is an Option<T>:
        // the caller may leave it out and get the default.
        let optional = kind.optional || !options.default_value.is_empty();
        let ignore = options
            .ignore
            .iter()
            .map(|p| quote_str(p))
            .collect::<Vec<_>>()
            .join(", ");

        args.push_str(&format!(
            "::dagger_sdk::ArgDef {{ name: {name}, kind: {kind}, optional: {optional}, doc: {doc}, default_value: {default}, default_path: {path}, ignore: &[{ignore}], deprecated: {deprecated} }},",
            name = quote_str(&camel_case(&param.name)),
            kind = quote_str(kind.kind),
            optional = optional,
            doc = quote_str(&options.doc),
            default = quote_str(&options.default_value),
            path = quote_str(&options.default_path),
            ignore = ignore,
            deprecated = quote_str(&options.deprecated),
        ));
    }

    let ret = kind_of(&f.return_ty)?;
    Ok(format!(
        "::dagger_sdk::FunctionDef {{ name: {name}, doc: {doc}, return_kind: {ret}, args: &[{args}] }},",
        name = quote_str(&camel_case(&f.name)),
        doc = quote_str(&f.doc),
        ret = quote_str(ret.kind),
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
        bindings.push_str(&format!(
            "let {binding} = args.{accessor}({name})?;",
            binding = param.name,
            accessor = accessor,
            name = quote_str(&camel_case(&param.name)),
        ));
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
    let encode = match ret.kind {
        "VOID_KIND" => format!("{{ {call}; ::dagger_sdk::encode_void() }}"),
        "STRING_KIND" => format!("::dagger_sdk::encode_string(&{call})"),
        "INTEGER_KIND" => format!("::dagger_sdk::encode_int({call})"),
        "BOOLEAN_KIND" => format!("::dagger_sdk::encode_bool({call})"),
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
