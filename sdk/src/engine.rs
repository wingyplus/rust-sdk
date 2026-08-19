//! Talking to the Dagger Engine.
//!
//! The engine starts a module binary with the session's port and token in the
//! environment and listens for GraphQL on loopback. [`Session`] is that
//! connection; [`fetch`] is how a selection built by
//! [`querybuilder`](crate::querybuilder) crosses it.

use alloc::sync::Arc;

use goish::encoding::base64;
use goish::encoding::json;
use goish::net::http;
use goish::{bytes, io, nil, os, string};

use crate::querybuilder::{json_string, Chain, Sel};

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
    /// reaches [`crate::fail`] with the same shape.
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

/// Something that can answer a GraphQL document.
///
/// [`Session`] is the real one. The trait exists so that [`fetch`] can be
/// tested against a canned reply, and so a caller with its own client — a
/// recording proxy, a retrying wrapper — can supply it without this crate
/// growing a policy about either.
///
/// Deliberately object-safe: `&dyn Transport` works, since the generic lives on
/// [`fetch`] rather than here.
pub trait Transport {
    /// Send one GraphQL document and return its `data` object.
    ///
    /// The error is a message rather than a goish `error` for the reason
    /// [`Session::query`] gives: every failure — transport, HTTP status,
    /// malformed body, GraphQL `errors` — reaches the caller with one shape.
    fn query(&self, document: &string) -> Result<json::Value, string>;
}

impl Transport for Session {
    fn query(&self, document: &string) -> Result<json::Value, string> {
        Session::query(self, document)
    }
}

/// The transport for the session this process was started in.
///
/// This is what the generated `dag()` is built on: the engine puts the session
/// in the environment before it starts a module, so the common case needs no
/// argument, and the client can be reached from anywhere without one being
/// threaded through.
///
/// A missing session is not recoverable — the process was not started by the
/// engine, so there is nothing to talk to — so this goes to [`fail`] rather
/// than returning an error, the way the Go SDK's package-level `dag` panics in
/// `init`. Use [`Session::from_env`] directly to handle the absence yourself.
///
/// A new [`Session`] each call, deliberately: it is two environment lookups and
/// holds no connection — [`Session::query`] opens one per request — so there is
/// nothing to keep alive, and nothing to synchronise in a `no_std` global.
///
/// [`fail`]: crate::fail
pub fn default_transport() -> Arc<dyn Transport> {
    match Session::from_env() {
        Some(session) => Arc::new(session),
        None => crate::fail(
            string("no engine session: ") + SESSION_PORT_ENV + " and " + SESSION_TOKEN_ENV
                + " are not set. Is this running as a Dagger module?",
        ),
    }
}

/// Render `sel` at the end of `chain`, send it, and decode the reply. One
/// round trip.
///
/// `?Sized` so this takes `&dyn Transport` as readily as a concrete session:
/// the monomorphisation that matters is over `S`, which is where the
/// zero-overhead decode comes from.
pub fn fetch<T: Transport + ?Sized, S: Sel>(
    transport: &T,
    chain: &Chain,
    sel: &S,
) -> Result<S::Out, string> {
    let data = transport.query(&chain.render(sel))?;
    chain.decode(&data, sel)
}
