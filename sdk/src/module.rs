//! What a module declares, and how a call reaches it.
//!
//! The tables here are what `#[dagger::object]` emits. They are `const`, so the
//! names and kinds are `&'static str` — that is core, not std, and costs no
//! allocation. Everything that carries a *value* at runtime is a goish type.

use goish::encoding::json;
use goish::{nil, string};

/// One argument of an exported function.
pub struct ArgDef {
    /// API name, camelCased from the Rust parameter.
    pub name: &'static str,
    /// The engine's TypeDefKind: `STRING_KIND`, `INTEGER_KIND`, `BOOLEAN_KIND`.
    pub kind: &'static str,
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
}

/// One exported function.
pub struct FunctionDef {
    /// API name, camelCased from the Rust method.
    pub name: &'static str,
    /// The method's `///` doc comment.
    pub doc: &'static str,
    /// The engine's TypeDefKind for the return value.
    pub return_kind: &'static str,
    /// From `#[dagger::check]`: `dagger check` runs this function.
    pub is_check: bool,
    pub args: &'static [ArgDef],
}

/// A module's root object, as declared by `#[dagger::object]`.
pub trait Object {
    /// The object name the engine knows this module by.
    const NAME: &'static str;

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
/// Holds the decoded `inputArgs` as JSON values; the accessors below are what
/// the generated dispatch calls, one per supported type.
pub struct Arguments {
    entries: json::Value,
}

impl Arguments {
    /// Wrap the `inputArgs` array from `currentFunctionCall`.
    pub fn new(entries: json::Value) -> Arguments {
        Arguments { entries }
    }

    /// Find an argument's decoded value.
    ///
    /// `inputArgs` is a list of `{name, value}` where `value` is itself a JSON
    /// document encoded as a string, so it is decoded a second time here.
    fn lookup(&self, name: &str) -> Option<json::Value> {
        let list = self.entries.AsArray()?;
        for i in 0..list.Len() {
            let entry = &list[i as usize];
            let object = match entry.AsObject() {
                Some(o) => o,
                None => continue,
            };
            let (found, ok) = object.Get("name");
            if !ok {
                continue;
            }
            match found.AsString() {
                Some(s) if s == name => {}
                _ => continue,
            }
            let (raw, ok) = object.Get("value");
            if !ok {
                return None;
            }
            // An absent optional arrives as JSON null rather than being missing.
            let encoded = raw.AsString()?;
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
