//! A small recursive-descent reader over `proc_macro::TokenTree`.
//!
//! This is the whole reason the crate has no dependencies: `syn` would do all of
//! it, but the repository takes nothing from crates.io, so the shapes we care
//! about — an `impl` block, a function signature, an attribute list — are
//! matched by hand.
//!
//! Only what the macros need is parsed. Anything unrecognized is left alone and
//! re-emitted, so an unusual signature fails in `rustc` against the user's own
//! source rather than being silently dropped here.

use proc_macro::{Delimiter, TokenStream, TokenTree};

/// One `#[...]` attribute, split into its path and its argument tokens.
pub struct Attr {
    /// The attribute path, joined without spaces: `dagger`, `dagger::function`, `doc`.
    pub path: String,
    /// The tokens inside the delimiter, if the attribute had any.
    pub args: Vec<TokenTree>,
}

impl Attr {
    /// The value of a `#[doc = "..."]` attribute, unescaped.
    pub fn doc_value(&self) -> Option<String> {
        if self.path != "doc" {
            return None;
        }
        // `#[doc = "text"]` arrives as `= "text"`.
        let literal = self.args.iter().find_map(|t| match t {
            TokenTree::Literal(l) => Some(l.to_string()),
            _ => None,
        })?;
        Some(unquote(&literal))
    }
}

/// Where a declaration was written, read off the span of the token that names
/// it.
///
/// The name's own token rather than the keyword in front of it: what a source
/// map is for is jumping to a declaration, and `pub fn build` puts the name
/// three tokens past the start of the item. `file` is what
/// `proc_macro::Span::file()` reports — relative to the crate root, so
/// `src/main.rs` for a module's own source — and both numbers are one-indexed.
///
/// [`unknown`](SourceLoc::unknown) is what a hand-built [`Function`] carries in
/// the crate's own tests: `Span` cannot be touched outside a macro expansion,
/// so there is nothing to read there.
pub struct SourceLoc {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

impl SourceLoc {
    /// No location: emitted as `SourceMapDef::UNKNOWN`, and registered as no
    /// source map at all.
    pub fn unknown() -> SourceLoc {
        SourceLoc {
            file: String::new(),
            line: 0,
            column: 0,
        }
    }

    /// The location a token was written at.
    pub fn of(tree: &TokenTree) -> SourceLoc {
        let span = tree.span();
        SourceLoc {
            file: span.file(),
            line: span.line(),
            column: span.column(),
        }
    }
}

/// A parameter of a function: its name, its type tokens, and its `#[dagger(...)]`.
pub struct Param {
    pub name: String,
    /// The type, rendered back to source text.
    pub ty: String,
    pub attrs: Vec<Attr>,
    /// Where the parameter's name was written.
    pub source: SourceLoc,
}

/// The `impl` block `#[dagger::object]` was applied to.
pub struct ImplBlock {
    /// The type the block implements, by its last path segment.
    pub type_name: String,
    /// Joined `///` lines on the block itself.
    pub doc: String,
    /// Where the type name was written.
    pub source: SourceLoc,
    pub functions: Vec<Function>,
}

/// A function we were asked to export.
pub struct Function {
    pub name: String,
    /// Joined `///` lines.
    pub doc: String,
    /// Where the function's name was written.
    pub source: SourceLoc,
    pub params: Vec<Param>,
    /// The return type as source text, empty when the function returns nothing.
    pub return_ty: String,
    /// Whether the function takes a receiver (`&self` / `self`).
    pub takes_self: bool,
    /// Which of the requested markers the method carried — `function`, `check`.
    pub markers: Vec<String>,
    /// The tokens inside the marker attribute — `generate` in
    /// `#[dagger::function(generate)]`. Empty when it carried none.
    pub options: Vec<TokenTree>,
}

impl Function {
    /// Whether the method carried a given marker attribute.
    pub fn has_marker(&self, marker: &str) -> bool {
        self.markers.iter().any(|m| m == marker)
    }
}

/// Strip the surrounding quotes from a string literal and undo its escapes.
pub fn unquote(literal: &str) -> String {
    let inner = literal
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(literal);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Escape a string so it can be emitted as a Rust string literal.
pub fn quote_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Read the `#[...]` attributes at the cursor, leaving it on the first token
/// that is not part of one.
fn take_attrs(tokens: &[TokenTree], cursor: &mut usize) -> Vec<Attr> {
    let mut attrs = Vec::new();
    loop {
        let is_attr = matches!(tokens.get(*cursor), Some(TokenTree::Punct(p)) if p.as_char() == '#');
        if !is_attr {
            return attrs;
        }
        let group = match tokens.get(*cursor + 1) {
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Bracket => g.clone(),
            // A lone `#` is not ours; stop rather than guess.
            _ => return attrs,
        };
        *cursor += 2;

        let inner: Vec<TokenTree> = group.stream().into_iter().collect();
        let mut path = String::new();
        let mut split = inner.len();
        for (i, tree) in inner.iter().enumerate() {
            match tree {
                TokenTree::Ident(id) => path.push_str(&id.to_string()),
                TokenTree::Punct(p) if p.as_char() == ':' => path.push(':'),
                _ => {
                    split = i;
                    break;
                }
            }
        }
        let args = match inner.get(split) {
            // `#[dagger(...)]` — the arguments are the group's contents.
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                g.stream().into_iter().collect()
            }
            // `#[doc = "..."]` — keep the rest as-is.
            _ => inner[split..].to_vec(),
        };
        attrs.push(Attr { path, args });
    }
}

/// Split a token run on top-level commas.
pub fn split_commas(tokens: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    for tree in tokens {
        match tree {
            TokenTree::Punct(p) if p.as_char() == ',' => {
                if !current.is_empty() {
                    parts.push(core::mem::take(&mut current));
                }
            }
            other => current.push(other.clone()),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Render tokens back to source text, collapsing the spaces `to_string` adds
/// inside paths and generics so `Option < string >` reads as `Option<string>`.
///
/// A path separator arrives as two `:` tokens, not one `::`, so it is ` : : `
/// that has to be collapsed — and every one of them, since `a::b::c` is three
/// segments. Getting this wrong is quiet: `gen::Directory` renders as
/// `gen : : Directory`, whose last segment is the whole string, and the engine
/// is then told the argument is an object of that name.
pub fn render(tokens: &[TokenTree]) -> String {
    let raw: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
    let joined = raw.join(" ");
    joined
        .replace(" : : ", "::")
        .replace(" :: ", "::")
        .replace(" < ", "<")
        .replace(" > ", ">")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(" ,", ",")
        .replace(" ;", ";")
        .trim()
        .to_string()
}

/// One variant of an enum: its name and its doc comment.
#[cfg_attr(test, derive(Debug))]
pub struct Variant {
    pub name: String,
    /// Joined `///` lines.
    pub doc: String,
}

/// An enum we were asked to declare.
pub struct Enum {
    pub name: String,
    /// Joined `///` lines.
    pub doc: String,
    pub variants: Vec<Variant>,
}

/// Parse an `enum` item into its name, its doc comment and its variants.
///
/// Only the shape the engine has an equivalent for is accepted. A variant that
/// carries data — `Tagged(string)`, `Point { x: int }` — is refused by name:
/// the engine's enum is a set of names, so there is nowhere for a payload to
/// go, and a variant with one would otherwise be declared as if it had none and
/// fail to construct on the way back in.
///
/// A discriminant (`Alpine = 1`) is refused for the same reason from the other
/// side: what crosses the boundary is the member's *name*, so a number written
/// next to it would be silently dropped rather than being what the engine sees.
pub fn parse_enum(item: TokenStream) -> Result<Enum, String> {
    let tokens: Vec<TokenTree> = item.into_iter().collect();
    let mut cursor = 0;

    let attrs = take_attrs(&tokens, &mut cursor);
    let doc = join_docs(&attrs);

    // `pub` / `pub(crate)` and friends.
    while let Some(TokenTree::Ident(id)) = tokens.get(cursor) {
        if id.to_string() == "pub" {
            cursor += 1;
            if let Some(TokenTree::Group(g)) = tokens.get(cursor) {
                if g.delimiter() == Delimiter::Parenthesis {
                    cursor += 1;
                }
            }
        } else {
            break;
        }
    }

    match tokens.get(cursor) {
        Some(TokenTree::Ident(id)) if id.to_string() == "enum" => cursor += 1,
        _ => return Err("#[dagger::enum_type] expects an `enum`".to_string()),
    }

    let name = match tokens.get(cursor) {
        Some(TokenTree::Ident(id)) => id.to_string(),
        _ => return Err("expected a name after `enum`".to_string()),
    };
    cursor += 1;

    let body = match tokens.get(cursor) {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g.stream(),
        // A generic enum never reaches the engine, and neither does anything
        // else between the name and the body.
        _ => return Err(format!("`{name}` is not a plain `enum Name {{ … }}`")),
    };

    let mut variants = Vec::new();
    let body: Vec<TokenTree> = body.into_iter().collect();
    for part in split_commas(&body) {
        let mut cursor = 0;
        let attrs = take_attrs(&part, &mut cursor);
        let rest = &part[cursor..];

        let variant = match rest.first() {
            Some(TokenTree::Ident(id)) => id.to_string(),
            None => continue,
            _ => return Err(format!("could not read a variant of `{name}`: {}", render(rest))),
        };

        variants.push(variant_of(&name, &variant, &render(&rest[1..]), join_docs(&attrs))?);
    }

    if variants.is_empty() {
        return Err(format!("`{name}` has no variants; an engine enum needs at least one member"));
    }

    Ok(Enum { name, doc, variants })
}

/// One variant, from its name and whatever the declaration wrote after it.
///
/// Split out of the walk above so the rule it enforces is reachable from this
/// crate's own tests: the `proc_macro` API panics outside a macro expansion, so
/// a test cannot hand [`parse_enum`] an `enum` to read, but it can hand this the
/// text a variant rendered as.
pub fn variant_of(
    enum_name: &str,
    name: &str,
    trailing: &str,
    doc: String,
) -> Result<Variant, String> {
    if !trailing.is_empty() {
        return Err(format!(
            "`{enum_name}::{name}` carries `{trailing}`; an engine enum is a set of member names, so its variants take no data and no discriminant"
        ));
    }
    Ok(Variant { name: name.to_string(), doc })
}

/// Join the `///` lines of an attribute list into one description.
fn join_docs(attrs: &[Attr]) -> String {
    attrs
        .iter()
        .filter_map(|a| a.doc_value())
        .map(|line| line.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse the body of an `impl` block into the functions carrying any of
/// `markers`, recording which ones each carried.
///
/// Returns the impl's own metadata alongside them: its type name, its `///`
/// doc comment and where the type name was written. The doc comment is the
/// block's, not the type's — a `struct` declaration is a separate item and this
/// macro never sees it — and it survives being written in front of
/// `#[dagger::object]`, since an inert `#[doc = "..."]` stays on the item the
/// attribute macro is handed.
pub fn parse_impl(item: TokenStream, markers: &[&str]) -> Result<ImplBlock, String> {
    let tokens: Vec<TokenTree> = item.into_iter().collect();
    let mut cursor = 0;

    // The attributes on the impl block itself: its doc comment is the object's
    // description, and the module's.
    let attrs = take_attrs(&tokens, &mut cursor);
    let doc = join_docs(&attrs);

    match tokens.get(cursor) {
        Some(TokenTree::Ident(id)) if id.to_string() == "impl" => cursor += 1,
        _ => return Err("#[dagger::object] expects an `impl` block".to_string()),
    }

    let mut type_name = String::new();
    let mut source = SourceLoc::unknown();
    while let Some(tree) = tokens.get(cursor) {
        match tree {
            TokenTree::Ident(id) => {
                type_name = id.to_string();
                source = SourceLoc::of(tree);
                cursor += 1;
            }
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => break,
            _ => cursor += 1,
        }
    }
    if type_name.is_empty() {
        return Err("could not read the type name from the `impl` block".to_string());
    }

    let body = match tokens.get(cursor) {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g.stream(),
        _ => return Err("`impl` block has no body".to_string()),
    };

    Ok(ImplBlock {
        type_name,
        doc,
        source,
        functions: parse_items(body, markers)?,
    })
}

/// Whether an attribute names a marker, as `#[marker]` or `#[path::marker]`.
fn is_marker(attr: &Attr, marker: &str) -> bool {
    attr.path == marker || attr.path.ends_with(&format!("::{marker}"))
}

/// Walk the items of an impl body, collecting the marked functions.
fn parse_items(body: TokenStream, markers: &[&str]) -> Result<Vec<Function>, String> {
    let tokens: Vec<TokenTree> = body.into_iter().collect();
    let mut cursor = 0;
    let mut functions = Vec::new();

    while cursor < tokens.len() {
        let attrs = take_attrs(&tokens, &mut cursor);

        // `pub` / `pub(crate)` and friends.
        while let Some(TokenTree::Ident(id)) = tokens.get(cursor) {
            if id.to_string() == "pub" {
                cursor += 1;
                if let Some(TokenTree::Group(g)) = tokens.get(cursor) {
                    if g.delimiter() == Delimiter::Parenthesis {
                        cursor += 1;
                    }
                }
            } else {
                break;
            }
        }

        let is_fn = matches!(tokens.get(cursor), Some(TokenTree::Ident(id)) if id.to_string() == "fn");
        if !is_fn {
            // Not a function: skip to the end of this item so a stray `const`
            // or type alias in the impl block does not derail the walk.
            skip_item(&tokens, &mut cursor);
            continue;
        }
        cursor += 1;

        let (name, source) = match tokens.get(cursor) {
            Some(tree @ TokenTree::Ident(id)) => (id.to_string(), SourceLoc::of(tree)),
            _ => return Err("expected a name after `fn`".to_string()),
        };
        cursor += 1;

        let params_group = loop {
            match tokens.get(cursor) {
                Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                    let g = g.clone();
                    cursor += 1;
                    break g;
                }
                Some(_) => cursor += 1,
                None => return Err(format!("function `{name}` has no parameter list")),
            }
        };

        // Everything up to the body is the return type.
        let mut return_tokens = Vec::new();
        while let Some(tree) = tokens.get(cursor) {
            match tree {
                TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => break,
                other => {
                    return_tokens.push(other.clone());
                    cursor += 1;
                }
            }
        }
        cursor += 1; // the body

        // A method may carry more than one marker, and a marker may carry
        // options: `#[dagger::function(generate)]`. Only one of them ever does,
        // so the options collected here are that one's.
        let mut found: Vec<String> = Vec::new();
        let mut options: Vec<TokenTree> = Vec::new();
        for marker in markers {
            let marked = match attrs.iter().find(|a| is_marker(a, marker)) {
                Some(attr) => attr,
                None => continue,
            };
            found.push(marker.to_string());
            options.extend(marked.args.iter().cloned());
        }
        if found.is_empty() {
            continue;
        }

        let doc = join_docs(&attrs);

        let (params, takes_self) = parse_params(params_group.stream())?;

        // Drop the leading `->`.
        let return_ty = {
            let text = render(&return_tokens);
            text.trim_start_matches("->").trim().to_string()
        };

        functions.push(Function {
            name,
            doc,
            source,
            params,
            return_ty,
            takes_self,
            markers: found,
            options,
        });
    }

    Ok(functions)
}

/// Advance past a non-function item.
fn skip_item(tokens: &[TokenTree], cursor: &mut usize) {
    while let Some(tree) = tokens.get(*cursor) {
        *cursor += 1;
        match tree {
            TokenTree::Punct(p) if p.as_char() == ';' => return,
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => return,
            _ => {}
        }
    }
}

/// Parse a parameter list, reporting whether it began with a receiver.
fn parse_params(stream: TokenStream) -> Result<(Vec<Param>, bool), String> {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut params = Vec::new();
    let mut takes_self = false;

    for part in split_commas(&tokens) {
        let mut cursor = 0;
        let attrs = take_attrs(&part, &mut cursor);
        let rest = &part[cursor..];

        let is_receiver = rest.iter().any(|t| matches!(t, TokenTree::Ident(id) if id.to_string() == "self"))
            && !rest.iter().any(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ':'));
        if is_receiver {
            takes_self = true;
            continue;
        }

        let colon = rest
            .iter()
            .position(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ':'))
            .ok_or_else(|| format!("parameter `{}` has no type", render(rest)))?;

        let name = render(&rest[..colon]);
        let ty = render(&rest[colon + 1..]);
        if name.is_empty() || ty.is_empty() {
            return Err(format!("could not read parameter `{}`", render(rest)));
        }
        let source = rest.first().map(SourceLoc::of).unwrap_or_else(SourceLoc::unknown);
        params.push(Param {
            name,
            ty,
            attrs,
            source,
        });
    }

    Ok((params, takes_self))
}
