# dagger

The Dagger client library for Rust modules, plus the generator that produces its
API bindings.

```text
.
├── Cargo.toml          the `dagger` crate — no_std, depends on goish
├── src/lib.rs          session parameters and the module entrypoint
├── src/engine.rs       the session, and sending a selection over it
├── src/module.rs       what a module declares, and how a call reaches it
├── src/querybuilder.rs building a selection and decoding it; NOT under src/gen
├── src/objects.rs      how an object crosses the call boundary; NOT under src/gen
├── src/gen/            API bindings; replaced wholesale by `dagger generate`
├── macros/             the attribute macros that declare objects and functions
└── codegen/            no_std binary: introspection schema in, `src/gen/` out
```

This crate is never consumed from crates.io or by git ref. `dagger generate`
vendors it into each module as `dagger/`, with `src/gen/` replaced by
bindings generated from that module's own schema, and the module depends on it
by path. Modules commit the result; the runtime builds from it and never
regenerates it.

`codegen/` is `no_std` and built on goish like everything else here, so it
carries its own `.cargo/config.toml` and its binary lands under the target tuple
rather than directly in `release/` — `mod.dang` accounts for that when it copies
the binary out. goish is its only dependency, so `dagger generate` needs no
crates.io access beyond the toolchain image, and it parses the introspection
schema with goish's `encoding/json` rather than serde.

## Working on it standalone

```sh
cd sdk && cargo test
cd sdk/codegen && cargo test
cd sdk/macros && cargo test
```

The first two are `harness = false` targets handed to goish's `testing::Main`;
libtest is `std` and its `panic_impl` collides with goish's, so there are no
`#[test]` functions. A new test has to be added to the list in the target's
`main` or it never runs.

`macros/` is the exception, and the only one: it is a proc-macro crate built for
the host, so it has no goish in it and its tests are ordinary `#[test]`
functions in `src/tests.rs`. They cover the type mapping and the code emitted
for one signature. They cannot cover the parsing, because the `proc_macro` API
panics when called outside a macro expansion — the `Function` values they work
from are built by hand rather than parsed.

`sdk/`'s suite has three targets. `tests/querybuilder.rs` is the query builder
and `tests/module.rs` the values the module protocol decodes and encodes;
`tests/state.rs` is the one place in the repository where what the attribute
macros *emit* is compiled and then run, because the object it declares is
written exactly as a module's would be. Between them they cover the halves
`macros/` cannot reach from the host.

`cargo test` in `sdk/` exercises the query builder against a hand-written
stand-in for generated code. It cannot exercise the *real* bindings, because the
`src/gen/` in this repository is a placeholder. To do that, generate against a
live engine and build with the result in place:

```sh
cd sdk/codegen
dagger query --doc introspect.graphql IntrospectionQuery > /tmp/schema.json
cargo build --release
./target/x86_64-unknown-linux-gnu/release/codegen \
  --introspection /tmp/schema.json --outdir /tmp/gen
cp /tmp/gen/mod.rs ../src/gen/mod.rs && cd .. && cargo build   # then restore
```

`introspect.graphql` is committed next to the generator for exactly this. The
`--doc` flag and the trailing operation name are both required: `dagger query`
reads a bare positional argument as the operation, so passing the file that way
gets `no operation provided`.

That is worth doing for any change to what `codegen` emits: the generator's own
suite pins the text it writes, but only a real schema proves the whole surface
compiles.

## Status

See the [repository README](../README.md#status). The session protocol,
`serve()` and the generated client are all real — a module registers,
dispatches, and the bindings reach a live engine. Reaching it from inside a
function needs nothing threaded through: `dag()` opens the session the engine
left in the environment, via `default_transport()` in `src/engine.rs`.

Objects cross the call boundary too: `codegen` emits an `ObjectId` impl for
every object the schema lets it rebuild from an id, so a function can take a
`Directory` and return a `Container`. Which spelling that is depends on the
engine, and both are supported: engines through v0.21 carry a `loadXFromID` per
type, while Dagger 1.0 dropped all of them for one Relay-style
`node(id: ID!): Node` — an interface, so the chain narrows it with
`... on Directory`, which is what `Chain::field_on` renders. A loader is
preferred where one exists, so one set of bindings works against both. An argument arrives as an ID and is
rebuilt into a real client object, over the same session `dag()` opens; a
returned one is resolved to its ID, which is a round trip and so can fail.

What is left is the reverse trip: a client method's `DirectoryID` argument is a
`string` in these bindings, so handing an object back to one goes through
`ObjectId::to_id` rather than passing the object. Making it take the object
would need the query builder to resolve an ID while rendering — the chain holds
finished argument text today, and a method that extends it returns an object,
not a `Result`.
