//! What a module declares, and how a call reaches it.
//!
//! The tables here are what `#[dagger::object]` emits. They are `const`, so the
//! names and kinds are `&'static str` — that is core, not std, and costs no
//! allocation. Everything that carries a *value* at runtime is a goish type.
//!
//! [`serve`] is the entry point a module's `main` calls: it answers the
//! engine's pending call, either by describing what this module serves
//! ([`register`]) or by running one of its functions ([`dispatch`]).

use goish::encoding::json;
use goish::{nil, os, string};

use crate::engine::{field, field_string, Session};
use crate::{fail, json_string};

/// One argument of an exported function.
pub struct ArgDef {
    /// API name, camelCased from the Rust parameter.
    pub name: &'static str,
    /// The engine's TypeDefKind: `STRING_KIND`, `INTEGER_KIND`, `BOOLEAN_KIND`,
    /// `OBJECT_KIND`.
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

    let call = match session.query(&string(
        "{currentFunctionCall{name,parentName,inputArgs{name,value}}}",
    )) {
        Ok(data) => data,
        Err(message) => fail(message),
    };

    let parent_name = match field_string(&call, &["currentFunctionCall", "parentName"]) {
        Ok(name) => name,
        Err(message) => fail(message),
    };

    // An empty parentName is the engine asking what this module serves; anything
    // else is a call against that object.
    let result = if parent_name.Len() == 0 {
        register::<T>(&session)
    } else {
        dispatch::<T>(&session, &call)
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
fn register<T: Object>(session: &Session) -> Result<string, string> {
    let mut object = string("{typeDef{withObject(name:") + json_string(&string(T::NAME)) + "){";
    let mut closers = string("");

    for def in T::functions() {
        let function_id = build_function(session, def)?;
        object = object + "withFunction(function:" + json_string(&function_id) + "){";
        closers = closers + "}";
    }

    let query = object + "id}" + closers + "}}";
    let type_def = session.query(&query)?;
    let type_def_id = nested_id(&type_def, "typeDef", "withObject", T::functions().len())?;

    let module = session
        .query(&(string("{module{withObject(object:") + json_string(&type_def_id) + "){id}}}"))?;
    let module_id = field_string(&module, &["module", "withObject", "id"])?;

    // Both paths hand back "a JSON document", so the ID is JSON-encoded here to
    // match what dispatch returns from encode_*. returnValue then embeds it as a
    // GraphQL string exactly once.
    Ok(json_string(&module_id))
}

/// Build one `Function` and return its ID.
fn build_function(session: &Session, def: &FunctionDef) -> Result<string, string> {
    let return_type = build_type_def(session, def.return_kind, def.return_object, false)?;

    let mut query = string("{function(name:")
        + json_string(&string(def.name))
        + ",returnType:"
        + json_string(&return_type)
        + "){";
    let mut depth: usize = 0;

    // What makes `dagger generate` run this function. It takes no arguments:
    // the engine reads the flag off the function, then enforces the rest of the
    // contract — a `Changeset` return, and no required arguments — when the
    // module loads.
    if def.generator {
        query = query + "withGenerator{";
        depth += 1;
    }

    if !def.doc.is_empty() {
        query = query + "withDescription(description:" + json_string(&string(def.doc)) + "){";
        depth += 1;
    }

    // Flags the function for `dagger check`. It takes no arguments — the whole
    // effect is the flag.
    if def.is_check {
        query = query + "withCheck{";
        depth += 1;
    }

    for arg in def.args {
        let arg_type = build_type_def(session, arg.kind, arg.object, arg.optional)?;
        query = query
            + "withArg(name:"
            + json_string(&string(arg.name))
            + ",typeDef:"
            + json_string(&arg_type);
        if !arg.doc.is_empty() {
            query = query + ",description:" + json_string(&string(arg.doc));
        }
        if !arg.default_value.is_empty() {
            // defaultValue is a JSON scalar, so the already-encoded JSON is
            // embedded as a GraphQL string.
            query = query + ",defaultValue:" + json_string(&string(arg.default_value));
        }
        if !arg.default_path.is_empty() {
            query = query + ",defaultPath:" + json_string(&string(arg.default_path));
        }
        if !arg.ignore.is_empty() {
            query = query + ",ignore:[";
            let mut first = true;
            for pattern in arg.ignore {
                if !first {
                    query = query + ",";
                }
                query = query + json_string(&string(*pattern));
                first = false;
            }
            query = query + "]";
        }
        if !arg.deprecated.is_empty() {
            query = query + ",deprecated:" + json_string(&string(arg.deprecated));
        }
        query = query + "){";
        depth += 1;
    }

    let mut closers = string("");
    for _ in 0..depth {
        closers = closers + "}";
    }
    let data = session.query(&(query + "id}" + closers + "}"))?;
    nested_id(&data, "function", "", depth)
}

/// Build a `TypeDef` of one kind and return its ID.
///
/// `object` names the engine object for `OBJECT_KIND` and is empty otherwise;
/// an object is described by name rather than by kind, since the kind alone
/// would not say which object it is.
fn build_type_def(
    session: &Session,
    kind: &'static str,
    object: &'static str,
    optional: bool,
) -> Result<string, string> {
    // `kind` is a GraphQL enum literal, so it is spliced unquoted. It only ever
    // comes from the macro's fixed set, never from user text. An object's name
    // is a string argument, so it goes through the usual quoting.
    let (builder, head) = if object.is_empty() {
        (string("withKind(kind:") + kind + ")", "withKind")
    } else {
        (
            string("withObject(name:") + json_string(&string(object)) + ")",
            "withObject",
        )
    };

    let query = if optional {
        string("{typeDef{") + builder + "{withOptional(optional:true){id}}}}"
    } else {
        string("{typeDef{") + builder + "{id}}}"
    };
    let data = session.query(&query)?;
    if optional {
        field_string(&data, &["typeDef", head, "withOptional", "id"])
    } else {
        field_string(&data, &["typeDef", head, "id"])
    }
}

/// Walk `depth` repetitions of a chained field down to the `id` it wraps.
///
/// Chained builder calls nest in the response exactly as they do in the query,
/// so `withFunction(...){withFunction(...){id}}` comes back doubly wrapped.
fn nested_id(
    data: &json::Value,
    root: &'static str,
    repeated: &'static str,
    depth: usize,
) -> Result<string, string> {
    let mut current = if root.is_empty() {
        data.clone()
    } else {
        field(data, &[root])?
    };
    if !repeated.is_empty() {
        current = field(&current, &[repeated])?;
    }
    for _ in 0..depth {
        current = match current.AsObject() {
            Some(object) => {
                let mut found = json::Value::Null;
                // Each level has exactly one field, whatever it was named.
                let keys = object.Keys();
                for i in 0..keys.Len() {
                    let (value, ok) = object.Get(keys[i as usize].clone());
                    if ok {
                        found = value;
                    }
                }
                found
            }
            None => return Err(string("unexpected shape in the engine response")),
        };
    }
    match current.AsObject() {
        Some(object) => {
            let (id, ok) = object.Get("id");
            if !ok {
                return Err(string("engine response is missing id"));
            }
            match id.AsString() {
                Some(s) => Ok(s.clone()),
                None => Err(string("id was not a string")),
            }
        }
        None => match current.AsString() {
            Some(s) => Ok(s.clone()),
            None => Err(string("engine response is missing id")),
        },
    }
}

/// Call the requested function and return its JSON-encoded result.
fn dispatch<T: Object>(session: &Session, call: &json::Value) -> Result<string, string> {
    let _ = session;
    let name = field_string(call, &["currentFunctionCall", "name"])?;
    let input_args = field(call, &["currentFunctionCall", "inputArgs"])?;
    let args = Arguments::new(input_args);
    T::invoke(&name, &args)
}

/// Hand a JSON-encoded result back to the engine.
fn return_value(session: &Session, value: &string) -> Result<(), string> {
    session.query(
        &(string("{currentFunctionCall{returnValue(value:") + json_string(value) + ")}}"),
    )?;
    Ok(())
}
