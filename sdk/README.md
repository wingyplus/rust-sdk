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
├── src/objects.rs      the engine objects a module can name; NOT under src/gen
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
```

Both suites are `harness = false` targets handed to goish's `testing::Main`;
libtest is `std` and its `panic_impl` collides with goish's, so there are no
`#[test]` functions. A new test has to be added to the list in the target's
`main` or it never runs.

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
left in the environment, via `default_transport()` in `src/engine.rs`. What is
missing is the seam in the other direction — `ObjectId` still wraps a bare ID
with no transport behind it, so function signatures are limited to scalars and
the ID wrappers in `src/objects.rs`.
