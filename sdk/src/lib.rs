//! Dagger client library for Rust modules.
//!
//! This crate is `no_std` and built on [goish], which supplies the Go standard
//! library — `net/http`, `encoding/json`, `crypto/tls` — that the engine
//! session protocol needs, without libc or a garbage collector. A module built
//! against it links to a single static binary.
//!
//! It is vendored into each module as `dagger_sdk/` by `dagger generate`,
//! together with [`gen`], the API bindings generated from that module's own
//! schema. Modules never regenerate it themselves.
//!
//! [goish]: https://github.com/cogentica-ai/goish
//!
//! # Status
//!
//! Scaffold. The session transport, the GraphQL query builder and function
//! dispatch are not implemented yet — see [`serve`]. What is real today is the
//! shape: how a module is laid out, built, and started.

#![no_std]

/// API bindings generated from the module's schema.
///
/// The checked-in contents are a placeholder; `dagger generate` replaces this
/// whole module with bindings derived from the engine's introspection schema.
pub mod gen;

use goish::{os, string};

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
}

/// Serve this module: read the pending function call from the engine, dispatch
/// it, and write the result back.
///
/// Not implemented. A scaffolded module compiles and its container starts, but
/// invoking a function exits non-zero with the message below rather than
/// failing somewhere less legible.
///
/// Landing this needs three things, in order: the session transport (HTTP +
/// basic auth over the loopback port from [`Session`]), a GraphQL query builder
/// to drive `currentFunctionCall` and return values, and a registration
/// mechanism so the engine learns the module's functions — the piece with no
/// direct Go analogue, since the Go SDK recovers signatures by parsing the
/// user's package at codegen time. Rust would use proc-macros instead.
pub fn serve() -> ! {
    const MSG: &[u8] =
        b"dagger: this module was built with a scaffold of the Rust SDK; \
          function dispatch is not implemented yet\n";
    goish::syscall::Write(goish::syscall::STDERR, MSG.as_ptr(), MSG.len());
    goish::syscall::Exit(1)
}
