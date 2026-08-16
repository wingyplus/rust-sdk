//! Render an init template for a Dagger module name.
//!
//! ```text
//! render-template MODULE_NAME TEMPLATE_DIR OUT_DIR
//! ```
//!
//! Walks `TEMPLATE_DIR` and writes the result to `OUT_DIR`. Files ending in
//! `.tmpl` are rendered and lose the suffix; everything else is copied
//! verbatim. Path segments containing `{{` are rendered too, so a template can
//! name a file after the module (e.g. `src/{{.ModuleCrate}}.rs.tmpl`).
//!
//! Available fields:
//!
//! ```text
//! .ModuleName   the Dagger module name, verbatim  ("my-module")
//! .ModuleType   the Rust type name                ("MyModule")
//! .ModuleCrate  the cargo package / crate name    ("my_module")
//! ```
//!
//! [`rust_crate_name`] MUST stay byte-for-byte identical to `toRustCrateName`
//! in `../../runtime/main.dang`: this helper writes the `[package]` name into
//! the scaffolded `Cargo.toml` at init time, and the runtime derives the binary
//! cargo emits from the Dagger module name at call time. If the two ever
//! disagree, the module builds and the entrypoint points at a path that does
//! not exist.
//!
//! # Ported from Go
//!
//! This was a Go program built on `text/template` and `regexp`. It is now
//! `no_std` Rust on goish, like everything else in this repository, and two
//! things changed with the port — both deliberate:
//!
//! * **The name derivation is a scan, not three regular expressions.** goish's
//!   `regexp.ReplaceAllString` treats its replacement as literal text — `${1}`
//!   group expansion is not in its v1 subset — so the substitutions the Go
//!   version applied cannot be spelled that way here. [`rust_crate_name`]
//!   folds them into one left-to-right pass instead; the equivalence is
//!   argued at the function and *tested* by `TestDangCrateNameMatchesRust`,
//!   which replays the dang recipe through goish's regexp engine.
//! * **The template dialect is a strict subset of `text/template`.** An action
//!   is a field reference and nothing else, and an unknown field is an error
//!   rather than Go's silent `<no value>` — see [`render`]. Every template
//!   lives in this repository, so a typo is a bug to report, not a value to
//!   substitute.

#![no_std]

use goish::path::filepath;
use goish::{append, bytes, error, errors, fmt, len, make, nil, os, slice, string, strings};

/// Every word Rust reserves as of the 2024 edition: the strict keywords, the
/// reserved-for-future-use set, and the edition-specific additions.
///
/// Raw identifiers are not an escape hatch for all of them — `crate`, `self`,
/// `super` and `Self` cannot be written `r#`-prefixed — so a name landing here
/// is rejected rather than escaped.
pub const RUST_KEYWORDS: &[&str] = &[
    // Strict.
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", // Strict from the 2018 edition on.
    "async", "await", "dyn", // Reserved for future use.
    "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof", "unsized",
    "virtual", "yield", // Reserved from the 2018 edition on.
    "try",   // Reserved from the 2024 edition on.
    "gen",
];

/// Whether `name` is a word Rust reserves. See [`RUST_KEYWORDS`].
pub fn is_rust_keyword(name: &string) -> bool {
    let name = name.as_bytes();
    for keyword in RUST_KEYWORDS {
        if name == keyword.as_bytes() {
            return true;
        }
    }
    false
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

fn is_alphanumeric(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

/// Whether a word boundary — an underscore in the crate name — falls
/// immediately before `src[i]`.
///
/// This is the whole of what the Go version's first two regular expressions
/// did, read one position at a time:
///
/// ```text
/// ([A-Z]+)([A-Z][a-z])  ->  ${1}_${2}     an acronym running into a word
/// ([a-z0-9])([A-Z])     ->  ${1}_${2}     a word running into the next
/// ```
///
/// The first inserts its underscore before the last uppercase letter of an
/// uppercase run that is followed by a lowercase one — which is exactly
/// "upper, upper, lower" read around position `i`. Nothing is lost by scanning
/// instead of matching: within one of those matches the *only* position where
/// an uppercase letter is followed by a lowercase one is the insertion point
/// itself, so a match can never swallow a boundary a later match would have
/// found. And because the first rule only ever inserts between two uppercase
/// letters, it cannot break up a `[a-z0-9][A-Z]` pair, which is why the second
/// rule can be evaluated against the original text rather than the first
/// rule's output.
fn is_word_boundary(src: &[u8], i: usize) -> bool {
    if i == 0 || i >= src.len() {
        return false;
    }
    let prev = src[i - 1];
    let current = src[i];
    if !is_upper(current) {
        return false;
    }
    if is_lower(prev) || is_digit(prev) {
        return true;
    }
    let next = if i + 1 < src.len() { src[i + 1] } else { 0 };
    is_upper(prev) && is_lower(next)
}

/// Convert a Dagger module name into a Rust crate name (`"my-module"` ->
/// `"my_module"`).
///
/// Mirrors `toRustCrateName` in `runtime/main.dang`, which is three regular
/// expressions followed by `trim("_")` and `toLower`. The third of those —
/// `[^A-Za-z0-9]+` -> `_` — is the run-collapsing below: an underscore is held
/// back until the next alphanumeric byte is written, so runs collapse to one
/// and leading and trailing runs are dropped without a separate trim. Bytes
/// outside ASCII alphanumerics — including every byte of a multi-byte UTF-8
/// sequence — are non-alphanumeric to that pattern too, and collapse the same
/// way, so this stays a byte scan rather than a rune one.
pub fn rust_crate_name(name: string) -> string {
    let src = name.as_bytes();
    let mut out = strings::Builder::new();
    let mut written = 0;
    let mut pending_underscore = false;

    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if !is_alphanumeric(c) {
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

/// Derive the module's Rust type name (`"my-module"` -> `"MyModule"`).
///
/// Built from [`rust_crate_name`]'s segmentation, so the two never disagree
/// about where a word starts.
pub fn rust_type_name(name: string) -> string {
    let crate_name = rust_crate_name(name);
    let mut out = strings::Builder::new();
    let mut at_word_start = true;

    for &c in crate_name.as_bytes() {
        if c == b'_' {
            at_word_start = true;
            continue;
        }
        let _ = out.WriteByte(if at_word_start {
            c.to_ascii_uppercase()
        } else {
            c
        });
        at_word_start = false;
    }

    out.String()
}

/// The substitutions a template is rendered with.
pub struct TemplateData {
    /// The Dagger module name, verbatim.
    pub module_name: string,
    /// The Rust type name the module's root object is declared with.
    pub module_type: string,
    /// The cargo package / crate name, and the name of the binary cargo emits.
    pub module_crate: string,
}

impl TemplateData {
    /// Derive the substitutions for a Dagger module name, or explain why the
    /// name cannot be used for a Rust module.
    pub fn new(module_name: string) -> (TemplateData, error) {
        let empty = TemplateData {
            module_name: string(""),
            module_type: string(""),
            module_crate: string(""),
        };

        let crate_name = rust_crate_name(module_name.clone());
        if crate_name.Len() == 0 {
            return (
                empty,
                fmt::Errorf!("module name %q has no alphanumeric characters", module_name),
            );
        }
        // A cargo package name may not start with a digit, and a Rust type name
        // may not either. Both are derived from the same first segment, so one
        // check covers them.
        if is_digit(crate_name.as_bytes()[0]) {
            return (
                empty,
                fmt::Errorf!(
                    "module name %q yields the crate name %q, which starts with a digit",
                    module_name,
                    crate_name
                ),
            );
        }

        // The type name becomes a `struct` declaration in the generated
        // main.rs, so a reserved word there is a hard compile error. In
        // practice only `Self` reaches this — every other Rust keyword is
        // lowercase, and the type name is capitalized per segment — but
        // checking the whole set keeps the rule honest if the derivation ever
        // changes.
        //
        // The crate name is deliberately *not* checked against the same set. It
        // only ever appears as a cargo package name, a bin target name and a
        // filename, never as a Rust identifier: a module named `crate`, `self`,
        // `type` or `move` builds and produces a binary. `cargo new` refuses
        // those names because it also creates a lib target, which these
        // templates do not.
        let type_name = rust_type_name(module_name.clone());
        if is_rust_keyword(&type_name) {
            return (
                empty,
                fmt::Errorf!(
                    "module name %q yields the Rust type name %q, which is a reserved keyword; pick another name",
                    module_name,
                    type_name
                ),
            );
        }

        (
            TemplateData {
                module_name,
                module_type: type_name,
                module_crate: crate_name,
            },
            nil.into(),
        )
    }

    /// The value of a template field, or `None` when no such field exists.
    fn field(&self, name: &[u8]) -> Option<string> {
        match name {
            b"ModuleName" => Some(self.module_name.clone()),
            b"ModuleType" => Some(self.module_type.clone()),
            b"ModuleCrate" => Some(self.module_crate.clone()),
            _ => None,
        }
    }
}

/// Index of `needle` in `haystack` at or after `from`.
fn index_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn trim_ascii_space(mut s: &[u8]) -> &[u8] {
    while let Some((first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = s.split_last() {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Whether every byte of `name` can appear in a template field name.
fn is_field_name(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().all(|&c| is_alphanumeric(c) || c == b'_')
}

/// Render the actions in `text`.
///
/// The dialect is the subset of Go's `text/template` these templates use: an
/// action is `{{ .Field }}`, surrounding whitespace is ignored, and everything
/// else is literal text. Anything an action can be in the full language —
/// pipelines, `if`/`range`, functions, trim markers, comments — is reported as
/// unsupported rather than half-rendered, and an unknown field is an error
/// rather than Go's silent `<no value>`.
///
/// `origin` names what is being rendered — a template path, relative to the
/// template directory — and appears in those errors.
pub fn render(text: string, data: &TemplateData, origin: string) -> (string, error) {
    let src = text.as_bytes();
    let mut out = strings::Builder::new();

    let mut i = 0;
    while i < src.len() {
        let open = match index_from(src, b"{{", i) {
            Some(open) => open,
            None => break,
        };
        let _ = out.WriteString(string::from_bytes(&src[i..open]));

        let close = match index_from(src, b"}}", open + 2) {
            Some(close) => close,
            None => {
                return (
                    string(""),
                    fmt::Errorf!(
                        "%s: unterminated action: %s",
                        origin,
                        string::from_bytes(&src[open..])
                    ),
                )
            }
        };

        let action = trim_ascii_space(&src[open + 2..close]);
        let name: &[u8] = match action.split_first() {
            Some((&b'.', rest)) => rest,
            _ => b"",
        };
        if !is_field_name(name) {
            return (
                string(""),
                fmt::Errorf!(
                    "%s: unsupported template action {{%s}}: only field references are supported (.ModuleName, .ModuleType, .ModuleCrate)",
                    origin,
                    string::from_bytes(action)
                ),
            );
        }
        match data.field(name) {
            Some(value) => {
                let _ = out.WriteString(value);
            }
            None => {
                return (
                    string(""),
                    fmt::Errorf!(
                        "%s: unknown template field .%s: known fields are .ModuleName, .ModuleType and .ModuleCrate",
                        origin,
                        string::from_bytes(name)
                    ),
                )
            }
        }

        i = close + 2;
    }
    let _ = out.WriteString(string::from_bytes(&src[i..]));

    (out.String(), nil.into())
}

/// `filepath.Join(a, b)`.
fn join(a: string, b: string) -> string {
    let elems = make!([]string, 0, 2);
    let elems = append!(elems, a, b);
    filepath::Join(elems)
}

/// Render `TEMPLATE_DIR` for a module name into `OUT_DIR`.
///
/// `args` is argv without the program name: `MODULE_NAME TEMPLATE_DIR OUT_DIR`.
pub fn run(args: slice<string>) -> error {
    if len(&args) != 3 {
        return errors::New("usage: render-template MODULE_NAME TEMPLATE_DIR OUT_DIR");
    }

    let module_name = args[0].clone();
    let template_dir = args[1].clone();
    let out_dir = args[2].clone();

    let (data, err) = TemplateData::new(module_name);
    if err != nil {
        return err;
    }

    filepath::WalkDir(template_dir.clone(), |path, entry, err| {
        if err != nil {
            return err;
        }
        let (rel, err) = filepath::Rel(template_dir.clone(), path.clone());
        if err != nil {
            return err;
        }
        if rel == string(".") {
            return nil.into();
        }

        let trimmed = strings::TrimSuffix(rel.clone(), ".tmpl");
        let dst_rel = if strings::Contains(trimmed.clone(), "{{") {
            let (rendered, err) = render(trimmed, &data, rel.clone());
            if err != nil {
                return err;
            }
            rendered
        } else {
            trimmed
        };
        let dst = join(out_dir.clone(), dst_rel);

        if entry.IsDir() {
            return os::MkdirAll(dst, 0o755);
        }
        if entry.Type() & os::ModeSymlink != 0 {
            return fmt::Errorf!("template symlinks are not supported: %s", rel);
        }
        let err = os::MkdirAll(filepath::Dir(dst.clone()), 0o755);
        if err != nil {
            return err;
        }

        let (contents, err) = os::ReadFile(path.clone());
        if err != nil {
            return err;
        }
        if !strings::HasSuffix(rel.clone(), ".tmpl") {
            return os::WriteFile(dst, contents, 0o644);
        }

        let (rendered, err) = render(string(contents), &data, rel);
        if err != nil {
            return err;
        }
        os::WriteFile(dst, bytes(rendered), 0o644)
    })
}
