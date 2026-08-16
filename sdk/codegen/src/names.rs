//! Turning schema names into Rust names.
//!
//! Three conversions, each with a rule the schema forces:
//!
//! - **Type names are taken verbatim.** `Container`, `GitRef`, `ContainerID`,
//!   `JSON` are already valid Rust type names, and rewriting them would only
//!   introduce a mapping to get wrong. (`ContainerID` is what the Go SDK calls
//!   it too.)
//! - **Field and argument names are snake_cased**, acronym-aware, because
//!   `withGPU` has to become `with_gpu` rather than `with_g_p_u`.
//! - **Enum variants are CamelCased, except where that would collide.** The
//!   schema carries case-variant aliases of the same value — `Gzip` and `GZIP`
//!   both live in `ImageLayerCompression` — so the conversion cannot be
//!   unconditional. See [`enum_variant`].
//!
//! Anything landing on a Rust keyword is escaped by [`escape_ident`].

use goish::{slice, string, strings};

/// Rust's keywords, reserved words included: a schema name that lands on one
/// cannot be used bare.
const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "abstract", "become", "box", "do", "final", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield", "gen",
];

/// The keywords `r#` cannot rescue: they mean something to the language even as
/// a raw identifier, so a name landing on one gets a trailing underscore
/// instead.
const NOT_RAW_ESCAPABLE: &[&str] = &["crate", "self", "Self", "super"];

fn contains(list: &[&str], name: &string) -> bool {
    let name = name.as_bytes();
    for entry in list {
        if name == entry.as_bytes() {
            return true;
        }
    }
    false
}

/// Whether `name` is a Rust keyword.
pub fn is_rust_keyword(name: &string) -> bool {
    contains(RUST_KEYWORDS, name)
}

/// Make `name` usable as a Rust identifier.
///
/// A keyword becomes a raw identifier — `ref` is `r#ref` — which keeps the
/// generated name recognisably the schema's. The four keywords `r#` cannot
/// escape get a trailing underscore.
pub fn escape_ident(name: string) -> string {
    if !is_rust_keyword(&name) {
        return name;
    }
    if contains(NOT_RAW_ESCAPABLE, &name) {
        return name + "_";
    }
    string("r#") + name
}

fn is_upper(c: u8) -> bool {
    c.is_ascii_uppercase()
}

fn is_lower(c: u8) -> bool {
    c.is_ascii_lowercase()
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

/// Whether a word starts at `i`.
///
/// An uppercase letter begins a word when what precedes it is lowercase or a
/// digit (`withExec`, `foo2Bar`) or when what follows it is lowercase, which is
/// the last letter of an acronym starting the next word (`HTTPState` breaks
/// before the `S`, not the `T`).
fn is_word_boundary(src: &[u8], i: usize) -> bool {
    if i == 0 || !is_upper(src[i]) {
        return false;
    }
    if is_lower(src[i - 1]) || is_digit(src[i - 1]) {
        return true;
    }
    i + 1 < src.len() && is_lower(src[i + 1])
}

/// `withExec` → `with_exec`, `withGPU` → `with_gpu`, `HTTPState` →
/// `http_state`.
///
/// Anything that is not a letter or a digit separates words, so an
/// already-snake_cased name survives unchanged.
pub fn snake_case(name: &string) -> string {
    let src = name.as_bytes();
    let mut out = strings::Builder::new();
    let mut written = 0;
    let mut pending_underscore = false;

    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if !(is_upper(c) || is_lower(c) || is_digit(c)) {
            pending_underscore = true;
            i += 1;
            continue;
        }
        if is_word_boundary(src, i) {
            pending_underscore = true;
        }
        // Held back rather than written when it was found: an underscore with
        // nothing before it is a leading one, and one with nothing after it
        // never gets here at all.
        if pending_underscore && written > 0 {
            let _ = out.WriteByte(b'_');
        }
        pending_underscore = false;
        let _ = out.WriteByte(c.to_ascii_lowercase());
        written += 1;
        i += 1;
    }

    out.String()
}

/// A schema field or argument name as a Rust method, parameter or struct field.
pub fn rust_name(name: &string) -> string {
    escape_ident(snake_case(name))
}

/// `withExec` → `WithExec` — the field's contribution to a generated type name
/// like `ContainerWithExecOpts`.
///
/// Only the first byte changes: the rest of a camelCase field name is already
/// the shape a Rust type name wants, and rewriting it would fold `withGPU` and
/// `withGpu` together.
pub fn upper_first(name: &string) -> string {
    let src = name.as_bytes();
    if src.is_empty() {
        return name.clone();
    }
    let mut out = strings::Builder::new();
    let _ = out.WriteByte(src[0].to_ascii_uppercase());
    let mut i = 1;
    while i < src.len() {
        let _ = out.WriteByte(src[i]);
        i += 1;
    }
    out.String()
}

/// `SCREAMING_SNAKE` → `ScreamingSnake`, leaving anything already mixed-case
/// alone.
///
/// A value that already contains a lowercase letter is written the way its
/// author wanted it — `PerSession`, `EStarGZ` — and folding it would only lose
/// information.
fn camel_case(value: &string) -> string {
    let src = value.as_bytes();

    let mut i = 0;
    while i < src.len() {
        if is_lower(src[i]) {
            return value.clone();
        }
        i += 1;
    }

    let mut out = strings::Builder::new();
    let mut start_of_word = true;
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c == b'_' {
            start_of_word = true;
            i += 1;
            continue;
        }
        if start_of_word {
            let _ = out.WriteByte(c.to_ascii_uppercase());
        } else {
            let _ = out.WriteByte(c.to_ascii_lowercase());
        }
        start_of_word = false;
        i += 1;
    }
    out.String()
}

/// The Rust variant name for one value of an enum whose full value set is
/// `all`.
///
/// Normally [`camel_case`]. But an enum can hold two values that differ only in
/// case — `ImageLayerCompression` has both `Gzip` and `GZIP`, neither
/// deprecated — and CamelCasing both produces the same variant twice, which
/// does not compile. When a value's CamelCase form is not unique across `all`,
/// every value sharing it keeps its schema spelling instead: `Gzip` and `GZIP`.
///
/// Deciding from the whole value set rather than from what has been emitted so
/// far is what makes this independent of the order the values arrive in, so the
/// same schema always generates the same variant names.
pub fn enum_variant(value: &string, all: &slice<string>) -> string {
    let folded = camel_case(value);

    let mut collisions = 0;
    let mut i: goish::int = 0;
    while i < all.Len() {
        if camel_case(&all[i]) == folded {
            collisions += 1;
        }
        i += 1;
    }

    if collisions > 1 {
        return escape_ident(value.clone());
    }
    escape_ident(folded)
}
