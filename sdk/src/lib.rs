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
//! Registration works, so a module loads and serves its API. Step 3 does not:
//! there is no way yet to declare functions, so [`serve`] registers an object
//! with no functions and refuses any invocation. See the repository README.

#![no_std]

/// API bindings generated from the module's schema.
///
/// The checked-in contents are a placeholder; `dagger generate` replaces this
/// whole module with bindings derived from the engine's introspection schema.
pub mod gen;

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
/// the query that produced it, so one walker covers all of them.
fn field(value: &json::Value, path: &[&'static str]) -> Result<json::Value, string> {
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
fn field_string(value: &json::Value, path: &[&'static str]) -> Result<string, string> {
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
/// escapes keeps module IDs — opaque, engine-chosen text — safe to embed.
fn json_string(value: &string) -> string {
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
/// `object_name` is the module's root object — the PascalCase form of the
/// Dagger module name, which is what the engine expects to find.
///
/// Registration is implemented, so the module loads and `dagger api functions`
/// succeeds against it. Invocation is not: nothing can declare a function yet,
/// so a call to one exits with a message rather than hanging or returning a
/// wrong answer.
pub fn serve(object_name: &'static str) -> ! {
    let session = match Session::from_env() {
        Some(s) => s,
        None => fail(string(
            "no engine session in the environment; a module binary is started by the engine, not run directly",
        )),
    };

    let parent_name = match session
        .query(&string("{currentFunctionCall{parentName}}"))
        .and_then(|data| field_string(&data, &["currentFunctionCall", "parentName"]))
    {
        Ok(name) => name,
        Err(message) => fail(message),
    };

    if parent_name.Len() != 0 {
        fail(string("this module serves no functions yet, but the engine asked for one on ") + parent_name);
    }

    match register(&session, object_name) {
        Ok(()) => os::Exit(0),
        Err(message) => fail(message),
    }
}

/// Describe the module to the engine and return the description's ID.
///
/// The shape mirrors every other SDK: build a `TypeDef` for the root object,
/// attach it to a `Module`, and hand back the module's ID as the call's return
/// value. The object carries no functions yet.
fn register(session: &Session, object_name: &'static str) -> Result<(), string> {
    let name = string(object_name);

    let type_def = session.query(
        &(string("{typeDef{withObject(name:") + json_string(&name) + "){id}}}"),
    )?;
    let type_def_id = field_string(&type_def, &["typeDef", "withObject", "id"])?;

    let module = session.query(
        &(string("{module{withObject(object:") + json_string(&type_def_id) + "){id}}}"),
    )?;
    let module_id = field_string(&module, &["module", "withObject", "id"])?;

    // returnValue takes a JSON scalar, so the ID is JSON-encoded and that
    // encoding is then embedded as a GraphQL string — hence the double pass.
    let encoded = json_string(&module_id);
    session.query(
        &(string("{currentFunctionCall{returnValue(value:") + json_string(&encoded) + ")}}"),
    )?;

    Ok(())
}
