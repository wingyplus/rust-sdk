//! Dagger client library for Rust modules.
//!
//! This crate is `no_std` and built on [goish], which supplies the Go standard
//! library — `net/http`, `encoding/json`, `encoding/base64` — that the engine
//! session protocol needs, without libc or a garbage collector. A module built
//! against it links to a single static binary.
//!
//! It is vendored into each module as `dagger_sdk/` by `dagger generate`,
//! together with [`gen`], the API bindings generated from that module's own
//! schema. Modules never regenerate it themselves.
//!
//! [goish]: https://github.com/cogentica-ai/goish
//!
//! # The module protocol
//!
//! The engine starts a module binary once per call, with the session's port and
//! token in the environment, and expects it to talk GraphQL back over loopback:
//!
//! 1. Ask for `currentFunctionCall { parentName }`.
//! 2. When `parentName` is empty this is the *registration* call: build a
//!    `Module` describing what the module serves and return its ID.
//! 3. Otherwise it is an invocation: dispatch to the named function.
//! 4. Either way, hand the result back through `returnValue`.
//!
//! # Status
//!
//! The protocol above runs end to end: [`serve`] registers what the module
//! serves — its functions and their arguments, which of them are checks, and
//! which are generators — and dispatches an incoming call. What is missing is
//! the other direction, a module calling the engine API back: the generated
//! bindings are still a placeholder, so function types are limited to scalars
//! and the objects in [`objects`], and [`Session::query`] is the only way to
//! reach the engine. See the repository README.

#![no_std]

/// API bindings generated from the module's schema.
///
/// The checked-in contents are a placeholder; `dagger generate` replaces this
/// whole module with bindings derived from the engine's introspection schema.
pub mod gen;

mod module;
mod objects;

pub use module::{
    encode_bool, encode_int, encode_object, encode_string, encode_void, ArgDef, Arguments,
    FunctionDef, Object,
};
pub use objects::{Changeset, ObjectId, Workspace};

/// Declare a module's root object and the functions it serves.
pub use dagger_sdk_macros::{check, function, object};

use goish::encoding::base64;
use goish::encoding::json;
use goish::net::http;
use goish::{bytes, io, nil, os, string};

/// Environment variable naming the port the engine session listens on.
pub const SESSION_PORT_ENV: &str = "DAGGER_SESSION_PORT";

/// Environment variable holding the session's bearer token.
pub const SESSION_TOKEN_ENV: &str = "DAGGER_SESSION_TOKEN";

/// How to reach the engine session serving this module.
///
/// The engine starts every module process with these two variables set. The
/// GraphQL endpoint is `http://127.0.0.1:<port>/query`, authenticated with the
/// token as the HTTP basic-auth username and an empty password.
pub struct Session {
    /// Loopback port the session listens on.
    pub port: string,
    /// Bearer token for the session.
    pub token: string,
}

impl Session {
    /// Read the session parameters the engine placed in the environment.
    ///
    /// Returns `None` when either variable is unset — which means the process
    /// was not started by the engine, so there is no session to talk to.
    pub fn from_env() -> Option<Session> {
        let (port, port_ok) = os::LookupEnv(string(SESSION_PORT_ENV));
        if !port_ok {
            return None;
        }
        let (token, token_ok) = os::LookupEnv(string(SESSION_TOKEN_ENV));
        if !token_ok {
            return None;
        }
        Some(Session { port, token })
    }

    /// The session's GraphQL endpoint.
    pub fn url(&self) -> string {
        string("http://127.0.0.1:") + self.port.clone() + "/query"
    }

    /// The `Authorization` header value: the token as the basic-auth username
    /// with an empty password.
    fn authorization(&self) -> string {
        let raw = self.token.clone() + ":";
        string("Basic ") + base64::StdEncoding.EncodeToString(&bytes(raw))
    }

    /// Send one GraphQL query and return its `data` object.
    ///
    /// Errors come back as a message rather than a goish `error` so every
    /// failure — transport, HTTP status, malformed body, GraphQL `errors` —
    /// reaches [`fail`] with the same shape.
    pub fn query(&self, query: &string) -> Result<json::Value, string> {
        let body = string("{\"query\":") + json_string(query) + "}";

        let (mut req, err) = http::NewRequest(string("POST"), self.url(), bytes(body));
        if err != nil {
            return Err(string("building request: ") + err.Error());
        }
        req.Header.Set(string("Content-Type"), string("application/json"));
        req.Header.Set(string("Authorization"), self.authorization());

        let (resp, err) = http::Client::default().Do(&req);
        if err != nil {
            return Err(string("querying the engine session: ") + err.Error());
        }

        let (raw, err) = io::ReadAll(&mut resp.Body.clone());
        if err != nil {
            return Err(string("reading the response: ") + err.Error());
        }

        let mut parsed = json::Value::Null;
        let err = json::Unmarshal(&raw, &mut parsed);
        if err != nil {
            return Err(string("decoding the response: ") + err.Error());
        }

        let obj = match parsed.AsObject() {
            Some(o) => o,
            None => return Err(string("engine response was not a JSON object")),
        };

        // GraphQL reports failures in `errors` with a 200, so the status alone
        // is not enough to tell success from failure.
        let (errors, has_errors) = obj.Get("errors");
        if has_errors && !errors.IsNull() {
            return Err(string("engine returned an error: ") + string(raw));
        }
        if resp.StatusCode != 200 {
            return Err(string("engine returned HTTP status ") + string(raw));
        }

        let (data, has_data) = obj.Get("data");
        if !has_data {
            return Err(string("engine response had no data"));
        }
        Ok(data)
    }
}

/// Follow a chain of object keys into a JSON value.
///
/// Every response this module reads is a nested single-field object shaped like
/// the query that produced it, so one walker covers all of them — including the
/// queries a module writes by hand while the bindings are a placeholder.
pub fn field(value: &json::Value, path: &[&'static str]) -> Result<json::Value, string> {
    let mut current = value.clone();
    for key in path {
        let obj = match current.AsObject() {
            Some(o) => o,
            None => return Err(string("expected an object at ") + string(*key)),
        };
        let (next, ok) = obj.Get(*key);
        if !ok {
            return Err(string("engine response is missing ") + string(*key));
        }
        current = next;
    }
    Ok(current)
}

/// Follow [`field`] and require the result to be a string.
///
/// Most of what a query is asked for is an ID, so this is the accessor a
/// hand-written query reaches for.
pub fn field_string(value: &json::Value, path: &[&'static str]) -> Result<string, string> {
    let found = field(value, path)?;
    match found.AsString() {
        Some(s) => Ok(s.clone()),
        None => Err(string("expected a string in the engine response")),
    }
}

/// Render a Rust-side string as a quoted, escaped literal.
///
/// Used for both the GraphQL arguments and the JSON request body, which share
/// escaping rules. Going through `json::Marshal` rather than hand-rolling the
/// escapes keeps module IDs — opaque, engine-chosen text — safe to embed. It is
/// public for the same reason [`field`] is: a query written by hand has to
/// quote its arguments, and this is what the rest of the crate quotes with.
pub fn json_string(value: &string) -> string {
    let (encoded, err) = json::Marshal(&json::Value::String(value.clone()));
    if err != nil {
        fail(string("encoding a query argument: ") + err.Error());
    }
    string(encoded)
}

/// Write a message to stderr and exit non-zero.
///
/// The engine surfaces a module's stderr, so this is what a user sees when a
/// module fails to serve.
pub fn fail(message: string) -> ! {
    let stderr = os::Stderr();
    let _ = stderr.Write(bytes(string("dagger: ") + message + "\n"));
    os::Exit(2)
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
